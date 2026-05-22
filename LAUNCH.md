# Coop — Open-Source Launch Plan (v0.1.0-alpha)

> Researched & drafted by 最强大脑 + 最强创业者 review.
> Status: ready to execute pending Max's final green light on **GitHub org creation**.

---

## 1. System audit (snapshot of what we're shipping)

| Area | State | Evidence |
|---|---|---|
| **LoC** | ~4.6K Rust + ~600 bash | `wc -l crates/*/src/*.rs scripts/*.sh` |
| **Crates** | 7 (`coopd`, `coopd-core`, `coopd-storage`, `coopd-vault`, `coopd-tools`, `coopd-brain`, `coop-cli`) — `coopd-market` is **proprietary** (sibling `coop-market` repo, open-core split) | `Cargo.toml`, `AGENTS.md` |
| **Tests** | 27 unit, all green | `cargo test --workspace` |
| **E2E** | 12 / 12 checks passing on mock mode | `scripts/e2e.sh` |
| **Demos** | farm-demo 8/8 (market-demo lives in private `coop-market` repo) | `scripts/*.sh` |
| **CI** | ubuntu + macos matrix, fmt + clippy + test + doc + e2e | `.github/workflows/ci.yml` |
| Toolchain | `rust-toolchain.toml` channel = `"stable"`; workspace lints use `clippy::all` only (no `pedantic`) | `Cargo.toml`, `rust-toolchain.toml` |
| **License** | Apache-2.0 (code) + CC-BY-4.0 (spec) + CC-BY-SA-4.0 (assets) | `LICENSE-APACHE`, `NOTICE` |
| **Docs** | `README.md`, `DECISIONS.md`, this `LAUNCH.md`, `CHANGELOG.md`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md` | (committed) |
| **Brand** | Coop, hens, roost, farm, market, grain — internally consistent | code + docs |
| **Auth** | None on local API (loopback only) — documented in SECURITY.md | known limitation |
| **Demoable artifact** | `localhost:9700/` Farm UI with PTY shells | `crates/coopd/src/ui/farm.html` |

**Verdict**: production-quality scaffolding for a pre-alpha. Safe to publish.

---

## 2. Project name — decision

**Keep `Coop`.** Researched alternatives; no rename needed.

| Asset | Value | Status |
|---|---|---|
| Project / brand | **Coop** | wide-open in AI-agent space (no major project, no trademark hit in Class 9/42) |
| GitHub org | **`coop-network`** | ✅ available (verified via GitHub API) |
| Repo | `coop-network/coop` | available |
| Daemon crate | `coopd` | ✅ free on crates.io |
| Other crates | `coopd-core`, `coopd-storage`, `coopd-vault`, `coopd-tools`, `coopd-brain`, `coop-cli` | ✅ all free |
| Proprietary crate | `coopd-market` | private repo (`coop-market`); not published to crates.io |
| User-facing CLI | `coop` (binary inside `coop-cli` crate) | ✅ |
| Domain | `coop.network` (primary), `coop.io` (acquire if cheap) | defer registration to v0.3 per DECISIONS.md |

**Why not rename?** Researched `henhouse`, `coopnet`, `coop-os`, `cluck`, `roost`, `peck`, `grain` — most cute farm crate names are squat-occupied on crates.io (`cluck`, `roost`, `peck`, `grain` all taken). `coop` itself is squat-reserved on crates.io but we never publish the bare `coop` crate (we publish `coopd-*`). The brand name "Coop" is clear in the AI-agent ecosystem (verified via web search of top frameworks: AutoGPT/LangChain/CrewAI/AgentGPT/BabyAGI/AutoGen/Dify/MetaGPT — no collision).

**Risk accepted**: "coop" is short and overloaded as an English word (multiplayer games, agricultural cooperatives, generic dev jargon). Mitigated by **always pairing the wordmark with the cartoon-hen mark** (per DECISIONS.md trademark plan).

---

## 3. Pre-launch checklist (do these before flipping the repo public)

### Repo hygiene
- [x] `LICENSE-APACHE` present
- [x] `NOTICE` present
- [x] `README.md` — quickstart, concepts, architecture
- [x] `DECISIONS.md` — engineering, legal, naming
- [x] `CHANGELOG.md`
- [x] `CONTRIBUTING.md` (DCO, conventional commits, local dev loop)
- [x] `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1)
- [x] `SECURITY.md` (private advisory + threat model)
- [x] `.github/ISSUE_TEMPLATE/{bug_report,feature_request,config}.{md,yml}`
- [x] `.github/PULL_REQUEST_TEMPLATE.md`
- [x] `.github/FUNDING.yml` (commented stub)
- [x] CI passes on `main`
- [ ] README screenshot/GIF of Farm UI (do this after first push, add via PR)
- [ ] Repo description + topics set on GitHub (see §4)

### Crate metadata (before `cargo publish`)
- [ ] Each `Cargo.toml` has `description`, `keywords`, `categories`, `license = "Apache-2.0"`, `repository`, `homepage`, `readme`.
- [ ] Run `cargo publish --dry-run -p <crate>` for each in dependency order.

### Author identity
- [ ] Replace placeholder `Max <max@coop.local>` git identity with Max's real GitHub-verified email before first push.

---

## 4. GitHub setup script (run after creating the org)

```bash
# 1. Create the org coop-network at github.com/organizations/new (manual; pick "Free" plan).
# 2. Then from the local repo:

gh repo create coop-network/coop \
  --public \
  --source=. \
  --description "🐔 An open-source, distributed AI agent farm OS. Raise, train, and trade autonomous agents on your own hardware." \
  --homepage "https://coop.network" \
  --push

# 3. Topics for discoverability:
gh repo edit coop-network/coop --add-topic \
  ai-agents,agent-framework,rust,distributed-systems,llm,anthropic,claude,raspberry-pi,multi-agent,agentic,self-hosted,homelab,websocket,axum,tokio

# 4. Enable Discussions:
gh repo edit coop-network/coop --enable-discussions
gh repo edit coop-network/coop --enable-issues
gh repo edit coop-network/coop --enable-projects

# 5. Branch protection on main:
gh api -X PUT "repos/coop-network/coop/branches/main/protection" --input - <<'JSON'
{
  "required_status_checks": {"strict": true, "contexts": ["ci"]},
  "enforce_admins": false,
  "required_pull_request_reviews": null,
  "restrictions": null,
  "allow_force_pushes": false,
  "allow_deletions": false
}
JSON

# 6. (Optional) Create a "good first issue" + "help wanted" label set
gh label create "good first issue" --color "7057ff" --description "Friendly entry-point tasks"
gh label create "help wanted" --color "008672" --description "We'd love a hand on this"
gh label create "area/L1" --color "0e8a16" --description "coopd / agent runtime"
gh label create "area/L2" --color "1d76db" --description "World relay / federation"
gh label create "area/L4" --color "fbca04" --description "Farm UI / game layer"
```

---

## 5. v0.1.0-alpha release

```bash
# tag from main once CI is green
git tag -s v0.1.0-alpha -m "Coop v0.1.0-alpha — ALONE FARMER"
git push origin v0.1.0-alpha

gh release create v0.1.0-alpha \
  --title "v0.1.0-alpha — ALONE FARMER" \
  --notes-file <(awk '/^## \[0\.1\.0-alpha\]/,/^## \[/' CHANGELOG.md | sed '$d')
```

Crate publication order (dependency-first):
1. `coopd-core`
2. `coopd-storage`, `coopd-vault`, `coopd-tools`, `coopd-brain` (parallel)
3. `coopd` (with `default-features` only — market feature is off by default)
4. `coop-cli`

> **Note:** `coopd-market` is **NOT** published to crates.io (open-core proprietary).
> See [AGENTS.md](./AGENTS.md) for the boundary contract.

---

## 6. Launch-day marketing checklist

- [ ] **Show HN** post — title: `Show HN: Coop – a Rust agent OS for raising AI hens on your own hardware`. Lead with the Farm UI GIF + "5-minute quickstart".
- [ ] **r/rust** post — focus on the architecture (8 crates, axum, tokio, redb, portable-pty, sealed vault).
- [ ] **r/selfhosted** post — focus on "run on a Pi, click a hen, get a terminal".
- [ ] **X / Twitter thread** — 5-tweet teardown ending with repo link.
- [ ] **dev.to article** — "Why we open-sourced an AI agent farm before the federation was ready" (links to coop-substrate-v1.md moat thesis).
- [ ] **Hacker News alternative submission timing**: Tue-Thu, 9am Pacific.

---

## 7. The one decision still needing Max

> **Create the `coop-network` GitHub org and confirm Max wants to be the owner?**
>
> Verified available via GitHub API today. Alternative orgs available if Max prefers: `coop-os`, `coop-farm`. (`coopnet`, `coop-ai`, `henhouse` are all taken.)

Everything else is decided. Once Max says "go", I execute §3 → §4 → §5.
