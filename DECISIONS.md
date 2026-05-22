# Coop — v0.1 Decisions Record

Autonomous decisions taken to start v0.1 implementation without further blocking on the user.

## Scope: v0.1 "ALONE FARMER"

In:
- `coopd` single binary, **single-Roost only** (no Worker mode)
- `coop-cli` CLI
- Hen lifecycle: `DEFINED → IDLE → WORKING → IDLE → SLEEPING`
- 1 brain adapter: **Anthropic (BYOK)**
- Built-in tools: `bash`, `file_read`, `file_write`, `http`, `git`, `sleep`, `log`
- Local HTTP API on `localhost:9700`
- redb persistence
- Sealed vault (sodiumoxide, passphrase-derived key)
- Reconciler loop
- Structured logging (`tracing`)
- Subprocess Hen runtime (UNIX socket JSON-RPC)
- agent.yaml schema + validation
- Unit + integration tests

Out (deferred to ≥ v0.2):
- World cloud (`coopd-world` crate not started)
- Worker mode / gRPC mesh / WireGuard
- Lease / Grain ledger / Pecking Order
- Animated UI (`coop-cc`)
- Cluck messaging
- Container runtime (`youki`)
- WASM tool plugins
- Multi-brain (OpenAI / local / Claude CLI)

## Engineering Decisions

| Topic | Choice | Reason |
|---|---|---|
| Language | Rust | Already decided in L1 doc |
| Edition | 2024 | Latest stable |
| MSRV | 1.85+ | edition 2024 requirement |
| Async runtime | Tokio multi-thread | Industry standard |
| Workspace | Cargo workspace, 5 crates v0.1 | Modular, parallel build |
| HTTP framework | axum 0.7 | Ergonomic, tokio-native |
| Storage | redb 2.x | Pure-Rust, ACID, embedded |
| Crypto | ed25519-dalek + blake3 + sodiumoxide | Standard, well-audited |
| Serialization | serde + serde_json + serde_yaml + prost | One-stop |
| CLI | clap 4 (derive) | Standard |
| Error | thiserror (libs) + anyhow (bins) | Idiomatic split |
| Logging | tracing + tracing-subscriber | Structured, async-friendly |
| Testing | cargo test + criterion (bench) | Stdlib + standard bench |
| Container | Subprocess only in v0.1 | youki deferred to v0.2 |
| Style | rustfmt default + clippy `-D warnings` | Enforce hygiene |
| Metrics | deferred (Prometheus to v0.2) | Not needed solo |

## Legal / Licensing — research-backed (2025-11)

Following the legal research agent's findings (FinCEN, EU EUR-Lex, Steam ToS, USPTO,
OSI, Apache, IRS precedents):

| Topic | Decision | Reason |
|---|---|---|
| **Core code license** | **Apache-2.0** | Patent grant + termination, max enterprise adoption, CNCF precedent |
| **Relay infrastructure license** | **AGPL-3.0** *(when world.coop crate lands in v0.3)* | Prevents cloud SaaS hijack of relay; Mongo/Grafana precedent |
| **Spec docs license** | **CC-BY-4.0** | Spec free to copy with attribution |
| **Asset license** | **CC-BY-SA-4.0** | Sprites/avatars protected from rebrand takeover |
| **CLA** | Required for all contributions | Preserves foundation's relicensing rights when entity formed |
| **Grain legal model** | **V-Bucks model (closed-loop, no cash-out, non-transferable)** | Avoids US MTL; Steam Wallet/Robux DevEx analysis |
| ↳ ToS language | "Grain has no cash value, not a payment instrument" | Mirrors Steam Subscriber Agreement |
| ↳ Marketplace fee | Agent-of-payee structure | Same model as Stripe/Airbnb/Uber |
| ↳ EU notification | When > €1M/year transactions | Limited Network Exemption under PSD2 |
| **Foundation entity** | **Swiss Stiftung in Canton Zug** (target ≥ v1.0) | Ethereum Foundation precedent; protocol+grants+trademark+Grain operations under one roof; FINMA utility-token classification |
| ↳ Setup cost | ~CHF 95k year 1 (CHF 50k endowment + CHF 20k legal + CHF 25k ops) | Deferred until Grain or external $ exists |
| ↳ Runner-up | US 501(c)(6) trade association | Accept UBIT tax on Grain sales |
| **Trademark** | **Composite mark (COOP + cartoon hen)**, USPTO Classes 9 & 42, file in v0.4 | Stronger than word mark alone; ~$4-7k incl. attorney |
| ↳ Word mark | "COOP PROTOCOL" as separate word mark | More distinctive than bare "COOP" |
| ↳ Madrid Protocol | EU, UK, JP, CA, AU (+$6-10k) | Cover top dev markets |
| ↳ Risk | EU retail cooperatives may oppose | Different classes — likely fine; freedom-to-operate search before filing |
| **Domain strategy** | **coop.network primary; coop.io if acquirable** | `.coop` TLD requires cooperative-org registration; do **not** depend on it |
| **Name collision** | Clear in AI/agent/devtools space | No competitor uses Coop branding |

**Action items deferred to ≥ v0.4 (not blocking v0.1):**
- File USPTO composite mark
- Engage California fintech counsel before any US Grain sale
- Acquire coop.io domain (~$5-50k)
- Draft CLA (use Apache 2.0 CLA template)
- Draft Terms of Service with Steam-style language

## Repo Layout

```
coop/
├── Cargo.toml                # workspace
├── rust-toolchain.toml       # pinned stable
├── rustfmt.toml
├── clippy.toml
├── DECISIONS.md              # this file
├── LICENSE-APACHE
├── NOTICE
├── README.md
├── .github/workflows/        # CI
├── crates/
│   ├── coopd/                # main binary
│   ├── coopd-core/           # orchestrator, hen mgr, scheduler
│   ├── coopd-storage/        # redb persistence
│   ├── coopd-vault/          # sealed creds
│   └── coop-cli/             # CLI binary
└── docs/
    └── ... (linked from session-state/files copies)
```

Crates deferred to later phases:
- coopd-brain (lifted from brain trait, v0.2 multi-provider)
- coopd-tools (plugin loader, v0.2)
- coopd-world (WSS to World, v0.3)
- coopd-mesh (worker gRPC, v0.2)
- coopd-ledger (Grain, v0.4)
- coopd-runtime (container, v0.2)
- coopd-proto (protobuf, deferred until cross-binary need)

## Open Questions for Max (non-blocking; default chosen)

- **GitHub org name**: default to `coop-network` if available, else `coop-ai`, else create under personal.
- **Repo public/private**: default **public** from day 1 (build-in-public).
- **Foundation timing**: assume not before v1.0 unless Max accelerates.
- **Domain registrations**: defer until v0.3 (when World service begins).

## v0.1 Implementation Notes (added during build)

- **Open-core split: `coopd-market` is proprietary.** The cross-Coop market crate
  (listings, bids, escrow) lives in a separate **private** repo, `coop-market`,
  and is wired into the OSS daemon via an optional Cargo feature
  (`--features market`) with a path dependency to a sibling checkout. This
  preserves the Coop substrate as fully usable open source ("raise, train, and
  run hens on your own hardware") while keeping the monetization layer — the
  thing that funds the project — proprietary. Rationale: the open core is the
  agent OS people self-host; the market is the federation/payments layer that
  becomes a SaaS business at scale. OSS users get a complete single-farm
  experience; commercial users license the market crate.
  - Architectural contract: see [AGENTS.md](./AGENTS.md) — code in this repo
    MUST NOT contain market types, payment logic, or any reference that leaks
    `coop-market` internals.
- **In-process tools (no subprocess sandbox).** v0.1 runs `bash` / `file_*` / `http` directly in coopd's tokio runtime. This is acceptable for the single-trust-domain "alone farmer" milestone. v0.2 will introduce subprocess + container isolation per tool — see [coop-l1-os] design doc.
- **Reason loop encodes tool results as plaintext user messages.** v0.1 does not use Anthropic's structured `tool_use` / `tool_result` content blocks. This works for short flows but loses fidelity on multi-turn conversations. Upgrade in v0.2.
- **Single startup reconciler, no periodic tick.** On boot, any Job left `RUNNING` is marked `FAILED("interrupted at restart")` and any Hen left `Hatching`/`Working` is forced back to `Idle`. There is no periodic reconciliation tick — only at process start.
- **Brain factory resolves only `vault:<secret-name>` provider IDs.** Any other `brain.provider_id` is rejected. v0.2 will add registry-style adapters.
- **Vault auto-unlock from env.** If `COOP_VAULT` and `COOP_PASSPHRASE` are both set, the vault unlocks at startup. Otherwise it remains locked until `POST /api/v1/vault/unlock`.
- **MAX_TURNS = 16** per job in the reason loop. Hard-coded safety cap for v0.1.
- **`coopd-tools` and `coopd-brain` crates exist now** (previously listed as deferred). Adapter trait still lives in `coopd-core::brain`.

## Security: third-party advisories

- **`rustls-webpki 0.102.8` advisories (RUSTSEC-2026-0049/0098/0099/0104)
  are ignored in `deny.toml` pending a serenity upgrade.** They reach us
  transitively from `serenity 0.12.5 → tokio-tungstenite 0.21 → rustls 0.22`,
  and serenity 0.12.5 is the latest published version. The vulnerable code
  path is only exercised by the *optional* Discord connector when it validates
  the Discord WebSocket's TLS certificate; the main Anthropic path uses
  `reqwest → rustls 0.23 → rustls-webpki 0.103` which is not affected.
  Operators who do not need Discord should leave the connector unconfigured.
  We will lift the ignores as soon as serenity 0.13 (or a maintained fork)
  ships against modern rustls.
- **`portable-pty` bumped 0.8 → 0.9** to drop the unmaintained `serial`
  crate (RUSTSEC-2017-0008). No code changes required.
- **`cargo deny check advisories` is now wired into CI** (alongside licenses,
  bans, sources) so any new advisory against our dep tree fails the build
  unless it is explicitly added to the `[advisories].ignore` list with a
  written justification here.
