# Configuration

`coopd` is configured entirely through environment variables — no config file.

## Core

| Variable | Default | Purpose |
|----------|---------|---------|
| `COOP_DATA_DIR` | `~/.coop` | Data directory (vault, redb state, hen workdirs; `0700`). |
| `COOP_LOG` | `info` | Tracing filter, e.g. `coopd=debug`. |
| `COOP_VAULT` + `COOP_PASSPHRASE` | *(unset)* | Auto-unlock this sealed vault at startup. |
| `COOP_SANDBOX` | `1` | Set to `0` to disable the per-hen `bash` OS sandbox (not recommended). |

## Exposure & auth

| Variable | Default | Purpose |
|----------|---------|---------|
| `COOP_API_TOKEN` | *(unset)* | Bearer token for the API/UI. **Required before exposing beyond loopback.** Unset = auth disabled. |
| `COOP_PUBLIC` | *(unset)* | Set to `1` to accept non-loopback `Host`/`Origin` headers (needed for LAN/public binds). |
| `COOP_PUBLIC_ORIGIN` | *(unset)* | Exact browser origin required with `COOP_PUBLIC=1`, including scheme and non-default port (for example `https://farm.example.com`). |
| `COOP_LOGIN_MAX_ATTEMPTS` | `10` | Failed `/auth/login` attempts per client IP per 60s before HTTP 429. |
| `COOP_MAX_PROMPT_BYTES` | `262144` | Max job/task prompt size in bytes (`0` disables); over-size → HTTP 413. |

Auth is opt-in: with `COOP_API_TOKEN` set, every `/api/v1/*` request and the UI
must present the token via `Authorization: Bearer <token>` or the `coop_token`
cookie (set by the `/login` page). Tokens are not accepted in URLs, where
browser history and proxy logs could expose them. Healthchecks are always
exempt.

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

Both adapters implement provider-level **streaming** (`BrainAdapter::stream`):
provider SSE streams are decoded into incremental text deltas plus a final
assembled response. The v0.1 job runner still uses complete responses; live
incremental runner output is not shipped yet.

## Runtime limits

Each job is capped at 16 reason/tool turns. This v0.1 safety limit is compiled
in and is not configurable; a job that reaches it fails with
`max turns (16) exhausted`.

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

Global options work before or after the subcommand:

| Option | Environment | Default | Purpose |
|--------|-------------|---------|---------|
| `--api` | `COOP_API` | `http://127.0.0.1:9700` | Daemon base URL, optionally with a reverse-proxy path prefix. |
| `--token` | `COOP_API_TOKEN` | *(empty)* | API bearer token. Prefer the environment over shell-history/process-list exposure. |
| `--request-timeout-s` | `COOP_REQUEST_TIMEOUT_S` | `300` | Total deadline per HTTP request, including response body; `1..=86400` seconds. |
| `--log` | `COOP_LOG` | `warn` | CLI logging filter. |

The base URL must be an absolute `http://` or `https://` URL with no credentials,
query, or fragment. Trailing slashes are normalized; path prefixes are preserved.
Use HTTPS when sending a token to a remote daemon. Redirects are never followed,
including same-host redirects: point `--api` at the final address.

Connections are bounded by the smaller of 10 seconds and the request deadline.
The default 300-second request budget accommodates the daemon's default
180-second synchronous delegation wait. If you increase the daemon's
`COOP_DELEGATE_TIMEOUT_SECS`, also allow sufficient CLI request time:

```sh
coop --request-timeout-s 900 hen delegate local.coop/aria local.coop/scout "Review the plan"
```

Changing the CLI timeout does not change the daemon's timeout. Every HTTP command
exits nonzero on authentication, connection, HTTP-status, or response-decoding
failure. Errors include the HTTP status and available API error message, including
plain-text proxy errors. Successful existing command JSON formats are unchanged.
Local `vault` commands do not contact the daemon.

### Starter providers

`coop hen starter [name]` still defaults to `aria` and Anthropic. Provider
selection uses the existing built-in brain adapters, not an external CLI agent:

| `--provider` | Default `--provider-id` | Default `--model` | `--base-url` |
|--------------|-------------------------|-------------------|--------------|
| `anthropic` | `vault:byok-anthropic` | `claude-sonnet-4-5-20250929` | Not accepted. |
| `openai` | `vault:byok-openai` | `gpt-4o-mini` | Not accepted. |
| `openai-compat` | `none` | `llama3.1` | Required; include the endpoint's API prefix, usually `/v1`. |

`--provider-id` and `--model` override these starter defaults; model availability
depends on your account/server. Only `openai-compat` permits the keyless `none`
reference. Provider base URLs must also be HTTP(S) URLs without credentials,
query, or fragment; the manifest's metadata-endpoint restriction still applies.
The daemon, not the CLI, connects to that provider URL.

Starter preflight validates the manifest and constructs the provider adapter,
including resolving its key, **before** creating or hatching the Hen. It does not
send a model request, test account quota, or prove the model server is reachable.
If hatch fails after creation, the error identifies the created Hen for inspection;
the CLI does not silently recreate or delete it.

### Job operations and exit status

```sh
coop job run local.coop/aria "Summarize today's work" --wait --interval-s 2 --timeout-s 600
coop job wait <job-id> --interval-s 2 --timeout-s 600
coop job list --hen-id local.coop/aria --status FAILED --order desc --limit 50 --offset 0
coop job list --q "build error"
coop job retry <failed-or-cancelled-job-id>
coop job cancel <queued-job-id>
```

- `job run` without `--wait` retains the `{"job_id":"…"}` acknowledgement.
  With `--wait`, stdout contains the final job JSON instead. Polling options on
  `job run` require `--wait`.
- `job wait` and `job run --wait` default to a 2-second poll interval and a
  600-second total deadline. Both options must be greater than zero. The total
  deadline includes HTTP requests, response bodies, and sleeps; for `run --wait`,
  it also includes submission. The per-request timeout still applies.
- `DONE` exits zero. `FAILED` and `CANCELLED` print the complete terminal job JSON
  before exiting nonzero. A timeout does not cancel server-side work. If submission
  times out before an acknowledgement arrives, acceptance is unknown: inspect
  `job list` before resubmitting to avoid duplicate work.
- `job list` remains a JSON array, oldest first, with no default limit. `--status`
  accepts `QUEUED`, `RUNNING`, `DONE`, `FAILED`, or `CANCELLED` case-insensitively.
  `--limit` accepts `1..=500`, `--offset` is a nonnegative count of matching jobs
  to skip, and `--order asc|desc` selects creation order.
- `--q` (alias `--search`) searches identifiers, prompt, result, and error
  case-insensitively. It permits at most **256 UTF-8 bytes after trimming
  surrounding whitespace**, not 256 Unicode characters.
- Retry only accepts `FAILED`/`CANCELLED` jobs. It returns a new `{"job_id":"…"}`
  and preserves the original, copying its Hen, prompt, and delegation depth.
- Cancel only accepts **QUEUED** jobs and returns the updated job JSON.
  Already-`CANCELLED` is an idempotent success. `RUNNING`, `DONE`, and `FAILED`
  return HTTP 409: this command **does not stop an active process**.

Hen lifecycle commands are not cancellation operations either. `hen sleep`,
`hen wake`, and `hen delete` return HTTP 409 while that Hen has a running job.
Deletion also refuses a nonempty queue. Inspect `job list --hen-id <hen-id>` and
wait for running work to finish; cancel queued jobs individually if needed.
The CLI surfaces these errors and exits nonzero rather than claiming the work
was interrupted.

### Doctor

`coop doctor` concurrently reads `/api/v1/healthz`, `/api/v1/readyz`,
`/api/v1/farm`, `/api/v1/vault/status`, and `/api/v1/session/capabilities`.
It prints a JSON report with each actual response or error, then exits nonzero
if any probe fails, reports an invalid response, or misses a required capability.
Readiness checks the daemon's orchestrator, not just its HTTP listener.

The report's `client_version` identifies this CLI build, as does `coop --version`.
`checks.farm.response.coopd_version` comes from the **running daemon**, not the
installed daemon binary. Replacing binaries does not update a running process:
update both binaries, let active jobs finish, then restart the daemon to use new
server features. Doctor reports versions without assuming compatibility merely
because its basic probes pass.

```sh
coop doctor
coop doctor --require-vault
coop doctor --require-task-dispatch
```

A locked local vault is informational by default because Azure and keyless
providers can work without it; `--require-vault` makes unlocking mandatory.
A non-persistent shell is likewise reported without claiming external-agent
task dispatch works; `--require-task-dispatch` requires a persistent,
dispatch-capable daemon session backend. Failed HTTP probes always cause a
nonzero exit, even when the associated capability is optional. Doctor does not
create Hens, mutate the vault, or test model API credentials or model reachability.
