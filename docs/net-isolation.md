# Per‑Hen Network Isolation (Forced‑Egress) — Architecture Spec

> Status: **v1 SHIPPED (Linux off + macOS off/allowlist + http-tool allowlist
> + fail-closed hatch) / forced-egress proxy = follow-up.** This doc is the
> design + threat model; §9 marks the exact v1 scope. Owner: blue-team /
> sandboxing.
>
> **What shipped (v1):** the `network:` manifest block (`off`/`allowlist`/
> `open`); the in-process `http` tool L7 host+port allowlist; the `bash`/tmux
> OS sandbox denying **all direct egress** for `off`/`allowlist`
> (Linux `bwrap --unshare-net` empty netns; macOS Seatbelt `(deny network*)`);
> capability probe + **fail-closed refuse-to-hatch** for unenforceable strict
> policies and for tmux CLI agents. **Deferred:** the Linux forced-egress proxy
> that gives *bash itself* allow-listed egress (under v1 `allowlist`, bash gets
> no direct egress — allow-listed egress is via the `http` tool only), SNI
> re-verification, `pasta` NAT for `open`, and sentinel-token secret injection.
>
> Open-core note: everything here lives in the **public** `coop` repo. No
> reference to the market layer. The forced-egress proxy is a daemon-local
> security control, not a federation feature.

## 0. Problem & design thesis

Coop's advantage over VM‑per‑agent systems (e.g. an OpenComputer‑style
bare‑metal QEMU farm) is that it is **lightweight and self‑hostable**: many
hens share one host. The cost is that "network isolation" cannot lean on a
hypervisor. We must give each hen an **independent egress restriction** on a
single Linux/macOS host **without a VM**.

The thesis the rest of this doc defends:

> **Do not rely on advisory controls (env‑var proxies, SSRF checks in one
> tool) as the enforcement boundary. Enforce default‑deny at L3/L4 with a
> kernel network namespace, and make the *only* reachable egress path a
> coopd‑owned proxy that applies the per‑hen L7 allowlist.** Where the OS
> cannot do this (macOS), be honest and fail **closed / stricter**, never
> silently weaker than advertised.

### 0.1 Two egress surfaces (critical framing)

Coop has **two independent ways a hen reaches the network**, and they need
different enforcement:

| Surface | Where it runs | Today's control | New control |
|---|---|---|---|
| **`http` tool** | **in‑process, in `coopd`** (see `crates/coopd-tools/src/http.rs`) | `safe_net::validate_url` (SSRF, L7) | + per‑hen **allowlist** check, in‑code, **portable** |
| **`bash` tool + tmux CLI agents** | **child process** (`crates/coopd-tools/src/sandbox.rs`; tmux agents via `shell.rs`/`session.rs`) | env‑scrub + fs sandbox; **no network restriction** | **OS network sandbox** (Linux netns forced‑proxy; macOS seatbelt) |

The `http` tool already runs with coopd's privileges and is trivially
constrained in Rust. The hard problem is the **bash/tmux subprocess**, which
can run `curl`, `nc`, raw sockets, direct‑IP connects, `curl --noproxy '*'`,
etc. That subprocess is where the netns work goes.

> **v1 completeness requirement:** tmux‑hosted CLI agents (`claude-code`,
> `codex`, `gh-copilot`, `shell` — see `AgentKind` in
> `crates/coopd-core/src/manifest.rs`) are a network egress surface **equal to
> bash**. They MUST run under the same per‑hen netns/seatbelt profile, or the
> policy is bypassable by simply using a tmux agent. If v1 cannot wrap tmux
> agents yet, hens with `agent_kind != anthropic` **must fail closed** under
> any policy stricter than `open` (refuse to hatch). See §6.

---

## 1. Policy model (manifest)

A new optional top‑level `network:` block in `agent.yaml`
(`spec_version: coop/v1`). Absent block = backward‑compatible default (§1.3).

### 1.1 YAML shape

```yaml
spec_version: coop/v1
name: aria
brain:
  provider_id: vault:byok-anthropic
  model: claude-sonnet-4-5-20250929
tools: [bash, file_read, file_write, http]

network:
  policy: allowlist          # off | allowlist | open   (REQUIRED if block present)
  allow:                     # only meaningful when policy == allowlist
    - host: api.anthropic.com           # exact host match
      ports: [443]                       # optional; default [443]
    - host: "*.githubusercontent.com"   # suffix-wildcard (matches sub.a.githubusercontent.com)
      ports: [443]
    - host: example.com
      ports: [80, 443]
```

### 1.2 Field semantics

- **`policy`** (enum, required when `network:` present):
  - `off` — **no egress at all** for this hen (neither bash nor http tool).
  - `allowlist` — egress permitted **only** to hosts/ports in `allow`,
    enforced at L7 by the proxy + the http tool. Default‑deny.
  - `open` — unrestricted egress; the http tool still applies `safe_net`
    SSRF protection (loopback/RFC1918/link‑local/etc. always blocked), bash
    gets NAT'd internet (Linux) / unrestricted sockets (macOS).
- **`allow[].host`** matching rules (exact, deterministic, case‑insensitive,
  IDNA/punycode‑normalised before compare):
  - **Exact**: `api.anthropic.com` matches only that host.
  - **Suffix wildcard**: a single leading `*.` — `*.example.com` matches
    `a.example.com` and `a.b.example.com` but **not** the apex `example.com`
    (add an explicit `example.com` entry if you want the apex). No other
    wildcard forms (`*`, `foo.*.com`, mid‑label `*`) are accepted; reject at
    validate time.
  - IP literals are allowed as hosts (exact), but are still subject to the
    `safe_net::is_disallowed_ip` block — so private/loopback literals are
    rejected even if listed.
- **`allow[].ports`** (optional `Vec<u16>`): if omitted, defaults to `[443]`.
  A connection is allowed iff **host matches AND port ∈ ports**. There is no
  port wildcard in v1 (enumerate them).
- Protocol scope: allowlist governs **TCP**. UDP/QUIC/ICMP get **no egress**
  under `off`/`allowlist` (empty netns has no UDP path either). `open` permits
  whatever the OS path allows. (HTTP/3 to allowed hosts is a follow‑up; for v1
  allowed hosts are reached over TCP/TLS via the proxy's `CONNECT`.)

### 1.3 Default & backward compatibility

- **Recommended authored default** (templates, `coop hen create`,
  `examples/aria.yaml`): `policy: allowlist` with an explicit `allow` list.
- **Absent `network:` block** = `open`, emitted with a **one‑time deprecation
  warning** ("hen X has no network policy; defaulting to open egress; this
  default becomes `allowlist` in coop/v2"). Rationale: existing manifests
  (`examples/aria.yaml`) ship `http` + `bash` and must keep working; flipping
  silently to deny would break every current hen. The fail‑closed posture is
  applied to *enforceability* (§6), not to *default selection*.
- **coop/v2 (follow‑up):** flip absent‑default to `allowlist` (deny‑all) and
  require operators to opt into `open`.

### 1.4 Core types (where they live)

`crates/coopd-core/src/manifest.rs`:

```rust
pub struct AgentManifest { /* … */ pub network: Option<NetworkSpec>, }

#[serde(rename_all = "lowercase")]
pub enum NetPolicy { Off, Allowlist, Open }

pub struct NetworkSpec {
    pub policy: NetPolicy,
    #[serde(default)] pub allow: Vec<NetAllow>,
}
pub struct NetAllow {
    pub host: String,
    #[serde(default = "default_https_port")] pub ports: Vec<u16>,
}
```

`validate()` additions (reject at manifest load, fail closed):
- `policy: allowlist` with empty `allow` is **legal** = deny‑all‑but‑nothing
  (effectively `off` for egress; still distinct because the proxy/socket is
  provisioned). Warn that no hosts are reachable.
- `policy: off`/`open` with a non‑empty `allow` → error (`allow` only valid
  for `allowlist`).
- Each `host`: reject empty, reject embedded scheme/`/`/`:`/spaces, reject
  multi‑`*` or non‑leading `*`. Normalise to lowercase punycode.
- Each port `!= 0`.

---

## 2. Linux enforcement (primary, the real boundary)

### 2.1 Option comparison (decided)

| Option | What it gives | Why / why not |
|---|---|---|
| **(a) `bwrap --unshare-net` (empty netns) + forced proxy over a bind‑mounted unix socket** | Brand‑new netns with only its own `lo`. **Zero** external interface ⇒ default‑deny at L3/L4 by construction. Only egress = a coopd‑owned unix socket. | **CHOSEN for `off` and `allowlist`.** Strongest: bypass attempts (raw sockets, `--noproxy`, direct‑IP) fail because **there is no route to anywhere**. The proxy is the sole L7 chokepoint. |
| (b) `slirp4netns`/`pasta` with filtering | User‑mode NAT egress to the real internet; `pasta` can restrict ports but **not** host/SNI. | Gives real internet by default ⇒ wrong primitive for allowlist (you'd be filtering an open door). **CHOSEN only for `open`** (NAT without exposing host loopback). |
| (c) `nftables` default‑drop inside the netns | L3/L4 IP/port filtering inside the hen's own netns. | Can't express host/SNI allowlists; redundant once the netns is empty. **Use only as optional defense‑in‑depth**, not the primary control. |

**Decision:** `off`/`allowlist` ⇒ **(a) empty netns + forced proxy**. `open`
⇒ **(b) pasta NAT**. (c) is a hardening follow‑up.

### 2.2 Why "advisory proxy" objections don't apply here

The classic red‑team line — *"`HTTPS_PROXY` is advisory; `curl --noproxy '*'`
or a raw socket ignores it"* — is **structurally defeated**, because we do not
depend on the env var for enforcement:

- The hen runs in an **empty netns**. There is **no default route, no external
  interface, no DNS reachability**. A raw socket / `nc` / direct‑IP `connect()`
  returns `ENETUNREACH`/`EHOSTUNREACH`. The env var is a **convenience** for
  cooperative clients; non‑cooperative clients simply get **no network**.
- The **only** reachable TCP endpoint inside the netns is `127.0.0.1:3128`
  (the hen‑local relay, §2.4), whose far end is the coopd proxy. The proxy is
  the enforcement point. **Enforcement is L3/L4 (kernel: nothing else is
  routable) + L7 (proxy allowlist).**

### 2.3 "How does loopback‑only netns reach the proxy without exposing coopd's
own loopback API/vault?"

**Each network namespace has its own private `127.0.0.1`.** The hen's
`127.0.0.1:3128` and coopd's root‑netns `127.0.0.1:<api/vault ports>` are
**different loopback stacks** — the hen cannot reach coopd's loopback at all.
The *only* cross‑namespace channel is a **per‑hen unix domain socket**
bind‑mounted into the sandbox, whose far end is the proxy — and the proxy
itself blocks loopback/RFC1918 via `safe_net`. Two independent layers protect
the vault. (Use a **filesystem** unix socket, not abstract: abstract sockets
are netns‑scoped, so the hen also cannot reach any root‑netns abstract socket.)

### 2.4 Socket layout & process model

```
root netns (coopd)                         hen netns (bwrap --unshare-net)
┌────────────────────────────┐             ┌──────────────────────────────┐
│ coopd egress proxy         │  AF_UNIX    │ coop-net-shim (entrypoint):   │
│  listens per-hen on:       │◄────────────┤  1. ip link set lo up         │
│  $DATA/egress/<key>.sock   │  stream     │  2. relay listen 127.0.0.1:3128
│  (0600, coopd-owned)       │             │     ──► /run/coop/egress.sock  │
│  applies <key>'s allowlist │             │  3. drop CAP_NET_ADMIN         │
│  + safe_net + conn cap     │             │  4. exec bash -c <cmd>         │
└────────────────────────────┘             │     env HTTPS_PROXY=…:3128      │
        ▲ vault/API on root-netns          └──────────────────────────────┘
        └─ unreachable from hen netns (separate loopback)
```

- `<key>` = `hen.id.workdir_key()` (e.g. `alice-coop__aria`), already unique
  per instance (`crates/coopd-core/src/ids.rs`).
- The per‑hen socket is created by coopd at hatch, mode `0600`, owned by the
  coopd uid. **Only that hen's** socket is bind‑mounted into **that hen's**
  sandbox ⇒ the socket the proxy `accept()`s on **is the hen's identity**
  (solves "loopback has no identity" — see §3.2). No tokens needed in v1.

### 2.5 Concrete bwrap flags

Extend `wrapped_command()` in `crates/coopd-tools/src/sandbox.rs`. Existing
fs flags stay; **add** per policy:

`off`:
```
bwrap … (existing fs flags) \
  --unshare-net \
  --                                   # no socket mounted, no relay, no proxy
  /path/to/coop-net-shim --no-egress -- bash -c <cmd>
```
(`coop-net-shim --no-egress` just brings `lo` up for software that expects a
loopback, then drops caps and execs; **no** relay, **no** socket ⇒ zero
egress.)

`allowlist`:
```
bwrap … (existing fs flags) \
  --unshare-net \
  --bind  $DATA/egress/<key>.sock  /run/coop/egress.sock \   # writable: connect() needs write on the socket inode
  --setenv HTTPS_PROXY http://127.0.0.1:3128 \
  --setenv HTTP_PROXY  http://127.0.0.1:3128 \
  --setenv ALL_PROXY   http://127.0.0.1:3128 \
  --unsetenv NO_PROXY \
  -- \
  /path/to/coop-net-shim --relay 127.0.0.1:3128 --upstream /run/coop/egress.sock -- bash -c <cmd>
```

`open`:
```
# pasta provides NAT egress in a fresh netns WITHOUT sharing host loopback.
pasta --config-net … -- \
  bwrap … (existing fs flags; do NOT --unshare-net, pasta owns the netns) \
    -- bash -c <cmd>
# http tool still applies safe_net SSRF in-process.
```
> Note: do **not** use `bwrap --share-net` for `open` — that shares the host's
> netns and re‑exposes coopd's loopback/vault. `pasta` gives a private netns
> with NAT'd external egress and no host‑loopback access.

`coop-net-shim` is a **hidden coopd subcommand** (`coopd __net-shim …`) or a
tiny sibling binary shipped in the same package — implemented in Rust so it has
no extra runtime dep. It must: bring `lo` up (needs `CAP_NET_ADMIN`, which the
unprivileged userns grants over its *own* netns), start the relay
(dumb TCP↔unix‑socket pipe), **`prctl(PR_CAPBSET_DROP)` / drop all caps**, then
`execve` the real command. Dropping caps before exec prevents the hen from
creating `tun`/`veth` inside its netns (blast radius is already just its own
empty netns, but drop for hygiene).

### 2.6 DNS & anti‑rebinding (TOCTOU)

- **Who resolves:** the hen never resolves. The empty netns has no resolver
  reachability and no `resolv.conf` pointing anywhere routable. Cooperative
  clients send hostnames to the proxy via `CONNECT host:port`; **the proxy
  does the DNS resolution** in the root netns.
- **Anti‑rebinding:** the proxy resolves the host **once**, runs every returned
  IP through `safe_net::is_disallowed_ip`, then `connect()`s to **that exact
  resolved `SocketAddr`**. There is no second resolution between check and
  connect ⇒ no rebinding TOCTOU window. (Contrast with the http tool, which
  passes a URL to `reqwest`; for the proxy we pin the IP explicitly.)
- **What's enforced where:** host‑allowlist match + IP‑disallow check are L7
  (proxy, on the `CONNECT` target). "No other egress exists" is L3/L4 (kernel,
  empty netns).

---

## 3. Proxy design

### 3.1 Shared process, per‑hen socket

**One** `coopd`‑owned egress proxy process (spawned at daemon start, see
`crates/coopd/src/main.rs`), **not** one process per hen. It **listens on a
distinct unix socket per hen** (`$DATA/egress/<key>.sock`). This gives per‑hen
identity, per‑hen allowlist, and per‑hen connection caps without N processes,
and reuses one DNS/`safe_net` codepath. New module:
`crates/coopd-tools/src/egress_proxy.rs`.

### 3.2 Authenticating which hen a connection belongs to

Loopback/unix has no app‑level identity, so we bind identity to the **socket
file**: the proxy keeps a map `socket_path → (hen_key, ResolvedNetPolicy)`.
Whichever per‑hen socket `accept()`s the connection **is** the hen. Because
each hen's bwrap mounts only its own `0600` socket, a hen cannot connect to
another hen's socket. (Belt‑and‑suspenders: `SO_PEERCRED` on the unix socket
confirms the peer uid is the expected sandbox uid; reject otherwise.)

### 3.3 Request handling (`CONNECT` / TLS)

For each accepted connection the proxy speaks minimal HTTP forward‑proxy:

1. Read the request line. v1 supports **`CONNECT host:port`** (covers HTTPS and
   any TLS upstream) and optionally plain `GET http://…` forward proxying.
2. **Allowlist check:** `host` must match the hen's `allow` (exact or
   `*.suffix`) **and** `port ∈ ports`. Else `403`, log, close.
3. **Resolve** `host` (root netns). For **every** returned address run
   `is_disallowed_ip`; if any is disallowed → refuse (`403`). Pick one allowed
   address; keep it.
4. **Connect** to the pinned `SocketAddr`. On success reply
   `200 Connection Established` and **byte‑pipe** both directions (TLS is
   terminated by the hen and the upstream; the proxy is a blind tunnel after
   `CONNECT`). The TLS cert is validated by the hen's client against the
   real host ⇒ no downgrade.
5. **Per‑hen connection cap** (anti‑DoS): max N concurrent tunnels per hen
   (default 64, configurable), plus a new‑connection rate limit. Over the cap
   ⇒ `503`, log. Idle‑timeout tunnels.

### 3.4 SNI re‑verification (L7 hardening)

Optional but recommended: peek the TLS `ClientHello` SNI on the tunneled bytes
and require `SNI == CONNECT host`. This blocks a hen that does
`CONNECT allowed.example.com:443` then speaks a different SNI to pivot via
domain‑fronting / shared front‑ends. State it as **L7 best‑effort**; if SNI is
absent/encrypted (ECH) fall back to the `CONNECT`‑host allowlist decision
(already made). Mark as a v1.1 nicety if it risks schedule.

### 3.5 What the proxy guarantees vs not

- **Guarantees:** TCP egress only to allowed host+port; never to a
  disallowed IP (anti‑SSRF, anti‑rebinding); per‑hen caps.
- **Does not** (v1): inspect/modify TLS‑encrypted payloads, inject secrets
  (see §5), or filter by URL path (it can't see inside TLS). Plain‑HTTP
  forward mode *can* see path/headers, but treat HTTP‑without‑TLS as
  discouraged.

---

## 4. macOS fallback (Seatbelt) — honest & fail‑closed

macOS has **no network namespaces** and a **single shared loopback**, so the
Linux "empty netns + 127.0.0.1 relay" trick is unavailable — a `127.0.0.1`
relay there would also expose coopd's real loopback/vault. Seatbelt can
`(deny network*)` and can allow egress to a specific **unix socket** or
**ip:port**, but it **cannot do hostname/SNI allowlisting** for TCP. So we are
explicit and **fail closed / stricter**:

| policy | macOS bash/tmux behavior (seatbelt) | macOS http tool |
|---|---|---|
| `off` | `(deny network*)` — full egress deny (L3/L4). | denied in‑code |
| `allowlist` | **All bash/tmux socket egress DENIED** (`deny network*`). We do **not** open a loopback relay (would expose host loopback). Host‑allowlisted egress is available **only via the in‑process `http` tool**. | per‑hen allowlist + `safe_net` (L7) |
| `open` | `(allow network*)` (the existing seatbelt profile, network unrestricted). | `safe_net` SSRF only |

Seatbelt profile sketch (extends the existing fs profile in
`seatbelt_profile()`):

```scheme
(version 1)
(allow default)
;; … existing file-write*/file-read* rules …
;; network, by policy:
;; off / allowlist:
(deny network*)
;; open:
;; (allow network*)
```

**Honesty contract:** on macOS, `allowlist` is delivered as a **strict
subset** of the advertised semantic — "egress only to allowed hosts" is
honored, but **only through the `http` tool**; raw bash/tmux network is denied
entirely (stricter, never weaker). This is documented in `SECURITY.md` and
surfaced as a hatch‑time log line:

> `hen aria: macOS — network=allowlist enforced for the http tool only; bash/tmux network is fully denied (no host-level allowlisting without a VM).`

If an operator needs allow‑listed egress from **bash** on macOS, the supported
answer is "run that hen on a Linux host," not a weaker silent fallback.

---

## 5. Secrets / BYOK composition (sentinel tokens)

Today the sealed BYOK vault (`crates/coopd-vault`) is read by **coopd**, and
the `http` tool runs **in coopd** — so a hen's *bash* process already never
sees raw keys (env is scrubbed in `sandbox.rs::scrub_env`). That property is
preserved and **strengthened** here:

- **v1 (in scope):** keys live only in coopd / the `http` tool. The bash/tmux
  hen has **no vault access and no credentials** — under `allowlist` it can
  reach allowed hosts but **cannot authenticate** to them (no secret to
  present). This is the secure default and requires no new mechanism; just
  keep scrubbing and never pass keys into the sandbox env.
- **v2 (follow‑up): sentinel‑token injection** (the OpenComputer pattern). The
  hen is given a **placeholder** token (e.g. `COOP_SENTINEL_<id>`); the egress
  proxy, for a specific allowed host, **terminates TLS** (proxy‑managed CA
  trusted only inside the hen) and **rewrites** the sentinel in the
  `Authorization` header to the real vault secret just‑in‑time, so the hen
  process never holds the raw key even when it must authenticate. This needs
  a TLS‑terminating (MITM) forward mode and per‑host credential mapping —
  explicitly **out of scope for v1** (v1's `CONNECT` tunnel is opaque and
  cannot rewrite headers). Recommend v1 ships the no‑credentials posture and
  documents sentinel injection as the planned follow‑up.

**Recommendation:** v1 = "bash/tmux egress is allow‑listed but unauthenticated;
authenticated upstream calls go through the `http` tool, which injects vault
secrets in‑coopd." Ship sentinel rewriting in v2.

---

## 6. Capability probe + fail‑closed degradation

Mirror the existing `bwrap_works()` / `seatbelt_works()` one‑time probes
(`crates/coopd-tools/src/sandbox.rs`). Add network‑specific probes and a hard
fail‑closed rule.

### 6.1 What to probe (once per process, cached in `OnceLock`)

- **Linux `netns_egress_works()`:** can we `bwrap --unshare-net` **and** the
  shim bring `lo` up (i.e. unprivileged userns grants `CAP_NET_ADMIN` over the
  new netns) **and** bind‑mount a unix socket and round‑trip one byte through
  the proxy? Probe end‑to‑end with a throwaway socket.
- **Linux `pasta_works()`** (only needed for `open`): is `pasta` present and
  able to set up NAT? If absent, `open` may fall back to the existing
  unrestricted path **with a warning** (open is allowed to degrade; it's the
  permissive policy).
- **macOS `seatbelt_net_deny_works()`:** does a profile with `(deny network*)`
  actually block a test `connect()`?

### 6.2 Fail‑closed rule (the important part)

> **If a hen requests a policy stricter than `open` (`off` or `allowlist`) and
> the platform cannot enforce it, REFUSE TO HATCH the hen.** Do not silently
> downgrade to open/env‑scrub‑only.

Concretely, at the `Defined → Hatching` transition (`crates/coopd/src/runner.rs`
hatch path; FSM in `crates/coopd-core/src/hen.rs`):

```
resolved = hen.manifest.network (or default per §1.3)
match resolved.policy {
  Open  => proceed (warn if pasta missing; http tool keeps safe_net),
  Off | Allowlist => {
    if !platform_can_enforce(resolved.policy) {        // probe result
        return Err("cannot enforce network=<p> on this host (no userns/netns
                    on Linux, or seatbelt unavailable on macOS); refusing to
                    hatch. Set policy: open to run without egress isolation,
                    or run on a supported host.");
        // Hatching -> Defined (the FSM already allows this rollback)
    }
    proceed;
  }
}
```

Also fail closed for the **tmux‑agent gap** (§0.1): if
`agent_kind != anthropic` and policy is stricter than `open` and the tmux
session is not yet wrapped in the netns/seatbelt profile, refuse to hatch.

`COOP_SANDBOX=0` (the existing escape hatch) must **also** disable network
isolation **and**, when set, force any `off`/`allowlist` hen to refuse to hatch
(you cannot ask for isolation and disable the sandbox). Add a parallel
`COOP_NET_SANDBOX=0` only if operators need to disable net isolation
independently — and it must trigger the same refuse‑to‑hatch for strict
policies.

### 6.3 Operator visibility

Expose enforcement state (like `isolation_active()`): add
`net_isolation_active(policy) -> Enforced | DeniedAll | OpenWithSsrf |
Unenforceable` so the farm UI / API can show, per hen, exactly what is
guaranteed.

---

## 7. Enforcement matrix (policy × OS × guarantee)

| policy | Linux (bwrap + netns + proxy) | macOS (seatbelt) | `http` tool (all OS) | Windows / no‑sandbox host |
|---|---|---|---|---|
| **off** | bash/tmux: empty netns, **zero egress** (L3/L4, kernel). | bash/tmux: `(deny network*)`, zero egress. | egress denied in‑code. | **refuse to hatch** (cannot enforce). |
| **allowlist** | bash/tmux: only reachable egress is the proxy ⇒ **host+port allowlist + IP block + anti‑rebind** (L7); raw socket / `--noproxy` / direct‑IP **fail** (no route, L3/L4). | bash/tmux: **all socket egress denied** (stricter); allow‑listed egress via the **http tool only**. | per‑hen allowlist + `safe_net` SSRF (L7). | **refuse to hatch**. |
| **open** | bash/tmux: `pasta` NAT egress (no host‑loopback exposure). | bash/tmux: `(allow network*)`. | `safe_net` SSRF only (L7). | env‑scrub only + **warn** (open is permissive; allowed to degrade). |

"Guaranteed" = enforced by the kernel/OS, not advisory. The only place an
attacker‑controlled process gets to make choices is **inside** the allowlist
(which host among the allowed set) — never *whether* to be restricted.

---

## 8. Red‑team bypasses, pre‑empted

| Attempted bypass | Why it fails |
|---|---|
| `curl --noproxy '*' https://1.2.3.4` / ignore `HTTPS_PROXY` | Empty netns has **no route/interface** ⇒ `ENETUNREACH`. Proxy env is convenience, not the boundary. |
| Raw socket / `nc` / custom client | Same — nothing is routable except the loopback relay; relay only speaks to the proxy. |
| `CONNECT 127.0.0.1:<vaultport>` (or any private IP) via the proxy | Proxy runs `safe_net::is_disallowed_ip` ⇒ loopback/RFC1918/link‑local/ULA/CGNAT refused. |
| DNS rebinding (resolve→TTL0→re‑resolve to 169.254/127.0.0.1) | Proxy resolves **once**, validates the resolved IP, and pins the `SocketAddr` for `connect()`. No second resolution. Hen can't resolve at all. |
| Reach coopd's API/vault on `127.0.0.1` | Per‑netns loopback: the hen's `127.0.0.1` ≠ coopd's. Only bridge is the per‑hen unix socket → proxy (which blocks loopback anyway). |
| Abstract unix socket to root‑netns service | Abstract sockets are netns‑scoped; unreachable across the boundary. We use a filesystem socket regardless. |
| Connect to another hen's egress socket | Each socket is `0600` and only that hen's socket is bind‑mounted into that hen; `SO_PEERCRED` re‑checks uid. |
| Kill the in‑netns relay | Only self‑harm: hen loses its sole egress path. No escape. |
| Use a tmux CLI agent instead of bash | **Addressed by §0.1 requirement**: tmux agents run under the same profile, else strict‑policy hens with `agent_kind != anthropic` refuse to hatch. |
| `CAP_NET_ADMIN` abuse (make a `tun`) | Shim drops caps before exec; even if not, it's the hen's **own empty netns** with no upstream. |
| Domain‑fronting (`CONNECT allowed.com`, different SNI) | Optional SNI re‑verification (§3.4); without it, the IP/host allowlist already bounds the connection to allowed infrastructure. |
| IPv6 link‑local / SLAAC auto‑config | Empty netns has no RAs and no global/LL route; nothing to autoconfigure. |
| Disable sandbox via env to weaken net | `COOP_SANDBOX=0`/`COOP_NET_SANDBOX=0` force strict‑policy hens to **refuse to hatch** (§6.2). |

---

## 9. v1 scope vs. follow‑ups

### v1 (minimal, genuinely secure on Linux, honest on macOS)

1. `network:` policy model: parse + validate (§1) — `coopd-core/manifest.rs`.
2. Thread a `ResolvedNetPolicy` into `ToolCtx` (`coopd-core/tool.rs`) and
   populate it in the runner (`coopd/runner.rs`).
3. `http` tool per‑hen allowlist (portable, L7) on top of existing `safe_net`
   (`coopd-tools/http.rs`, `coopd-tools/safe_net.rs`).
4. Linux bash/tmux enforcement:
   - `off` ⇒ `--unshare-net`, no socket/relay.
   - `allowlist` ⇒ `--unshare-net` + per‑hen unix socket + `coop-net-shim`
     relay + shared **egress proxy** (`CONNECT`, allowlist, `safe_net`
     IP‑recheck w/ pinned addr, per‑hen conn cap).
   - `open` ⇒ `pasta` NAT (fallback‑warn if absent).
5. macOS: seatbelt `(deny network*)` for `off`/`allowlist`(bash denied),
   `(allow network*)` for `open`; honest hatch‑time log line.
6. Capability probes + **fail‑closed refuse‑to‑hatch** for unenforceable
   strict policies and for un‑wrapped tmux agents (§6).
7. Docs: `SECURITY.md` threat‑model section + this file; flip
   `examples/aria.yaml` to an explicit `network: allowlist`.

### Follow‑ups (explicitly out of v1)

- **Sentinel‑token secret injection** (TLS‑terminating proxy mode) — §5, v2.
- **SNI verification / ECH handling** — §3.4 hardening.
- **`pasta` fine‑grained filtering** and **nftables default‑drop** as
  defense‑in‑depth inside the netns.
- **Per‑hen bandwidth / request‑rate limits** at the proxy.
- **Wrapping tmux CLI agents** fully (if not done in v1, they're gated by §6).
- **HTTP/3 (QUIC/UDP)** egress to allowed hosts.
- **Windows enforcement** (WFP/AppContainer) — until then Windows refuses to
  hatch strict policies.
- **coop/v2:** flip absent‑policy default to `allowlist`.

---

## 10. Implementation checklist → files/crates

| # | Change | File(s) |
|---|---|---|
| 1 | `NetworkSpec`/`NetPolicy`/`NetAllow` types; `network: Option<NetworkSpec>` on `AgentManifest`; validation (host/wildcard/port rules) | `crates/coopd-core/src/manifest.rs` |
| 2 | `ResolvedNetPolicy` (compiled allowlist matcher) on `ToolCtx` | `crates/coopd-core/src/tool.rs` |
| 3 | Host‑allowlist matcher (`host_allowed(host, port)`) reusing `is_disallowed_ip`; `validate_url` gains policy arg | `crates/coopd-tools/src/safe_net.rs` |
| 4 | `http` tool: honor `ctx.net_policy` (`off`⇒deny; `allowlist`⇒match; `open`⇒safe_net only) | `crates/coopd-tools/src/http.rs` |
| 5 | `bash` sandbox: add netns/seatbelt network args per policy; return `Result` so failures bubble | `crates/coopd-tools/src/sandbox.rs` |
| 6 | New shared egress proxy (per‑hen unix socket, `CONNECT`, allowlist, pinned‑addr connect, conn cap, `SO_PEERCRED`) | `crates/coopd-tools/src/egress_proxy.rs` (new) |
| 7 | `coop-net-shim` (lo‑up, relay, cap‑drop, exec) | hidden subcommand `coopd __net-shim` in `crates/coopd/src/main.rs` (+ helper module) |
| 8 | Runner: populate `ToolCtx.net_policy`; provision per‑hen socket at hatch; **refuse‑to‑hatch** on unenforceable strict policy / un‑wrapped tmux agent | `crates/coopd/src/runner.rs`, `crates/coopd-core/src/hen.rs` (FSM rollback already supported) |
| 9 | Spawn egress proxy at startup; wire `$DATA/egress/` dir | `crates/coopd/src/main.rs` |
| 10 | Wrap tmux CLI agents in the same profile (or gate via #8) | `crates/coopd/src/shell.rs`, `crates/coopd/src/session.rs` |
| 11 | Capability probes + `net_isolation_active()` for UI/API | `crates/coopd-tools/src/sandbox.rs`, `crates/coopd/src/api.rs` |
| 12 | Docs + threat model + example flip | `docs/net-isolation.md` (this), `SECURITY.md`, `examples/aria.yaml`, `CHANGELOG.md` |

### Test obligations (extend existing patterns)

- `safe_net`: allowlist matcher unit tests (exact, `*.suffix`, apex‑not‑matched,
  port gating, IP‑literal still SSRF‑blocked).
- `sandbox` (Linux, gated on `net_isolation_active`): inside `allowlist`,
  `curl https://<disallowed>` fails, `curl --noproxy '*' https://1.1.1.1`
  fails (no route), `CONNECT` to an allowed host succeeds, `CONNECT 127.0.0.1`
  refused by proxy. Under `off`, all egress fails. Skip when probe inactive
  (same pattern as the existing fs‑isolation tests).
- macOS: under `off`/`allowlist`, a bash `connect()` is denied; http tool
  still reaches allowed host.
- Manifest: validate rejects bad wildcards / `allow` on non‑allowlist policy.
- Fail‑closed: requesting `allowlist` on a host where the probe fails returns a
  hatch error (no silent open).
```
