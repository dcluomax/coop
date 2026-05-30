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

## 4. Define + run a Hen

A starter manifest lives at [`examples/aria.yaml`](../examples/aria.yaml):

```yaml
spec_version: coop/v1
name: aria
brain:
  provider_id: vault:byok-anthropic
  model: claude-sonnet-4-5-20250929
tools: [bash, file_read, file_write, http]
```

```bash
coop hen create examples/aria.yaml
coop hen hatch  local.coop/aria
coop job run    local.coop/aria "list files in your workdir using bash"
coop job wait   <job-id>
```

Open <http://127.0.0.1:9700/> to watch your hens in the Farm UI — click any hen
to drop into a live PTY shell in its workdir.

## Next

- Run it 24/7 or on another machine → [deployment.md](./deployment.md)
- Every environment variable → [configuration.md](./configuration.md)
- Bridge it to Discord → [discord.md](./discord.md)
