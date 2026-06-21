#!/usr/bin/env bash
# Coop v0.1 end-to-end test.
#
# Usage:
#   scripts/e2e.sh              # mock mode  (no Anthropic key required)
#   scripts/e2e.sh live         # live mode  (requires ANTHROPIC_API_KEY)
#
# Verifies:
#   1.  Cold boot              -> /farm returns 0 hens
#   2.  Vault init+put         -> file appears; status=locked
#   3.  Vault unlock           -> /vault/status.unlocked = true
#   4.  Hen create             -> POST yaml returns id
#   5.  Hen hatch              -> state transitions to IDLE
#   6.  WSS /watch             -> events captured into log
#   7.  Job submission         -> (mock) FAILED w/ auth-ish error  OR
#                                (live) DONE with non-empty result
#   8.  Hen state restored     -> hen back to IDLE after job
#   9.  Reconciler             -> kill -9 + restart leaves no stuck state
#   10. CLI parity            -> `coop job list` enumerates jobs
#   12. PTY shell             -> WSS /shell streams stdin/stdout w/ hen env
#                                and serves the farm UI at /
#   9.  Reconciler            -> kill -9 + restart leaves no stuck state
#   11. Graceful shutdown     -> SIGTERM exits 0
#
# Verbose coopd log: /tmp/coopd-e2e.log    WSS log: /tmp/coopd-e2e-wss.log

set -euo pipefail
trap 'rc=$?; teardown; exit $rc' EXIT INT TERM

MODE="${1:-mock}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${COOP_E2E_PORT:-9799}"
API="http://127.0.0.1:${PORT}"
DATA_DIR="$(mktemp -d -t coop-e2e-XXXXXX)"
VAULT_PATH="${DATA_DIR}/vault.json"
PASSPHRASE="e2e-passphrase-do-not-use"
LOG="/tmp/coopd-e2e.log"
WSS_LOG="/tmp/coopd-e2e-wss.log"
COOPD_PID=""
WSS_PID=""

b() { printf "\033[1m%s\033[0m\n" "$*"; }
g() { printf "  \033[32m✓\033[0m %s\n" "$*"; }
r() { printf "  \033[31m✗\033[0m %s\n" "$*" >&2; }
y() { printf "  \033[33m·\033[0m %s\n" "$*"; }

teardown() {
  set +e
  [[ -n "${WSS_PID}"   ]] && kill "${WSS_PID}"   2>/dev/null
  [[ -n "${COOPD_PID}" ]] && kill "${COOPD_PID}" 2>/dev/null
  wait 2>/dev/null
  if [[ "${COOP_E2E_KEEP:-0}" != "1" ]]; then
    rm -rf "$DATA_DIR"
  else
    y "kept DATA_DIR=$DATA_DIR (COOP_E2E_KEEP=1)"
  fi
}

PY="$(command -v python3 || command -v python)"
[[ -n "$PY" ]] || { r "python3 required"; exit 1; }
command -v curl >/dev/null || { r "curl required"; exit 1; }

j_get() {
  printf '%s' "$1" | "$PY" -c "
import sys, json
d = json.load(sys.stdin)
for k in '$2'.split('.'):
    if isinstance(d, list): d = d[int(k)]
    elif isinstance(d, dict): d = d.get(k)
    else: d = None
    if d is None: break
print('' if d is None else (d if isinstance(d, str) else json.dumps(d)))
"
}

wait_http() {
  local url="$1" tries="${2:-40}"
  for _ in $(seq 1 "$tries"); do
    curl -fsS "$url" >/dev/null 2>&1 && return 0
    sleep 0.25
  done
  return 1
}
# NOTE: wait_http is retained as a generic helper; start_coopd intentionally
# uses its own liveness-aware loop (see below).

start_coopd() {
  : > "$LOG"
  # Fail fast if something is already serving on our port (e.g. a stale coopd
  # left over from an interrupted run). Without this guard, our own coopd
  # fails to bind, we'd silently connect to the *other* server, and the
  # suite reports confusing mismatches (e.g. "expected 0 hens, got 2").
  if curl -fsS "$API/api/v1/farm" >/dev/null 2>&1; then
    r "port ${PORT} is already serving an HTTP API before coopd started — a stale coopd? Kill it, or set COOP_E2E_PORT to a free port."
    return 1
  fi
  ( cd "$ROOT" && \
    exec env COOP_VAULT="" COOP_PASSPHRASE="" \
      ./target/debug/coopd --data-dir "$DATA_DIR" --log info \
      serve --addr "127.0.0.1:${PORT}" >> "$LOG" 2>&1 ) &
  COOPD_PID=$!
  # Wait for *our* coopd, bailing out immediately if the process we launched
  # dies during startup (e.g. a bind failure) instead of waiting on a server
  # that will never be ours.
  for _ in $(seq 1 80); do
    if ! kill -0 "$COOPD_PID" 2>/dev/null; then
      r "coopd (pid ${COOPD_PID}) exited during startup; tail -50 $LOG:"
      tail -50 "$LOG" >&2
      return 1
    fi
    curl -fsS "$API/api/v1/farm" >/dev/null 2>&1 && return 0
    sleep 0.25
  done
  r "coopd failed to start; tail -50 $LOG:"
  tail -50 "$LOG" >&2
  return 1
}

stop_coopd_graceful() {
  if [[ -n "$COOPD_PID" ]] && kill -0 "$COOPD_PID" 2>/dev/null; then
    kill -TERM "$COOPD_PID"
    local n=0
    while kill -0 "$COOPD_PID" 2>/dev/null && (( n < 40 )); do sleep 0.1; n=$((n+1)); done
    if kill -0 "$COOPD_PID" 2>/dev/null; then
      r "coopd did not exit on SIGTERM; killing"; kill -9 "$COOPD_PID"; return 1
    fi
  fi
  COOPD_PID=""
}

stop_coopd_kill9() {
  if [[ -n "$COOPD_PID" ]] && kill -0 "$COOPD_PID" 2>/dev/null; then
    kill -9 "$COOPD_PID" || true
    wait "$COOPD_PID" 2>/dev/null || true
  fi
  COOPD_PID=""
}

b "== coop v0.1 E2E (${MODE} mode) =="
[[ -x "$ROOT/target/debug/coopd" && -x "$ROOT/target/debug/coop" ]] || {
  y "building debug binaries..."
  ( cd "$ROOT" && cargo build -q --workspace ) || { r "cargo build failed"; exit 1; }
}
g "binaries present"

# 1. cold boot
b "[1] cold boot"
start_coopd
n=$(j_get "$(curl -fsS "$API/api/v1/farm")" "hen_count")
[[ "$n" == "0" ]] && g "fresh farm has 0 hens" || { r "expected 0, got $n"; exit 1; }

# 2. vault init + put
b "[2] vault init + put"
COOP_PASSPHRASE="$PASSPHRASE" \
  "$ROOT/target/debug/coop" --api "$API" vault init "$VAULT_PATH" >/dev/null
[[ -f "$VAULT_PATH" ]] && g "vault file created" || { r "vault file missing"; exit 1; }

SECRET_VALUE="${ANTHROPIC_API_KEY:-sk-ant-e2e-mock-not-a-real-key}"
COOP_PASSPHRASE="$PASSPHRASE" COOP_SECRET_VALUE="$SECRET_VALUE" \
  "$ROOT/target/debug/coop" --api "$API" vault put \
    "$VAULT_PATH" byok-anthropic >/dev/null
g "stored byok-anthropic secret"

status_before=$(j_get "$(curl -fsS "$API/api/v1/vault/status")" "unlocked")
[[ "$status_before" == "false" ]] && g "vault locked pre-unlock" || { r "expected locked"; exit 1; }

# 3. vault unlock
b "[3] vault unlock"
unlock_body=$(curl -fsS -X POST "$API/api/v1/vault/unlock" \
  -H 'content-type: application/json' \
  -d "{\"path\":\"$VAULT_PATH\",\"passphrase\":\"$PASSPHRASE\"}")
[[ "$(j_get "$unlock_body" ok)" == "true" ]] && g "unlock OK" || { r "unlock failed: $unlock_body"; exit 1; }
status_after=$(j_get "$(curl -fsS "$API/api/v1/vault/status")" "unlocked")
[[ "$status_after" == "true" ]] && g "vault unlocked" || { r "expected unlocked"; exit 1; }

# 6. WSS subscriber
b "[6] WSS /watch (start subscriber)"
: > "$WSS_LOG"
WSS_READY="${WSS_LOG}.ready"
rm -f "$WSS_READY"
# Pre-install the websockets package synchronously so the backgrounded subscriber
# never races a slow pip install on CI runners (the original cause of the
# flaky "missing hen_created in WSS log" failure).
"$PY" -c "import websockets" 2>/dev/null || \
  "$PY" -m pip install -q websockets 2>/dev/null || \
  "$PY" -m pip install -q --break-system-packages websockets
"$PY" - "$PORT" "$WSS_LOG" "$WSS_READY" <<'PY' &
import asyncio, sys, pathlib
port, log_path, ready_path = sys.argv[1], sys.argv[2], sys.argv[3]
import websockets
async def main():
    url = f"ws://127.0.0.1:{port}/api/v1/watch"
    async with websockets.connect(url) as ws:
        pathlib.Path(ready_path).touch()
        with open(log_path, "a", buffering=1) as f:
            try:
                while True:
                    msg = await ws.recv()
                    f.write(msg + "\n")
            except Exception:
                pass
asyncio.run(main())
PY
WSS_PID=$!
# Wait up to 10s for the subscriber to actually establish the WS connection.
for _ in $(seq 1 40); do
  [[ -f "$WSS_READY" ]] && break
  sleep 0.25
done
[[ -f "$WSS_READY" ]] && g "WSS subscriber attached" || { r "WSS subscriber failed to connect"; kill "$WSS_PID" 2>/dev/null; exit 1; }

# 4. create hen
b "[4] create hen"
hen_yaml=$'spec_version: coop/v1\nname: aria\nbrain:\n  provider_id: vault:byok-anthropic\n  model: claude-sonnet-4-5-20250929\ntools: [bash, file_read, file_write]\n'
create_body=$(curl -fsS -X POST "$API/api/v1/hens" \
  -H 'content-type: application/yaml' \
  --data-binary "$hen_yaml")
[[ "$create_body" == '"local.coop/aria"' ]] && g "hen created" || { r "create unexpected: $create_body"; exit 1; }

# 5. hatch
b "[5] hatch hen"
curl -fsS -X POST "$API/api/v1/hens/local.coop%2Faria/hatch" >/dev/null
sleep 0.3
state=$(j_get "$(curl -fsS "$API/api/v1/hens/local.coop%2Faria")" "state")
[[ "$state" == "IDLE" ]] && g "hen reached IDLE" || { r "expected IDLE got $state"; exit 1; }

# 7. submit job
b "[7] submit job (${MODE})"
if [[ "$MODE" == "live" ]]; then
  prompt="Reply with the exact text END_OF_TEST and nothing else."
else
  prompt="hello from e2e"
fi
prompt_json=$("$PY" -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$prompt")
sub=$(curl -fsS -X POST "$API/api/v1/hens/local.coop%2Faria/jobs" \
  -H 'content-type: application/json' \
  -d "$(printf '{"prompt":%s}' "$prompt_json")")
job_id=$(j_get "$sub" job_id)
[[ -n "$job_id" ]] && g "submitted job $job_id" || { r "submit failed: $sub"; exit 1; }

deadline=$(( $(date +%s) + 120 ))
final=""
while (( $(date +%s) < deadline )); do
  body=$(curl -fsS "$API/api/v1/jobs/$job_id")
  s=$(j_get "$body" status)
  case "$s" in
    DONE|FAILED|CANCELLED) final="$s"; break ;;
  esac
  sleep 0.4
done
job_body=$(curl -fsS "$API/api/v1/jobs/$job_id")
case "$MODE:$final" in
  mock:FAILED)
    err=$(j_get "$job_body" error)
    g "mock job FAILED as expected ($err)"
    ;;
  live:DONE)
    result=$(j_get "$job_body" result)
    [[ -n "$result" ]] && g "live job DONE; result=${result:0:80}..." || { r "DONE but empty result"; exit 1; }
    ;;
  *)
    r "unexpected final='$final' mode=$MODE"
    echo "$job_body" | "$PY" -m json.tool >&2
    exit 1
    ;;
esac

# 8. hen state restored
b "[8] hen state restored to IDLE"
state=$(j_get "$(curl -fsS "$API/api/v1/hens/local.coop%2Faria")" state)
[[ "$state" == "IDLE" ]] && g "hen IDLE post-job" || { r "expected IDLE got $state"; exit 1; }

# 8b. persistent memory recorded
b "[8b] persistent memory"
mem=$(curl -fsS "$API/api/v1/hens/local.coop%2Faria/memory")
mem_len=$("$PY" -c 'import json,sys; print(len(json.loads(sys.argv[1])))' "$mem")
[[ "$mem_len" -ge 1 ]] && g "recorded $mem_len episode(s)" || { r "no memory recorded: $mem"; exit 1; }
last=$(( mem_len - 1 ))
mem_outcome=$(j_get "$mem" "${last}.outcome")
mem_job=$(j_get "$mem" "${last}.job_id")
case "$MODE" in
  mock) [[ "$mem_outcome" == "failed" ]] && g "episode outcome=failed (mock)" || { r "expected failed, got $mem_outcome"; exit 1; } ;;
  live) [[ "$mem_outcome" == "done"   ]] && g "episode outcome=done (live)"   || { r "expected done, got $mem_outcome"; exit 1; } ;;
esac
[[ "$mem_job" == "$job_id" ]] && g "episode linked to job $job_id" || { r "episode job_id mismatch: $mem_job"; exit 1; }
mem1=$(curl -fsS "$API/api/v1/hens/local.coop%2Faria/memory?limit=1")
mem1_len=$("$PY" -c 'import json,sys; print(len(json.loads(sys.argv[1])))' "$mem1")
[[ "$mem1_len" == "1" ]] && g "?limit=1 returns 1 episode" || { r "limit failed: got $mem1_len"; exit 1; }

# 6b. WSS event check
b "[6b] WSS event check"
sleep 0.5
for evt in hen_created hen_state_changed job_submitted job_status_changed memory_recorded; do
  if grep -q "\"type\":\"$evt\"" "$WSS_LOG"; then
    g "saw $evt"
  else
    r "missing $evt in WSS log"
    cat "$WSS_LOG" >&2
    exit 1
  fi
done

# 10. CLI parity
b "[10] CLI parity"
lst=$("$ROOT/target/debug/coop" --api "$API" job list)
echo "$lst" | grep -q "$job_id" && g "coop job list shows $job_id" || { r "job missing"; echo "$lst" >&2; exit 1; }

# 10b. CLI memory + forget
b "[10b] CLI memory + forget"
cli_mem=$("$ROOT/target/debug/coop" --api "$API" hen memory local.coop/aria)
echo "$cli_mem" | grep -q "$job_id" && g "coop hen memory shows episode" || { r "memory missing job"; echo "$cli_mem" >&2; exit 1; }
cli_forget=$("$ROOT/target/debug/coop" --api "$API" hen forget local.coop/aria)
echo "$cli_forget" | grep -q '"forgotten"' && g "coop hen forget reports count" || { r "forget output unexpected"; echo "$cli_forget" >&2; exit 1; }
mem_after=$(curl -fsS "$API/api/v1/hens/local.coop%2Faria/memory")
mem_after_len=$("$PY" -c 'import json,sys; print(len(json.loads(sys.argv[1])))' "$mem_after")
[[ "$mem_after_len" == "0" ]] && g "memory empty after forget" || { r "expected 0 after forget, got $mem_after_len"; exit 1; }

# 10c. in-farm delegation (manager hen dispatches to a worker hen)
b "[10c] in-farm delegation"
scout_yaml=$'spec_version: coop/v1\nname: scout\nbrain:\n  provider_id: vault:byok-anthropic\n  model: claude-sonnet-4-5-20250929\ntools: [bash, file_read]\n'
curl -fsS -X POST "$API/api/v1/hens" -H 'content-type: application/yaml' --data-binary "$scout_yaml" >/dev/null
curl -fsS -X POST "$API/api/v1/hens/local.coop%2Fscout/hatch" >/dev/null
del_resp=$(curl -fsS --max-time 90 -X POST "$API/api/v1/hens/local.coop%2Faria/delegate" \
  -H 'content-type: application/json' \
  -d '{"to":"local.coop/scout","prompt":"recon subtask from e2e"}')
del_job=$(j_get "$del_resp" job_id)
del_status=$(j_get "$del_resp" status)
[[ -n "$del_job" ]] && g "delegation produced sub-job $del_job" || { r "no sub-job: $del_resp"; exit 1; }
case "$MODE" in
  mock) [[ "$del_status" == "Failed" ]] && g "sub-job FAILED in mock (no key)" || { r "expected Failed, got $del_status"; exit 1; } ;;
  live) [[ "$del_status" == "Done"   ]] && g "sub-job DONE (live)"            || { r "expected Done, got $del_status"; exit 1; } ;;
esac
# the sub-job must be owned by the worker hen, not the manager
scout_jobs=$(curl -fsS "$API/api/v1/jobs?hen_id=local.coop%2Fscout")
echo "$scout_jobs" | grep -q "$del_job" && g "sub-job owned by worker scout" || { r "sub-job not owned by scout"; echo "$scout_jobs" >&2; exit 1; }
# self-delegation must be rejected with HTTP 400
self_code=$(curl -sS -o /dev/null -w '%{http_code}' -X POST "$API/api/v1/hens/local.coop%2Faria/delegate" \
  -H 'content-type: application/json' \
  -d '{"to":"local.coop/aria","prompt":"loop"}')
[[ "$self_code" == "400" ]] && g "self-delegation rejected (400)" || { r "expected 400, got $self_code"; exit 1; }
# the orchestrator must have emitted a `delegated` audit event
sleep 0.5
grep -q '"type":"delegated"' "$WSS_LOG" && g "saw delegated event" || { r "missing delegated in WSS log"; cat "$WSS_LOG" >&2; exit 1; }

# 12. PTY shell over WSS  (run before kill-9 step)
b "[12] PTY shell over WSS"
shell_log="/tmp/coopd-e2e-shell.log"
: > "$shell_log"
"$PY" - "$PORT" "$shell_log" <<'PY'
import asyncio, json, sys
port, log_path = sys.argv[1], sys.argv[2]
try:
    import websockets
except ImportError:
    import subprocess
    subprocess.check_call([sys.executable, "-m", "pip", "install", "-q", "websockets"])
    import websockets

async def main():
    url = f"ws://127.0.0.1:{port}/api/v1/hens/local.coop%2Faria/shell"
    async with websockets.connect(url) as ws:
        await ws.send(json.dumps({"type":"resize","cols":120,"rows":40}))
        await ws.send(b"echo COOP_E2E_SHELL_OK COOP_HEN_ID=$COOP_HEN_ID\n")
        buf = bytearray()
        try:
            while b"COOP_HEN_ID=local.coop/aria" not in buf:
                m = await asyncio.wait_for(ws.recv(), timeout=4)
                if isinstance(m, bytes): buf.extend(m)
        except (asyncio.TimeoutError, Exception):
            pass
        open(log_path,"wb").write(buf)
asyncio.run(main())
PY
grep -q "COOP_E2E_SHELL_OK" "$shell_log" && g "shell echoed token"   || { r "shell token missing"; cat "$shell_log" >&2; exit 1; }
grep -q "COOP_HEN_ID=local.coop/aria" "$shell_log" && g "COOP_HEN_ID env exported" || { r "COOP_HEN_ID missing"; exit 1; }

caps=$(curl -fsS "$API/api/v1/session/capabilities")
persistent=$("$PY" -c 'import json,sys; print(json.loads(sys.argv[1])["persistent_session"])' "$caps")
if [[ "$persistent" == "True" || "$persistent" == "true" ]]; then
  b "[12b] persistent tmux reconnect + send-key convergence"
  curl -fsS -X POST "$API/api/v1/hens/local.coop%2Faria/shell/send" \
    -H 'content-type: application/json' \
    -d '{"keys":"echo COOP_E2E_SEND_KEYS_OK"}' >/dev/null
  "$PY" - "$PORT" "$shell_log" <<'PY'
import asyncio, json, sys
port, log_path = sys.argv[1], sys.argv[2]
import websockets

async def main():
    url = f"ws://127.0.0.1:{port}/api/v1/hens/local.coop%2Faria/shell"
    async with websockets.connect(url) as ws:
        await ws.send(json.dumps({"type":"resize","cols":120,"rows":40}))
        buf = bytearray()
        try:
            while b"COOP_E2E_SEND_KEYS_OK" not in buf:
                m = await asyncio.wait_for(ws.recv(), timeout=4)
                if isinstance(m, bytes): buf.extend(m)
        except (asyncio.TimeoutError, Exception):
            pass
        await ws.send(b"exit\n")
        with open(log_path, "ab") as f:
            f.write(buf)
asyncio.run(main())
PY
  grep -q "COOP_E2E_SEND_KEYS_OK" "$shell_log" && g "shell/send reaches reattached tmux session" || { r "persistent marker missing"; cat "$shell_log" >&2; exit 1; }
else
  y "persistent tmux reconnect skipped ($(j_get "$caps" note))"
fi

# Verify static UI is served.
ui_head=$(curl -fsS "$API/" | head -1 || true)
[[ "$ui_head" == "<!doctype html>" ]] && g "farm UI served at /" || { r "UI not served; got: $ui_head"; exit 1; }

# 9. reconciler
if [[ "$MODE" == "mock" ]]; then
  b "[9] reconciler test (kill-9, restart)"
  hen2_yaml=$'spec_version: coop/v1\nname: bolt\nbrain:\n  provider_id: vault:byok-anthropic\n  model: m\n'
  curl -fsS -X POST "$API/api/v1/hens" -H 'content-type: application/yaml' --data-binary "$hen2_yaml" >/dev/null
  curl -fsS -X POST "$API/api/v1/hens/local.coop%2Fbolt/hatch" >/dev/null
  stop_coopd_kill9
  g "killed coopd -9"
  start_coopd
  jobs_json=$(curl -fsS "$API/api/v1/jobs")
  non_terminal=$("$PY" -c '
import json, sys
d = json.loads(sys.argv[1])
print(sum(1 for j in d if j["status"] in ("RUNNING","QUEUED")))' "$jobs_json")
  [[ "$non_terminal" == "0" ]] && g "no RUNNING/QUEUED after restart" || { r "found $non_terminal stuck jobs"; exit 1; }
  hens_json=$(curl -fsS "$API/api/v1/hens")
  stuck=$("$PY" -c '
import json, sys
d = json.loads(sys.argv[1])
print(sum(1 for h in d if h["state"] in ("HATCHING","WORKING")))' "$hens_json")
  [[ "$stuck" == "0" ]] && g "no HATCHING/WORKING hens after restart" || { r "found $stuck stuck hens"; exit 1; }
fi

# 11. graceful shutdown
b "[11] graceful shutdown"
stop_coopd_graceful && g "coopd exited cleanly on SIGTERM"

b "== ALL E2E CHECKS PASSED (${MODE}) =="
