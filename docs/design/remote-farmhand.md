# Design: Farmhand — remote monitor & steer

> Status: **accepted / phase-0 landed** · Layer: **L2 Federation** · Tracking: see the
> "Farmhand" GitHub issue.
>
> Phase-0 (the `coopd-core::remote` seam — trait, schema, loopback reference
> bridge) is implemented. Phases 1–3 are planned below.

## Problem

Coop's hens are **long-running autonomous agents**, and the operator usually
walks away from the Pi/Mac/box they run on. Two things then go wrong:

1. A hen hits a **fail-closed gate** (a tool/path/URL permission request, or a
   plan-mode approval) and **blocks**, waiting for a human who isn't at the
   keyboard.
2. The operator has **no way to see the flock** once they leave the LAN — the
   Farm UI is bound to `127.0.0.1:9700`.

GitHub Copilot CLI solves the single-session version of this with its `/remote`
feature (monitor & steer a running CLI session from github.com / mobile).
**Farmhand** is the farm-scale analogue for Coop: watch the whole flock and
unblock or steer any hen, from any device.

## Why it fits Coop

| Coop already has | The gap | Farmhand adds |
|---|---|---|
| Farm UI (local `127.0.0.1:9700`) | invisible once you leave the LAN | reach the flock from any device |
| Per-hen fail-closed permission / egress gates | a gate with no human present just blocks | escalate the gate to your phone; approve/deny remotely |
| Discord connector (one channel per hen) | one-way notifications, can't steer | two-way steer (the evolution of that channel) |
| L2 Federation (`world.coop` relay, planned) | no concrete first use-case | the relay's first killer app |

## Non-negotiable constraints

- **No mandatory cloud.** Coop's identity is self-hostable. The relay must be
  self-hostable, and the *trait + reference relay* must be OSS.
- **Outbound only.** The daemon dials *out* to a relay; it never opens an
  inbound port. This is NAT/Pi-friendly and consistent with `safe_origin` /
  `COOP_PUBLIC`. (Same shape Copilot uses: the CLI polls GitHub.)
- **Zero-knowledge relay (target).** Events/commands are end-to-end encrypted
  between daemon and operator device; the relay is a dumb pipe and never sees
  session content or model keys — even when hosted. This is the core
  differentiator vs. a vendor-hosted control plane.
- **Fail-open side channel.** Unlike the network egress policy (fail-*closed*:
  a hen that can't enforce its policy refuses to hatch), the bridge is a
  bypass. A dead/slow/misconfigured relay must **never** block local
  execution. The local terminal stays fully authoritative; bridge errors are
  logged and ignored.

## Architecture

```
  hen ──events──▶ coopd ──[E2E ciphertext]──▶ relay (dumb pipe) ──▶ phone / web
   ▲                                          relay can't read it      │
   └──────────────── remote command (ciphertext) ◀──────────────────┘
```

- A daemon-side **`RemoteBridge`** (outbound) publishes [`FarmEvent`]s and polls
  for [`RemoteCommand`]s.
- The relay only stores/forwards opaque envelopes.
- Both the local terminal and the remote interface are live at once;
  **first response wins** (mirrors Copilot's local+remote concurrency model).

### Three-tier posture (`RemoteMode`)

Mirrors the `off` / `allowlist` / `open` shape of the network policy:

| mode      | publishes events | accepts commands |
|-----------|------------------|------------------|
| `off`     | no               | no               |
| `view`    | yes (read-only)  | no               |
| `control` | yes              | yes (steer)      |

Default `off` — opt-in, like every egress surface in Coop.

### What you can do remotely (control mode)

Approve/deny permission gates · answer questions · approve/reject plans ·
submit new prompts · cancel the current operation · switch a hen's session
mode. (Slash-command-style admin actions stay local, as in Copilot.)

## Open-core split

| Piece | Home | License |
|---|---|---|
| `RemoteBridge` trait + event/command schema + `LoopbackBridge` reference + a self-hostable reference relay | **this repo** | Apache-2.0 |
| Hosted multi-tenant relay-as-a-service (push, mobile app, accounts) | separate offering | proprietary |

This is **Federation (L2)** — wholly distinct from the **Market**. No market
schema (`Listing`/`Bid`/…) is referenced, and the boundary CI grep stays clean.

## Phasing

- **P0 — seam (DONE).** `coopd-core::remote`: `RemoteMode`, `RemoteSpec` +
  validation, `RemoteBridge` trait, `FarmEvent` / `RemoteCommand` schema,
  `LoopbackBridge` reference impl. Pure, no I/O, fully unit-tested on macOS.
- **P1 — local wiring + gate escalation.** Wire a bridge into the daemon
  (config via `COOP_REMOTE_MODE` / `remote:` settings); publish flock/state
  events; route the existing permission gates through `FarmEvent::PermissionRequested`
  and resolve them from `RemoteCommand`. Reference relay over the loopback +
  a simple self-hostable HTTP relay. Keep-alive helper to stop the box sleeping.
- **P2 — E2E pairing + push.** QR-code device pairing carrying a shared key;
  encrypt envelopes (reuse `blake3` / `ed25519-dalek` already in the dep tree);
  `world.coop` relay; reuse the Discord channel for push alerts.
- **P3 — mobile.** Flock view + gate approvals from a phone.

## Failure modes (Gate-1 self-review)

- **Relay down / network blip** → hens keep running locally; reconnect resumes
  the stream (Copilot-style). Bridge calls are best-effort.
- **Replay / spoofed commands** → commands carry a nonce + monotonic sequence;
  envelopes are signed (P2).
- **Two writers** (local terminal + remote) issue conflicting input →
  first-response-wins.
- **Privacy** → `remote: off|view|control` bounds exposure the way
  `network: off|allowlist|open` bounds egress; relay is zero-knowledge.
- **Bridge fails to start** → fail-*open* (log + continue); it must NOT block a
  hen from hatching (the one deliberate inversion of Coop's fail-closed
  default, justified because this is a side channel, not an enforcement point).

## Prior art

- GitHub Copilot CLI `/remote` — monitor & steer a single CLI session from
  github.com / GitHub Mobile (outbound polling, local+remote concurrency,
  `keep-alive`). Farmhand generalises this to a whole flock and keeps the relay
  zero-knowledge and self-hostable.
