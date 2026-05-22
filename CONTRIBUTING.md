# Contributing to Coop

Coop is pre-alpha and **moving fast**. The fastest way to help right now:

1. **Try the quickstart** in [README.md](./README.md) and tell us what broke.
2. **Run `./scripts/e2e.sh`** on your machine and report any failing step.
3. **Pick a `good-first-issue`** label on the tracker.

## Ground rules

- **DCO sign-off** required on every commit (`git commit -s`). We use [Developer Certificate of Origin](https://developercertificate.org/) instead of a CLA for friction-free contribution. A CLA may be required once the foundation entity exists (see [DECISIONS.md](./DECISIONS.md)).
- **Conventional commits** preferred (`feat:`, `fix:`, `docs:`, `chore:`, `refactor:`, `test:`).
- **All PRs must pass CI** (`cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`, `cargo doc`, e2e on ubuntu+macos).
- **No new dependencies without rationale.** Add a line to the PR description for every new crate pulled in.
- **No `unsafe`** in the Rust workspace without a `// SAFETY:` comment and reviewer approval.

## Local dev loop

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test --workspace
./scripts/e2e.sh         # 12-step end-to-end (mock mode, no API key needed)
./scripts/farm-demo.sh   # multi-hen farm demo
./scripts/market-demo.sh # market lifecycle demo
```

## Architecture orientation

- **Read in order**: [README.md](./README.md) → [DECISIONS.md](./DECISIONS.md) → `crates/coopd-core/` → `crates/coopd/src/orchestrator.rs` → `crates/coopd/src/runner.rs`.
- Crate layering (top can import from below, never reverse):
  ```
  coopd      ← coop-cli
    │
    ├─ coopd-brain   ┐
    ├─ coopd-tools   ├─ all import coopd-core
    ├─ coopd-market  │
    ├─ coopd-storage │
    └─ coopd-vault   ┘
       └────────────── coopd-core (types, traits)
  ```

## What we want help with (v0.1 → v0.2)

- 🐧 **Linux ARM builds** (Raspberry Pi 4/5 + Pi Zero 2 W targets).
- 🧠 **Brain adapters**: OpenAI, local llama.cpp, Claude CLI, Codex CLI.
- 🧰 **Tools**: `git`, `sleep`, `log`, sandboxed `bash` via `nsjail`/`firejail`.
- 🌐 **L2 World relay** (Cloudflare Workers + D1) — see `world-protocol.md` in the design docs.
- 🎨 **Farm UI**: animated hens (idle/working/sleeping sprites), drag-to-reorder, dark mode.
- 📦 **Packaging**: Homebrew tap, `cargo-binstall`, `.deb`, OCI image.
- 📚 **Docs**: per-crate `lib.rs` examples, mdBook for the protocol spec.

## Filing issues

Please use the [issue templates](./.github/ISSUE_TEMPLATE). For security reports, see [SECURITY.md](./SECURITY.md) — **do not** open a public issue.

## Code of Conduct

By participating you agree to the [Code of Conduct](./CODE_OF_CONDUCT.md).
