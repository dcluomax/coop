# AGENTS.md — Coop

> Entry point for AI coding agents working on this repo.
> Per Max's methodology: **encode boundaries in code, not prose.**
> This file is the table of contents; deep knowledge lives in linked docs.

## TL;DR for agents

- This is **Coop**, an open-source AI agent farm OS (Rust).
- Code: **Apache-2.0**. Spec: CC-BY-4.0. Assets: CC-BY-SA-4.0.
- Architecture overview: [README.md](./README.md)
- Engineering & legal decisions: [DECISIONS.md](./DECISIONS.md)
- Launch plan: [LAUNCH.md](./LAUNCH.md)
- Contributing flow (DCO, conventional commits, local dev loop): [CONTRIBUTING.md](./CONTRIBUTING.md)
- Security threat model & reporting: [SECURITY.md](./SECURITY.md)
- Changelog: [CHANGELOG.md](./CHANGELOG.md)

## ⚠️ Open-core boundary (CRITICAL)

Coop ships as **open core**. This repo (`coop`) is **public, Apache-2.0** and
contains the **farm + hens** runtime — agent OS, vault, tools, brain adapter,
farm UI, CLI. A separate **private repo (`coop-market`)** owns the
proprietary **cross-Coop market** layer (listings, bids, escrow, federation
to the World relay).

```
┌────────────────────────────────┐      ┌─────────────────────────────┐
│  coop  (PUBLIC, Apache-2.0)    │      │  coop-market  (PRIVATE)     │
│  ─────────────────────────     │      │  ───────────────────────    │
│  crates/coopd                  │◄─────┤  coopd-market (optional)    │
│  crates/coopd-core             │ path │  scripts/market-demo.sh     │
│  crates/coopd-storage          │ dep  │                             │
│  crates/coopd-vault            │      │  Sibling-checkout pattern:  │
│  crates/coopd-tools            │      │  ~/coop/                    │
│  crates/coopd-brain            │      │  ~/coop-market/             │
│  crates/coop-cli               │      │                             │
└────────────────────────────────┘      └─────────────────────────────┘
```

### Rules for agents editing this repo

1. **Never re-introduce `crates/coopd-market/` here.** It was moved on purpose.
2. **Never add `use coopd_market::...`** anywhere — the OSS daemon has zero
   awareness of the market layer. Do not add the dep back, do not add cfg-gated
   market code, do not add market types to any public API surface.
3. **Never add `coopd-market` to `[workspace] members`** in the root `Cargo.toml`.
4. **Never publish a crate to crates.io that pulls in `coopd-market`** as a
   dependency.
5. **Never reference market schemas (Listing, Bid, ListSpec, etc.) here.**
6. **Never describe market internals in public docs** (README, DECISIONS,
   CHANGELOG) beyond the open-core split announcement. Implementation details
   belong in the `coop-market` repo.
7. When in doubt about whether a change leaks proprietary surface, **STOP**
   and ask Max. Encode the boundary as a CI check rather than relying on
   docs (per Max's "taste is encoded, not described" principle).

### Verifying the boundary holds

```bash
# Must succeed (OSS-only build — there is no other supported build here)
cargo build
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

# Sanity check: this should return zero matches
grep -r "coopd_market\|coopd-market" crates/ scripts/ .github/
```

## Repository map

| Path | Owner | Notes |
|------|-------|-------|
| `crates/coopd/` | OSS | Main daemon binary; zero market awareness |
| `crates/coopd-core/` | OSS | Types, IDs, traits, error |
| `crates/coopd-storage/` | OSS | redb persistence |
| `crates/coopd-vault/` | OSS | Sealed BYOK secret store |
| `crates/coopd-tools/` | OSS | bash/file_*/http tool registry |
| `crates/coopd-brain/` | OSS | Anthropic adapter |
| `crates/coop-cli/` | OSS | `coop` CLI binary |
| `scripts/install.sh` | OSS | `curl \| sh` binary installer (platform-detecting) |
| `scripts/e2e.sh`, `scripts/farm-demo.sh` | OSS | OSS-only demos |
| `Dockerfile`, `docker-compose.yml`, `.dockerignore` | OSS | Container deploy |
| `contrib/systemd/coopd.service`, `contrib/coop.env.example` | OSS | systemd deploy |
| `examples/aria.yaml` | OSS | Starter Hen manifest |
| `docs/*.md` | OSS | Quickstart, deployment, configuration, discord |
| `../coop-market/` | **PRIVATE** | `coopd-market` crate + `market-demo.sh` |

## Development loop (per the Sprint from gstack)

```
1. UNDERSTAND → Read relevant crate's docs + CONTRIBUTING.md
2. PLAN       → Update GitHub issue with decomposition
3. EXECUTE    → Make precise surgical changes; one logical change per commit
4. SELF-REVIEW → cargo fmt + cargo clippy --workspace -- -D warnings + cargo test
5. BUILD      → cargo build --workspace (and --features market if touching coopd)
6. SHIP       → Sign off with DCO trailer, conventional commit, open PR
```

## Toolchain

- Rust: pinned to **1.91.1** via `rust-toolchain.toml`. CI uses the same
  exact pin (`dtolnay/rust-toolchain@1.91.1`) so local clippy/fmt match CI.
- Components: `rustfmt`, `clippy` (auto-installed by `rustup`).
- CI: `.github/workflows/ci.yml` (Ubuntu + macOS matrix).
- License policy: enforced by `cargo deny check licenses` (see `deny.toml`).
  Only permissive licenses (Apache-2.0 / MIT / BSD / ISC / Zlib / Unicode-3.0
  family) are allowed. **No GPL/LGPL/AGPL/MPL/SSPL** anywhere in the dep tree.

## Communication style for agents

- Follow [CONTRIBUTING.md](./CONTRIBUTING.md) conventional commit prefixes.
- Sign commits with `Signed-off-by:` (DCO required).
- When closing an issue, link the PR with `Fixes #N` in the body.
