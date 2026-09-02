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
```

## 4. Hatch a starter Hen — no YAML required

The starter command creates a safe, cross-platform Hen with `bash`,
`file_read`, and `file_write`, enables 30-day episodic memory, and hatches it:

```bash
coop hen starter aria
coop job run local.coop/aria "Create hello.txt with one useful idea"
coop job wait <job-id>
```

Use a different secret reference or model when needed:

```bash
coop hen starter pepper \
  --provider-id vault:my-anthropic-key \
  --model claude-sonnet-4-5-20250929
```

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
