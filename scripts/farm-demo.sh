#!/usr/bin/env bash
# Farm demo: seeds a coopd with multiple hens in different lifecycle states,
# verifies the UI + every farm endpoint exercises correctly.
#
# Usage:
#   scripts/farm-demo.sh              # auto-teardown
#   COOP_DEMO_KEEP=1 scripts/farm-demo.sh   # leaves coopd running for browser inspection
set -euo pipefail
trap 'rc=$?; teardown; exit $rc' EXIT INT TERM

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${COOP_DEMO_PORT:-9700}"
API="http://127.0.0.1:${PORT}"
DATA_DIR="$(mktemp -d -t coop-farm-demo-XXXXXX)"
LOG="/tmp/coop-farm-demo.log"
COOPD_PID=""

b() { printf "\033[1m%s\033[0m\n" "$*"; }
g() { printf "  \033[32m✓\033[0m %s\n" "$*"; }
r() { printf "  \033[31m✗\033[0m %s\n" "$*" >&2; }
y() { printf "  \033[33m·\033[0m %s\n" "$*"; }

teardown() {
  set +e
  if [[ "${COOP_DEMO_KEEP:-0}" == "1" && -n "$COOPD_PID" ]]; then
    y "leaving coopd running on $API (pid $COOPD_PID); data: $DATA_DIR"
    y "open http://127.0.0.1:${PORT}/ in a browser to inspect"
    return
  fi
  [[ -n "$COOPD_PID" ]] && kill "$COOPD_PID" 2>/dev/null
  wait 2>/dev/null
  rm -rf "$DATA_DIR"
}

PY="$(command -v python3 || command -v python)"

cd "$ROOT"
[[ -x ./target/debug/coopd ]] || cargo build -q --workspace

: > "$LOG"
./target/debug/coopd --data-dir "$DATA_DIR" --log info serve --addr "127.0.0.1:${PORT}" >> "$LOG" 2>&1 &
COOPD_PID=$!
for _ in $(seq 1 40); do
  curl -fsS "$API/api/v1/farm" >/dev/null 2>&1 && break
  sleep 0.25
done

b "== Farm demo =="

# --- seed multiple hens in different states ---
b "[1] seed hens"
declare -a HENS=(aria bolt cleo dax eve)
for name in "${HENS[@]}"; do
  curl -fsS -X POST "$API/api/v1/hens" -H 'content-type: application/yaml' --data-binary "$(printf 'spec_version: coop/v1\nname: %s\nbrain:\n  provider_id: vault:byok\n  model: claude-sonnet-4-5-20250929\ntools: [bash, file_read, file_write]\n' "$name")" >/dev/null
  g "created local.coop/$name"
done

# hatch a subset
curl -fsS -X POST "$API/api/v1/hens/local.coop%2Faria/hatch" >/dev/null && g "hatched aria"
curl -fsS -X POST "$API/api/v1/hens/local.coop%2Fbolt/hatch" >/dev/null && g "hatched bolt"
curl -fsS -X POST "$API/api/v1/hens/local.coop%2Fcleo/hatch" >/dev/null && g "hatched cleo"
# put one to sleep
curl -fsS -X POST "$API/api/v1/hens/local.coop%2Fcleo/sleep" >/dev/null && g "cleo -> sleeping"

# --- assertions on endpoints ---
b "[2] /api/v1/farm shape"
farm=$(curl -fsS "$API/api/v1/farm")
echo "$farm" | "$PY" -m json.tool | sed 's/^/    /'
hen_count=$(printf '%s' "$farm" | "$PY" -c 'import sys,json;print(json.load(sys.stdin)["hen_count"])')
[[ "$hen_count" == "5" ]] && g "hen_count = 5" || { r "expected 5 got $hen_count"; exit 1; }

b "[3] /api/v1/hens list + filter"
all=$(curl -fsS "$API/api/v1/hens" | "$PY" -c 'import sys,json;d=json.load(sys.stdin);print(len(d))')
idle=$(curl -fsS "$API/api/v1/hens?state=IDLE" | "$PY" -c 'import sys,json;d=json.load(sys.stdin);print(len(d))')
defined=$(curl -fsS "$API/api/v1/hens?state=DEFINED" | "$PY" -c 'import sys,json;d=json.load(sys.stdin);print(len(d))')
sleeping=$(curl -fsS "$API/api/v1/hens?state=SLEEPING" | "$PY" -c 'import sys,json;d=json.load(sys.stdin);print(len(d))')
g "total=$all idle=$idle defined=$defined sleeping=$sleeping"
[[ "$all" == "5" && "$idle" == "2" && "$defined" == "2" && "$sleeping" == "1" ]] \
  || { r "filter mismatch"; exit 1; }
g "state filters work"

b "[4] /api/v1/hens/:id detail"
detail=$(curl -fsS "$API/api/v1/hens/local.coop%2Faria")
echo "$detail" | "$PY" -m json.tool | head -10 | sed 's/^/    /'
state=$(printf '%s' "$detail" | "$PY" -c 'import sys,json;print(json.load(sys.stdin)["state"])')
[[ "$state" == "IDLE" ]] && g "aria.state = IDLE" || { r "wrong state"; exit 1; }

b "[5] / UI serves single-page app"
ui=$(curl -fsS "$API/")
echo "$ui" | grep -q '<title>🐔 Coop Farm</title>' && g "title present"   || { r "title missing"; exit 1; }
echo "$ui" | grep -q 'cdn.jsdelivr.net/npm/xterm'   && g "xterm.js linked"  || { r "xterm missing"; exit 1; }
echo "$ui" | grep -q '/api/v1/hens'                 && g "calls hens API"   || { r "API call missing"; exit 1; }
echo "$ui" | grep -q '/shell'                       && g "wires shell WSS"  || { r "shell WSS missing"; exit 1; }

b "[6] WSS shell into a specific hen"
"$PY" - "$PORT" <<'PY'
import asyncio, json, sys
port = sys.argv[1]
try:
    import websockets
except ImportError:
    import subprocess
    subprocess.check_call([sys.executable, "-m", "pip", "install", "-q", "websockets"])
    import websockets
async def main():
    url = f"ws://127.0.0.1:{port}/api/v1/hens/local.coop%2Fbolt/shell"
    async with websockets.connect(url) as ws:
        await ws.send(json.dumps({"type":"resize","cols":120,"rows":40}))
        await ws.send(b"echo FARM_DEMO_OK && basename \"$COOP_HEN_WORKDIR\" && exit\n")
        buf = bytearray()
        try:
            while True:
                m = await asyncio.wait_for(ws.recv(), timeout=4)
                if isinstance(m,bytes): buf.extend(m)
                else:
                    if json.loads(m).get("type") == "exit": break
        except asyncio.TimeoutError: pass
        out = buf.decode("utf-8","replace")
        assert "FARM_DEMO_OK" in out, f"missing token in:\n{out}"
        assert "bolt" in out, f"missing hen name in:\n{out}"
        print("  \033[32m✓\033[0m shell rooted in bolt's workdir, streamed cleanly")
asyncio.run(main())
PY

b "[7] WSS /watch sees a state change"
"$PY" - "$PORT" "$ROOT" <<'PY' &
import asyncio, json, sys
port = sys.argv[1]
try:
    import websockets
except ImportError:
    import subprocess
    subprocess.check_call([sys.executable, "-m", "pip", "install", "-q", "websockets"])
    import websockets
async def main():
    async with websockets.connect(f"ws://127.0.0.1:{port}/api/v1/watch") as ws:
        found = False
        try:
            while True:
                m = await asyncio.wait_for(ws.recv(), timeout=6)
                j = json.loads(m)
                if j.get("type") == "hen_state_changed" and "dax" in j.get("id",""):
                    found = True
                    break
        except asyncio.TimeoutError: pass
        if found: print("  \033[32m✓\033[0m saw dax state change over /watch")
        else: print("  \033[31m✗\033[0m no dax event"); sys.exit(1)
asyncio.run(main())
PY
WPID=$!
sleep 0.5
curl -fsS -X POST "$API/api/v1/hens/local.coop%2Fdax/hatch" >/dev/null
wait $WPID

b "[8] CLI parity"
"$ROOT/target/debug/coop" --api "$API" hen list 2>/dev/null | head -3 | sed 's/^/    /' || true
"$ROOT/target/debug/coop" --api "$API" farm 2>/dev/null | sed 's/^/    /'
g "CLI talks to live farm"

if [[ "${COOP_DEMO_KEEP:-0}" == "1" ]]; then
  b "Demo coopd left running on $API"
  echo "  Open http://127.0.0.1:${PORT}/ in a browser."
  echo "  Stop with: kill $COOPD_PID"
fi

b "== Farm demo PASSED =="
