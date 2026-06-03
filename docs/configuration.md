# Configuration

`coopd` is configured entirely through environment variables — no config file.

## Core

| Variable | Default | Purpose |
|----------|---------|---------|
| `COOP_DATA_DIR` | `~/.coop` | Data directory (vault, redb state, hen workdirs; `0700`). |
| `COOP_LOG` | `info` | Tracing filter, e.g. `coopd=debug`. |
| `COOP_VAULT` + `COOP_PASSPHRASE` | *(unset)* | Auto-unlock this sealed vault at startup. |
| `COOP_SANDBOX` | `1` | Set to `0` to disable the per-hen `bash` OS sandbox (not recommended). |
| `COOP_MARKET_URL` | `https://farm.startcaas.com` | Public Market URL shown in the Farm UI. |

## Exposure & auth

| Variable | Default | Purpose |
|----------|---------|---------|
| `COOP_API_TOKEN` | *(unset)* | Bearer token for the API/UI. **Required before exposing beyond loopback.** Unset = auth disabled. |
| `COOP_PUBLIC` | *(unset)* | Set to `1` to accept non-loopback `Host`/`Origin` headers (needed for LAN/public binds). |
| `COOP_LOGIN_MAX_ATTEMPTS` | `10` | Failed `/auth/login` attempts per client IP per 60s before HTTP 429. |
| `COOP_MAX_PROMPT_BYTES` | `262144` | Max job/task prompt size in bytes (`0` disables); over-size → HTTP 413. |

Auth is opt-in: with `COOP_API_TOKEN` set, every `/api/v1/*` request and the UI
must present the token via `Authorization: Bearer <token>`, a `?token=` query
param, or the `coop_token` cookie (set by the `/login` page). Healthchecks are
always exempt.

## Discord connector

| Variable | Purpose |
|----------|---------|
| `COOP_DISCORD_TOKEN` | Bot token. |
| `COOP_DISCORD_GUILD_ID` | Server (guild) ID. |
| `COOP_DISCORD_PREFIX` | Command prefix (default `!coop`). |
| `COOP_DISCORD_ALLOWED_USERS` | Comma-separated user IDs allowed to dispatch jobs (default-deny). |

See [discord.md](./discord.md).

## BYOK secret backends

A Hen's `brain.provider_id` selects **where its model API key comes from**:

| `provider_id` | Backend | Notes |
|---------------|---------|-------|
| `vault:<secret>` | Local sealed vault | Default. Sealed XChaCha20-Poly1305 file, unlocked via `COOP_PASSPHRASE` / `/api/v1/vault/unlock`. |
| `azure-kv://<vault>/<secret>` | **Azure Key Vault** | Fetched at run time over HTTPS; never written to disk. Optional `/<version>` suffix pins a secret version. |

### Azure Key Vault

When a `provider_id` uses the `azure-kv://` scheme, `coopd` fetches the secret
from Azure Key Vault using credentials from the environment (the standard Azure
`EnvironmentCredential` model). Credentials are resolved in this order:

| Variable(s) | Auth mode |
|-------------|-----------|
| `AZURE_KEYVAULT_TOKEN` | A pre-acquired AAD bearer token (managed identity, `az account get-access-token --resource https://vault.azure.net`, …). Not auto-refreshed. |
| `AZURE_TENANT_ID` + `AZURE_CLIENT_ID` + `AZURE_CLIENT_SECRET` | Service principal (OAuth2 client-credentials). Tokens are acquired and cached automatically until just before expiry. |

Optional overrides for sovereign / national clouds:

| Variable | Default | Purpose |
|----------|---------|---------|
| `AZURE_KEYVAULT_DNS_SUFFIX` | `vault.azure.net` | Key Vault hostname suffix (e.g. `vault.azure.cn`, `vault.usgovcloudapi.net`). |
| `AZURE_AUTHORITY_HOST` | `https://login.microsoftonline.com` | AAD authority host. |

The service principal (or token) needs the **Get** secret permission on the
target vault (`Key Vault Secrets User` role under RBAC, or a `get` secrets
access policy). Example manifest:

```yaml
brain:
  provider_id: azure-kv://my-coop-kv/byok-anthropic
  model: claude-sonnet-4-5-20250929
```

Secrets fetched from Azure Key Vault are held in memory only (zeroized on drop)
and never persisted to the local vault file.

## Brain providers

A Hen's `brain.provider` selects **which model API the adapter speaks**.
It is independent of `brain.provider_id` (which selects where the *key* is
read from, see above). Default is `anthropic`, so v0.1 manifests are unchanged.

| `brain.provider` | Endpoint | Notes |
|------------------|----------|-------|
| `anthropic` (default) | Anthropic Messages API | Honors `brain.auto_route` (Haiku/Sonnet/Opus tiering). |
| `openai` | `https://api.openai.com/v1/chat/completions` | Single `brain.model`; `auto_route` is ignored. |
| `openai-compat` | Any OpenAI-compatible Chat Completions server | Requires `brain.base_url`. Works with Ollama, vLLM, LM Studio, OpenRouter, Groq, etc. |

For `openai-compat`, `brain.base_url` must be an `http(s)` URL and may not
target the cloud-metadata endpoint (`169.254.169.254`). Local servers that
need no API key use the `provider_id: none` sentinel, which yields an empty
key and skips the vault lookup.

```yaml
# Hosted OpenAI
brain:
  provider_id: vault:byok-openai
  provider: openai
  model: gpt-4o-mini
```

```yaml
# Local Ollama (keyless)
brain:
  provider_id: none
  provider: openai-compat
  base_url: http://localhost:11434/v1
  model: llama3.1
```

```yaml
# OpenRouter (BYOK key from the sealed vault)
brain:
  provider_id: vault:byok-openrouter
  provider: openai-compat
  base_url: https://openrouter.ai/api/v1
  model: anthropic/claude-3.5-sonnet
```

Tool calls round-trip as structured `tool_use`/`tool_result` blocks across all
providers; the OpenAI adapter translates them to and from OpenAI's
`tool_calls` / `role:tool` message shape and normalizes `finish_reason`.

Both adapters also support **streaming** (`BrainAdapter::stream`): provider SSE
streams are decoded into incremental text deltas plus a final assembled
response.

## Fallback brains

`brain.fallbacks` is an ordered list of full brain specs. When a call to the
primary brain fails (network error, rate limit, provider outage), the runtime
transparently retries each fallback in order and the first success wins. Each
entry takes the same fields as `brain` (`provider_id`, `provider`, `base_url`,
`model`) and is validated identically.

```yaml
brain:
  provider_id: vault:byok-anthropic
  provider: anthropic
  model: claude-sonnet-4.6
  fallbacks:
    # 1st choice on failure: OpenAI
    - provider_id: vault:byok-openai
      provider: openai
      model: gpt-4o-mini
    # last resort: a keyless local model that's always reachable
    - provider_id: none
      provider: openai-compat
      base_url: http://localhost:11434/v1
      model: llama3.1
```

A Hen with fallbacks reports healthy if *any* link in the chain is reachable.

## CLI

The `coop` CLI talks to `COOP_API` (default `http://127.0.0.1:9700`). Set it to
reach a remote daemon. When the daemon has auth enabled, give the CLI the same
token via `COOP_API_TOKEN` (env) or `--token <TOKEN>`; it is sent as an
`Authorization: Bearer` header. An empty/unset token sends no auth header,
matching a daemon started without `COOP_API_TOKEN`.

```sh
export COOP_API=https://farm.example.com
export COOP_API_TOKEN=…   # same value the daemon was started with
coop farm
coop hen list
```
