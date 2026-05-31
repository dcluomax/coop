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
