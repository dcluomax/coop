# Security Policy

## Supported Versions

Coop is **pre-alpha**. Only `main` is supported. There is no LTS yet.

## Reporting a Vulnerability

**Please do not open a public GitHub issue.**

Use GitHub's [Private Security Advisory](https://github.com/coop-network/coop/security/advisories/new) flow, or email **security@coop.network** (placeholder; route via private advisory until provisioned).

You should receive a response within **5 business days**. If you do not, please follow up.

## Threat model (v0.1 ALONE FARMER)

- **In-scope**: vault sealing/unsealing, hen workdir isolation, PTY shell
  auth bypass on loopback, brain key leakage in logs, tool sandbox escape.
- **Out of scope (known + accepted for v0.1)**:
  - Tools run **in-process** in `coopd` — no kernel sandbox. Containerized
    tool runtime is on the v0.2 roadmap.
  - HTTP API and PTY WSS bind to `127.0.0.1` only. Set `COOP_API_TOKEN` for
    bearer auth; set `COOP_PUBLIC=1` only after that to allow non-loopback
    binds (the daemon refuses non-loopback `Host`/`Origin` headers
    otherwise — see "Hardening" below).

## Hardening shipped in `main`

These controls land in the current source tree:

| ID  | Control                                                                       |
|-----|-------------------------------------------------------------------------------|
| C1  | `file_read` / `file_write` confine to the hen's workdir (no `..`, no `/`, symlink escapes rejected via canonicalization). |
| C2  | `http` tool blocks SSRF: scheme must be http/https; resolved IPs in loopback/RFC1918/CGNAT/link-local/IPv6 ULA are refused; redirects capped at 3 and re-validated per hop. |
| C3  | WebSocket endpoints (`/api/v1/watch`, `/api/v1/hens/:id/shell`) gated by `Host`/`Origin` allowlist (loopback only by default). |
| C4  | Same middleware fronts the JSON API → no CSRF from cross-origin browser pages. |
| H1  | `~/.coop` is `0700`; `vault.json` and `state.redb` are `0600`.                |
| H2  | Vault salt is held in-memory; `persist()` no longer re-reads the file → vault survives accidental deletion mid-run. |
| H3  | `bash` tool ignores model-supplied `workdir`; always uses the hen's workdir.   |
| H6  | WebSocket frames capped (`/watch`: 64 KiB; `/shell`: 256 KiB).                |
| M6  | Discord connector default-denies; only IDs in `COOP_DISCORD_ALLOWED_USERS` (or `allowed_user_ids` JSON field) can dispatch jobs. |
| L1  | Farm UI's xterm.js + addon load with SRI (`integrity=sha384-…`).              |
| LP1 | **Lease policy**. Leased hens can be pinned to a sandboxed CLI framework (`claude-code` / `codex` / `gh-copilot`) at manifest-validation time; insecure brains (`anthropic` in-process, raw `shell`) are refused for lease unless `lease.require_framework: false` is explicit. The farm owner declares `allowed_tools:` (subset of `tools:`) — for the in-process Anthropic brain this is a hard wall in `invoke_tool` (denied tools never execute and are also hidden from the brain's tool catalog). For CLI-framework hens the tool list is advisory: the hosted CLI governs its own tool calls (full `--allowedTools` plumbing is on the v0.2 roadmap). A `topic_filter` (case-insensitive plain-substring `deny_keywords` + `allow_keywords`; deny wins) is enforced **universally** on every leased prompt at `/api/v1/hens/:id/jobs` (HTTP 403) and at `/shell/send` / task dispatch (`PermissionDenied`). |

## Known limitations (still accepted for v0.1)

- Farm UI loads xterm.js from a CDN (with SRI). Offline bundling is planned.
- No rate-limit on `/api/v1/auth/login`.
- Anthropic API key not zeroized on heap (M1).
- Anthropic error bodies echoed to `/watch` subscribers (M2).
- `submit_job` accepts unbounded prompt length (M3).
- GitHub Actions are pinned by mutable tag (not commit SHA); release
  artifacts are unsigned. Sigstore signing is on the v0.2 roadmap (L2).

## Disclosure timeline

We follow **90-day coordinated disclosure** by default, accelerated for active exploitation.
