<div align="center">

# 🐔 Coop

### *Raise, train, and trade autonomous AI agents on your own hardware.*

[![CI](https://github.com/dcluomax/coop/actions/workflows/ci.yml/badge.svg)](https://github.com/dcluomax/coop/actions/workflows/ci.yml)
[![Release](https://github.com/dcluomax/coop/actions/workflows/release.yml/badge.svg)](https://github.com/dcluomax/coop/actions/workflows/release.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE-APACHE)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg?logo=rust)](https://www.rust-lang.org)
[![Status](https://img.shields.io/badge/status-pre--alpha-orange.svg)](./CHANGELOG.md)

**[Quickstart](#-quickstart)** ·
**[Deploy](#-deploy)** ·
**[Architecture](#-architecture)** ·
**[Docs](#-docs)** ·
**[Contribute](./CONTRIBUTING.md)**

![Coop Farm UI — flock view](docs/screenshots/01-farm-overview.png)

</div>

---

**Coop** is an open-source AI agent farm OS in Rust. Run autonomous AI agents —
**Hens** — on a Raspberry Pi, a Mac, a Windows box, or a fleet of cloud nodes.
One static binary: no Python, no Docker required.

> 🚧 **Pre-alpha** — `v0.1 "ALONE FARMER"`. See [DECISIONS.md](./DECISIONS.md)
> for scope and [CHANGELOG.md](./CHANGELOG.md) for what shipped.

## ✨ Why Coop?

|   |   |
|---|---|
| 🏡 **Self-hosted** | Your hardware, your agents, your data. No mandatory cloud. |
| 🔐 **BYOK vault** | Sealed `xchacha20poly1305` vault for your model keys, or pull them from **Azure Key Vault**. Locked at rest. |
| 🤖 **Real autonomy** | Hens get a sandboxed PTY shell, a tool ABI, and an Anthropic brain. |
| 🧱 **Per-hen isolation** | Each hen runs in its own OS sandbox (macOS Seatbelt / Linux Bubblewrap) — confined to its workdir, scrubbed env, siblings unreadable. |
| 🌐 **Network egress policy** | Per-hen `network:` block — `off` / `allowlist` / `open`. Fail-closed: a hen that can't enforce its policy refuses to hatch. |
| 🖥️ **Live shell** | Click any hen in the Farm UI to drop into a real terminal in its workdir. |
| 🍓 **Runs on a Pi** | First-class binaries for Raspberry Pi 3/4/5 + Pi Zero 2. |
| ⚡ **One static binary** | `coopd` daemon + `coop` CLI. No runtime deps. |

## 🚀 Quickstart

```bash
# 1. install coopd + coop
curl -fsSL https://raw.githubusercontent.com/dcluomax/coop/main/scripts/install.sh | sh

# 2. start the daemon
coopd serve &

# 3. watch your flock
open http://127.0.0.1:9700/      # Farm UI
coop hen list
```

Defining a Hen, sealing your model key, and running your first job →
**[docs/quickstart.md](./docs/quickstart.md)**.

Prefer source? `git clone … && cargo build --release` (Rust 1.85+), binaries land
in `target/release/`.

## 📦 Deploy

| Target | Command |
|--------|---------|
| 🐳 **Docker** | `docker compose up -d` ([compose](./docker-compose.yml)) |
| 🛠️ **systemd** | `contrib/systemd/coopd.service` (24/7 bare metal) |
| 📥 **Binaries** | [latest release](https://github.com/dcluomax/coop/releases/latest) — 7 platforms, SHA-256 checksums |

Full guide, including LAN/public exposure and the **required** `COOP_API_TOKEN`
+ `COOP_PUBLIC` settings → **[docs/deployment.md](./docs/deployment.md)**.

## 🏛️ Architecture

Coop is organised into four conceptual layers — **this repo is L1** (the agent OS).

```
┌─────────────────────────────────────────────────────────────┐
│  L4  Game           XP · leaderboards · spectator · UI      │
│  L3  Economic       Grain ledger · hen/roost lease · escrow │
│  L2  Federation     world.coop relay · registry · mailbox   │
│  L1  Coop OS        coopd · brain adapter · tool ABI · vault│  ← this repo
└─────────────────────────────────────────────────────────────┘
```

**Workspace (7 OSS crates):** `coopd` (daemon — HTTP/WS API, orchestrator) ·
`coopd-core` (types/traits) · `coopd-storage` (redb) · `coopd-vault` (sealed
BYOK) · `coopd-tools` (`bash`/`file_*`/`http`) · `coopd-brain` (Anthropic
adapter) · `coop-cli` (the `coop` binary).

## 🖥️ Farm UI

Open <http://127.0.0.1:9700/>. The single-page UI lists every hen with a live
state badge and lets you **click a hen to open a real terminal** streamed over
WebSocket directly into that hen's workdir — drive `claude login` / `gh auth
login`, inspect generated files, or troubleshoot a stuck job.

<table>
  <tr>
    <td width="50%">
      <a href="docs/screenshots/01-farm-overview.png"><img src="docs/screenshots/01-farm-overview.png" alt="Agents tab" /></a>
      <p align="center"><em>🐔 <b>Agents</b> — the flock, live state badges.</em></p>
    </td>
    <td width="50%">
      <a href="docs/screenshots/03-sessions.png"><img src="docs/screenshots/03-sessions.png" alt="Sessions tab — live PTY shell" /></a>
      <p align="center"><em>🖥 <b>Sessions</b> — live PTY into a hen's workdir.</em></p>
    </td>
  </tr>
</table>

## 📚 Docs

| Doc | What |
|-----|------|
| [Quickstart](./docs/quickstart.md) | Install → vault → first Hen → first job |
| [Deployment](./docs/deployment.md) | Docker · Compose · systemd · LAN/public |
| [Configuration](./docs/configuration.md) | Every environment variable |
| [Network isolation](./docs/net-isolation.md) | Per-hen sandbox + egress policy (`off`/`allowlist`/`open`) |
| [Discord connector](./docs/discord.md) | One channel per chicken |
| [Decisions](./DECISIONS.md) · [Launch](./LAUNCH.md) · [Changelog](./CHANGELOG.md) | Roadmap & rationale |
| [Security](./SECURITY.md) | Threat model & private advisory flow |

## 🔓 Open-core split

Coop ships as **open core**. This repo (Apache-2.0) is the **farm + hens**
runtime — agent OS, vault, tools, brain adapter, Farm UI, CLI — fully usable on
its own for single-farm and single-hen workflows. The cross-Coop **Market**
layer (listings, bids, escrow, federation to the World relay) is a separate
**proprietary component** in a private repo; the OSS daemon has **zero** market
awareness. Need market functionality? Reach out to the maintainer.

## 🤝 Contributing

We're pre-alpha and **moving fast**. Dev loop, DCO sign-off, and commit style →
[CONTRIBUTING.md](./CONTRIBUTING.md). AI coding agent? → [AGENTS.md](./AGENTS.md).
By participating you agree to the [Code of Conduct](./CODE_OF_CONDUCT.md).

## 📜 License

| Surface | License |
|---------|---------|
| Code | [Apache-2.0](./LICENSE-APACHE) + [NOTICE](./NOTICE) |
| Spec docs | CC-BY-4.0 |
| Assets | CC-BY-SA-4.0 |

<div align="center">

---

Built with 🦀 + 🐔 by farmers, for farmers.

</div>
