# Changelog

All notable changes to this project are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [SemVer](https://semver.org/spec/v2.0.0.html) — pre-1.0 may break.

## [Unreleased]

### Added

- **Persistent Hen memory** — Hens now remember. Each completed job (success
  or failure) is recorded as a compact *episode* (prompt, short outcome, turn
  count, status), and the most recent episodes are replayed into the hen's
  system prompt on the next job — so a hen continues from context instead of
  starting from zero every time. Governed by the existing `memory:` manifest
  block:
  - `episodic_retention_days` — prune episodes older than N days (enforced).
  - `inherit_from` — a new hen copies a parent hen's episodes at creation and
    records lineage (`parent`/`generation`).
  - Inject count is capped (default 8, override `COOP_MEMORY_CONTEXT_ENTRIES`,
    `0` disables injection).
  - New: `GET`/`DELETE /api/v1/hens/:id/memory` and `coop hen memory <id>
    [--limit N]` / `coop hen forget <id>`. Deleting a hen purges its memory.
  - Audit: orchestrator emits a `memory_recorded` event per episode.
  - `semantic_summarize_every` remains reserved (LLM-summarized memory is a
    future phase). See `docs/memory.md`.

## [0.1.0-alpha.2] - 2026-06-11

### Security

- **C1** — `file_read`/`file_write` now confine all paths to the hen's
  workdir. Absolute paths, `..`, and symlinks escaping the base are rejected
  via canonicalization. (Shared helper: `coopd_tools::safe_path::safe_resolve`.)
- **C2** — `http` tool gains SSRF guard: scheme must be http/https; resolved
  IPs in loopback/RFC1918/CGNAT/link-local/multicast/IPv6 ULA are refused;
  redirects capped at 3 hops and re-validated per hop. Adds
  `coopd_tools::safe_net`.
- **C3 + C4** — New `safe_origin` middleware fronts every route: refuses
  requests whose `Host` header isn't a loopback name and whose `Origin`
  (when present) isn't a loopback URL. Defeats cross-origin WebSocket
  hijack against `/api/v1/hens/:id/shell` and browser-initiated CSRF
  against the JSON API. Disable with `COOP_PUBLIC=1` for deliberate
  public deployments (still requires `COOP_API_TOKEN`).
- **H1** — `~/.coop` is `0700`; `vault.json` and `state.redb` are `0600` on
  Unix.
- **H2** — `Vault` holds its salt in-memory; `persist()` no longer
  round-trips through the file, so an accidentally-deleted (or replaced)
  vault file mid-run won't silently rotate to an unrecoverable key.
- **H3** — `bash` tool ignores model-supplied `workdir`; the hen workdir
  from `ToolCtx` is always used.
- **H6** — WebSocket frames capped (`/watch`: 64 KiB; `/shell`: 256 KiB)
  to prevent OOM via 64 MiB default ceiling.
- **M6** — Discord connector now default-denies. Set
  `COOP_DISCORD_ALLOWED_USERS=<id>,<id>,…` (or the `allowed_user_ids` JSON
  field) to enumerate which Discord user IDs may dispatch jobs. Empty
  list = bot is dormant.
- **L1** — Farm UI's xterm.js + addon CDN scripts now carry SRI
  (`integrity="sha384-…"`).
- **Sandbox phase-1 hardening (Linux `bwrap`).** The bash sandbox now (a) runs
  with a locked, fixed `PATH` instead of inheriting the host's; (b) passes
  `--new-session` to defeat the `TIOCSTI` terminal-injection escape
  (CVE-2017-5226); and (c) prepends a `ulimit` prologue capping CPU time and
  output file size. Landlock filesystem confinement is deferred to phase-2.
- **LP1 (lease policy)** — `manifest.lease` gains three enforcement knobs:
  - `require_framework: bool` (default **true**). When `allow_lease: true`,
    the agent's `brain.kind` is rejected at manifest-validation time unless
    it is one of the sandboxed CLI frameworks: `claude-code`, `codex`,
    `gh-copilot`. The in-process `anthropic` brain and the raw `shell`
    brain are refused for lease unless the farm owner explicitly opts out
    with `require_framework: false`.
  - `allowed_tools: [..]` — subset of the manifest's `tools:` list. For the
    in-process Anthropic brain this is a **hard wall** in `invoke_tool`
    (denied tools never execute and are also hidden from the brain's tool
    catalog). For CLI-framework hens the allowlist is advisory: the hosted
    CLI process governs its own tool calls (full `--allowedTools`
    plumbing tracked for v0.2). Unknown tool names in `allowed_tools` are
    rejected at manifest load.
  - `topic_filter.{deny_keywords, allow_keywords}` — case-insensitive plain
    substring filters (no regex, to defeat ReDoS). Deny wins. Enforced
    **universally** on every leased prompt: `POST /api/v1/hens/:id/jobs`
    returns **HTTP 403** on violation; the WSS `/shell/send` path and
    internal task dispatch return `PermissionDenied`.

### Added

- **Farmhand remote-control seam (L2 Federation, phase-0).** New
  `coopd_core::remote` module lays the foundation for monitoring and steering
  the flock from another device: a three-tier `RemoteMode` (`off`/`view`/
  `control`), an outbound `RemoteBridge` trait, a secret-free `FarmEvent` /
  `RemoteCommand` schema, and an in-process `LoopbackBridge` reference
  implementation. The bridge is a deliberately fail-*open* side channel — a
  dead relay never blocks local execution. Design + roadmap in
  [docs/design/remote-farmhand.md](./docs/design/remote-farmhand.md).

- **Debian packages + Homebrew formula (packaging phase-2).** Releases now ship
  `.deb` packages for `amd64`, `arm64`, and `armhf` (built via `cargo-deb`; they
  install the `coopd`/`coop` binaries plus the hardened systemd unit and depend
  on `bubblewrap`/`tmux`), and a generated Homebrew formula
  (`packaging/homebrew/coop.rb`) published to the `dcluomax/homebrew-coop` tap:
  `brew install dcluomax/coop/coop`. See
  [docs/deployment.md](./docs/deployment.md#install).

- **Azure Key Vault BYOK backend.** A Hen's `brain.provider_id` may now use the
  `azure-kv://<vault>/<secret>[/<version>]` scheme to fetch its model API key
  from Azure Key Vault at run time instead of the local sealed vault.
  Credentials come from the environment (Azure `EnvironmentCredential` model):
  a static `AZURE_KEYVAULT_TOKEN`, or an `AZURE_TENANT_ID` /`AZURE_CLIENT_ID` /
  `AZURE_CLIENT_SECRET` service principal (tokens cached until just before
  expiry). Sovereign clouds via `AZURE_KEYVAULT_DNS_SUFFIX` /
  `AZURE_AUTHORITY_HOST`. The fetched secret is held in `Zeroizing` memory and
  never written to disk. New module `coopd_vault::azure`. See
  [docs/configuration.md](./docs/configuration.md#azure-key-vault).

- **OpenAI + OpenAI-compatible brains.** A Hen manifest may now set
  `brain.provider` to `openai` (api.openai.com) or `openai-compat` (any
  Chat Completions endpoint — Ollama, vLLM, LM Studio, OpenRouter, Groq…)
  alongside the default `anthropic`. `openai-compat` requires a
  `brain.base_url` (http(s); the cloud-metadata endpoint is refused). Keyless
  local servers use the `provider_id: none` sentinel. New adapter
  `coopd_brain::OpenAi` translates Coop's structured tool blocks to and from
  OpenAI `tool_calls`/`role:tool` messages and normalizes `finish_reason`.
  See [docs/configuration.md](./docs/configuration.md#brain-providers).

- **Structured tool-call fidelity.** Assistant/tool turns now round-trip as
  typed `tool_use`/`tool_result` content blocks (carrying the provider tool-use
  `id` and an `is_error` flag) instead of being flattened to plaintext, so
  multi-turn tool conversations survive across providers.

- **`cargo binstall` support.** `crates/coop-cli` now carries
  `[package.metadata.binstall]` mapping the crate to the GitHub release archives,
  so `cargo binstall coop-cli` installs the prebuilt `coop`/`coopd` binaries
  without compiling from source.

- **ARM compile-check in CI.** A new `arm-build` job compiles the
  `aarch64-unknown-linux-gnu` (native arm64 runner) and
  `armv7-unknown-linux-gnueabihf` (via `cross`) targets on every push/PR, so
  ARM breakage is caught before a release tag instead of at release time.

- **Streaming brains.** Both the Anthropic and OpenAI adapters now implement
  `BrainAdapter::stream`, decoding the provider SSE streams into `ReasonChunk`
  text deltas plus a final assembled response (tool calls are reassembled from
  the streamed `tool_use`/`tool_calls` fragments). Shared SSE framing lives in
  `coopd_brain::sse`.

- **Fallback brains.** `brain.fallbacks` (an ordered list of full brain specs)
  lets a Hen fail over from a primary provider to one or more backups — e.g.
  Anthropic primary with a local OpenAI-compatible model as backup. On a failed
  call the new `coopd_brain::FallbackBrain` decorator transparently retries the
  next adapter; the first success wins. See
  [docs/configuration.md](./docs/configuration.md#fallback-brains).

### Changed
- **Release toolchain.** `aarch64-unknown-linux-gnu` is now built natively on
  GitHub's hosted `ubuntu-24.04-arm` runner instead of QEMU/`cross` — faster and
  removes reliance on the stagnant `cross` toolchain for that target (`cross`
  is now used only for the armv7 Pi build, which has no native runner).
- **Fixed crate `repository` metadata** (`coop-network/coop` →
  `dcluomax/coop`) so `cargo binstall` resolves the correct release URLs.
- **Open-core split.** `coopd-market` has been moved out of the OSS workspace
  into a separate proprietary repo (`coop-market`). The OSS daemon now ships
  with 7 crates instead of 8; the cross-Coop market layer is enabled via the
  optional `--features market` Cargo flag and requires a sibling checkout of
  the private repo. See [AGENTS.md](./AGENTS.md) for the boundary contract.
- Pinned `rust-toolchain.toml` to `stable` and relaxed the workspace
  `[lints.clippy]` to drop the `pedantic` preset — new rustc releases keep
  adding pedantic lints that fail CI without surfacing real bugs.

### Removed

- `crates/coopd-market/` and `scripts/market-demo.sh` (moved to `coop-market`).

## [0.1.0-alpha] — TBD

First public preview. The **ALONE FARMER** milestone: single-Roost, local-only.

### Added

- `coopd` daemon binary with HTTP API on `127.0.0.1:9700`.
- `coop` CLI (`hen hatch/list/get`, `job run/get/list/wait`, `vault init/unlock/put`).
- Hen lifecycle state machine: `DEFINED → IDLE → WORKING → IDLE → SLEEPING`.
- Job runtime: per-job reason/tool loop, 16-turn cap, redb-backed persistence.
- Built-in tools: `bash`, `file_read`, `file_write`, `http`.
- Brain adapter: Anthropic (BYOK) via `coopd-brain`.
- Sealed vault (sodiumoxide, passphrase-derived key) for BYOK secrets.
- WSS event stream at `GET /api/v1/jobs/:id/watch`.
- **Farm UI** (`GET /`) — single-page, lists hens with live state badges.
- **Per-hen PTY shell** at `GET /api/v1/hens/:id/shell` (xterm.js in the browser).
- `coopd-market` v0 — in-memory local market mock (Listing/Bid/Accept/Cancel). _Moved to private `coop-market` repo before launch — see [Unreleased] above._
- Startup reconciler: interrupted `RUNNING` jobs → `FAILED`, stuck Hens → `Idle`.
- E2E test harness (`scripts/e2e.sh`, 12 checks), farm-demo, market-demo.
- CI matrix (ubuntu + macos) running fmt + clippy + test + doc + e2e.
- Apache-2.0 license, DCO sign-off requirement, Contributor Covenant CoC.

### Known limitations

See [SECURITY.md](./SECURITY.md) threat-model section and [DECISIONS.md](./DECISIONS.md) "v0.1 Implementation Notes".
