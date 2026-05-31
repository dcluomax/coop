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
  - Non-`bash` tools run **in-process** in `coopd` — no kernel sandbox. The
    `bash` tool **is** sandboxed per instance (see C5/H7); a fully
    containerized tool runtime for the rest is on the v0.2 roadmap.
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
| C5  | **Per-instance `bash` sandbox.** Shell commands are confined to the hen's own workdir with an OS-native sandbox — macOS Seatbelt (`sandbox-exec`) and Linux Bubblewrap (`bwrap`): writes outside the workdir are denied and sibling hens' workdirs are unreadable, so one chicken cannot read or tamper with another. A cached capability probe falls back to env-scrub + `cwd` confinement (with a one-time warning) where the OS sandbox is unavailable; `COOP_SANDBOX=0` disables it. Windows strong confinement requires WSL/containers (limitation). |
| H7  | **`bash` environment scrub.** The shell runs with `env_clear()` and a minimal allowlist (`PATH`, `HOME`/`TMPDIR`=workdir, `COOP_HEN_*`, locale), so host secrets (vault passphrase, API keys, bearer tokens) and one hen's env never leak into another's shell. |
| H8  | **Unique-per-instance workdir.** Workdirs key on `HenId::workdir_key()` (`<coop>__<name>`), so a leased-in `bob.coop/aria` cannot collide with a local `alice.coop/aria`. |
| C6  | **Per-hen network egress policy.** A hen manifest's `network:` block (`off` / `allowlist` / `open`) restricts where the hen may reach. Enforcement is twofold: (1) the in-process `http` tool applies an L7 host+port allowlist on top of the existing SSRF guard (C2); (2) the `bash`/tmux OS sandbox denies **all direct socket egress** for `off`/`allowlist` hens — Linux via an empty network namespace (`bwrap --unshare-net`: raw sockets, `curl --noproxy`, direct-IP connects all fail with `ENETUNREACH`), macOS via Seatbelt `(deny network*)`. Under `allowlist`, host-scoped egress is delivered **only** through the `http` tool in v1 (a forced-egress proxy giving *bash* allow-listed egress on Linux is the documented follow-up). **Fail-closed:** a hen requesting a policy stricter than `open` on a host that cannot enforce it (no user namespaces / Seatbelt, or `COOP_SANDBOX=0`), or a tmux CLI agent (`agent_kind != anthropic`, an unconfined egress surface in v1), **refuses to hatch** rather than silently running open. See [`docs/net-isolation.md`](./docs/net-isolation.md). |
| H6  | WebSocket frames capped (`/watch`: 64 KiB; `/shell`: 256 KiB).                |
| M6  | Discord connector default-denies; only IDs in `COOP_DISCORD_ALLOWED_USERS` (or `allowed_user_ids` JSON field) can dispatch jobs. |
| L1  | Farm UI's xterm.js + addon load with SRI (`integrity=sha384-…`).              |
| M1  | **Anthropic API key heap-zeroized.** The BYOK key is held in `Zeroizing<String>` so its buffer is wiped when the adapter (and every clone) drops, and the adapter's `Debug` impl redacts it — the key never reaches logs or error strings. |
| M3  | **Prompt length bound.** `submit_job` and `submit_task` reject prompts over `COOP_MAX_PROMPT_BYTES` (default 256 KiB; `0` disables) with HTTP 413, capping per-request memory so one client can't OOM the daemon. |
| LR1 | **Login throttle.** `/api/v1/auth/login` records failed attempts per client IP; once an IP burns `COOP_LOGIN_MAX_ATTEMPTS` (default 10) failures within 60s it gets HTTP 429 + `Retry-After`, slowing token brute-forcing. A successful login clears the counter. Behind a reverse proxy this degrades to a global throttle (all requests share the proxy IP). |
| LP1 | **Lease policy**. Leased hens can be pinned to a sandboxed CLI framework (`claude-code` / `codex` / `gh-copilot`) at manifest-validation time; insecure brains (`anthropic` in-process, raw `shell`) are refused for lease unless `lease.require_framework: false` is explicit. The farm owner declares `allowed_tools:` (subset of `tools:`) — for the in-process Anthropic brain this is a hard wall in `invoke_tool` (denied tools never execute and are also hidden from the brain's tool catalog). For CLI-framework hens the tool list is advisory: the hosted CLI governs its own tool calls (full `--allowedTools` plumbing is on the v0.2 roadmap). A `topic_filter` (case-insensitive plain-substring `deny_keywords` + `allow_keywords`; deny wins) is enforced **universally** on every leased prompt at `/api/v1/hens/:id/jobs` (HTTP 403) and at `/shell/send` / task dispatch (`PermissionDenied`). |

## Known limitations (still accepted for v0.1)

- Farm UI loads xterm.js from a CDN (with SRI). Offline bundling is planned.
- Anthropic error bodies echoed to `/watch` subscribers (M2).
- GitHub Actions are pinned by mutable tag (not commit SHA); release
  artifacts are unsigned. Sigstore signing is on the v0.2 roadmap (L2).
- **Per-hen network (C6) v1 scope:** under `allowlist`, `bash`/tmux get **no
  direct** egress; allow-listed egress flows only through the in-process `http`
  tool. A Linux forced-egress proxy that gives *bash* allow-listed egress, SNI
  re-verification, and sentinel-token secret injection are documented
  follow-ups (see `docs/net-isolation.md`). tmux CLI agents are not yet wrapped
  in the per-hen sandbox, so strict-policy hens with `agent_kind != anthropic`
  fail closed (refuse to hatch).

## Disclosure timeline

We follow **90-day coordinated disclosure** by default, accelerated for active exploitation.
