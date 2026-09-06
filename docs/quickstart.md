# Quickstart

Get a Hen reasoning in about a minute.

## 1. Install

```bash
curl -fsSL https://raw.githubusercontent.com/dcluomax/coop/main/scripts/install.sh | sh
```

This drops `coopd` (daemon) and `coop` (CLI) into `/usr/local/bin` (or
`~/.local/bin`). Prefer building from source? See the [README](../README.md#-install).

## 2. Seal your model key in a BYOK vault

```bash
export COOP_PASSPHRASE='change-me'
coop vault init ~/.coop/vault.json
COOP_SECRET_VALUE='sk-ant-...' coop vault put ~/.coop/vault.json byok-anthropic
```

The vault is an `xchacha20poly1305` sealed file (mode `0600`); the passphrase
never touches disk.

## 3. Start the daemon (auto-unlocking the vault)

```bash
COOP_VAULT=~/.coop/vault.json coopd serve &
coop doctor --require-vault
```

`doctor` probes the daemon's health, orchestrator readiness, farm access, vault,
and session capabilities. It exits nonzero if a probe fails or the required
vault is locked. Missing persistent sessions are reported but do not prevent
built-in model jobs.

## 4. Hatch a starter Hen — no YAML required

The starter command creates a portable Hen with `bash`,
`file_read`, and `file_write`, enables 30-day episodic memory, and hatches it:

```bash
coop hen starter aria
coop job run local.coop/aria "Create hello.txt with one useful idea" --wait
```

The starter uses `network.policy: open` for cross-platform compatibility; it
does not restrict network egress. Before creating anything, it asks the daemon
to validate the provider configuration and resolve the key. This preflight
does **not** make a model request or guarantee provider reachability or quota.

Without `--wait`, `job run` returns the existing `{"job_id":"…"}` response.
Attach later with `coop job wait <job-id>`. Both wait commands print the terminal
job JSON; `FAILED`, `CANCELLED`, HTTP errors, and timeouts exit nonzero.

Use a different secret reference or model when needed:

```bash
coop hen starter pepper \
  --provider-id vault:my-anthropic-key \
  --model claude-sonnet-4-5-20250929
```

### OpenAI or a local compatible server

For OpenAI, store its key **during step 2, before starting the daemon**:

```bash
COOP_SECRET_VALUE='sk-...' coop vault put ~/.coop/vault.json byok-openai
```

Then, after starting the daemon:

```bash
coop hen starter pepper --provider openai
```

This selects `vault:byok-openai` and `gpt-4o-mini`; `--provider-id` and `--model`
override either default. A daemon already holding an unlocked vault must
reload/unlock it again or restart to see newly stored keys.

For an existing Ollama server with `llama3.1` available:

```bash
coop hen starter lobo --provider openai-compat \
  --base-url http://localhost:11434/v1 \
  --provider-id none --model llama3.1
```

`openai-compat` requires `--base-url` and defaults to the keyless `none` reference
and `llama3.1`. Choose a model actually served by your endpoint; for hosted
compatible services, override `--provider-id` with a vault/Azure key reference.
The provider URL is reached **from the daemon**, so `localhost` means the daemon
host (or its container), not a remote CLI machine. Keyless setups can skip the
vault steps and use `coop doctor` without `--require-vault`.

### Inspect and recover jobs

```bash
coop job list --status failed --order desc --limit 20
coop job list --hen-id local.coop/aria --q "hello.txt" --limit 20 --offset 0
coop job retry <failed-or-cancelled-job-id>
coop job cancel <queued-job-id>
coop job wait <job-id> --interval-s 2 --timeout-s 600
```

Retry creates a new job and keeps the original history. Cancellation is
**QUEUED-only**: it does not stop a running job or active process. A wait timeout
only stops the CLI's observation; it does not cancel the job. See
[configuration.md](./configuration.md#cli) for deadlines and diagnostics.

For full control over tools, personality, network policy, fallbacks, and
inheritance, edit [`examples/aria.yaml`](../examples/aria.yaml) and use
`coop hen create examples/aria.yaml`.

CLI-hosted agents (`claude-code`, `codex`, and `gh-copilot`) require
`network.policy: open` in v0.1 because their tmux sessions are not yet wrapped
by the per-Hen network sandbox. Use the built-in `anthropic` agent kind when a
strict `off` or `allowlist` policy is required.

Open <http://127.0.0.1:9700/> to watch your hens in the Farm UI — click any hen
to drop into a live PTY shell in its workdir.

## Next

- Run it 24/7 or on another machine → [deployment.md](./deployment.md)
- Every environment variable → [configuration.md](./configuration.md)
- Bridge it to Discord → [discord.md](./discord.md)
