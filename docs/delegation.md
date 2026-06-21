# In-farm Delegation

> Status: **v1 SHIPPED** — a manager Hen can dispatch a subtask to another Hen
> on the same farm via the built-in `delegate` tool and get the result back.

A single Hen running one job is an *assistant*. A Hen that runs a goal
end-to-end with memory is an *agent-operator*. The next tier is an
**organization**: you talk to one "manager" Hen and it coordinates specialist
Hens. In-farm delegation is the primitive that makes that tier possible — the
OSS, single-farm counterpart to cross-farm leasing.

## What it does

A Hen whose manifest grants the `delegate` tool can hand a subtask to another
Hen on the same farm:

1. The manager calls `delegate { hen, prompt }`.
2. Coop validates the request (no self-delegation, depth within limit, target
   exists), creates a **sub-job** owned by the **target** Hen, and emits a
   `delegated` audit event.
3. The target runs the sub-job through the normal reason/tool loop — and
   records its own memory episode, just like any other job.
4. The manager's `delegate` call returns the sub-job's `status` and `output`.

Because a sub-job is an ordinary job, everything else already works: per-hen
sandbox + network policy, memory, lease enforcement, and the Farm UI all apply
to delegated work unchanged.

## Enabling it

There are two ways delegation happens, with different gates:

- **Farmer-initiated** (you run `coop hen delegate …` or `POST …/delegate`) —
  always available, like submitting a job. You are explicitly orchestrating.
- **Hen-initiated / autonomous** (the Hen's brain decides to call the
  `delegate` tool mid-task) — **opt-in**: the Hen's manifest `tools:` list must
  include `delegate`. Hens without it can't delegate on their own.

To make a Hen an autonomous manager, grant it the tool:

```yaml
spec_version: coop/v1
name: aria
brain:
  provider_id: vault:byok-anthropic
  model: claude-sonnet-4-5-20250929
tools: [bash, file_read, file_write, delegate]   # <- manager
```

Worker Hens need no special configuration — any Hen on the farm can receive a
delegated subtask.

## The `delegate` tool

| field | meaning |
|---|---|
| `hen` (in) | target Hen id, e.g. `local.coop/scout` |
| `prompt` (in) | the subtask for the target to perform |
| `hen` (out) | the target Hen id (normalized) |
| `job_id` (out) | the created sub-job |
| `status` (out) | `Done`, `Failed`, or `Cancelled` |
| `output` (out) | the sub-job's result text (or error summary) |

The tool requires no `fs`/`net`/`proc` capabilities, so it works under
`network: off`.

## HTTP & CLI

```bash
# REST: manager aria delegates to worker scout
curl -X POST http://127.0.0.1:4477/api/v1/hens/local.coop%2Faria/delegate \
  -H 'content-type: application/json' \
  -d '{"to":"local.coop/scout","prompt":"summarize today's logs"}'

# CLI
coop hen delegate local.coop/aria local.coop/scout "summarize today's logs"
```

Both return `{ hen, job_id, status, output }`. The call blocks until the
sub-job finishes or the delegation times out.

## Governance & safety

Delegation is **governed by design** — it reuses Coop's boundary-enforcement
strengths rather than opening a new hole:

- **Opt-in (autonomous)** — a Hen only delegates *on its own* if its manifest
  lists `delegate`. Farmer-initiated delegation via API/CLI is always available.
- **No self-delegation** — a Hen cannot delegate to itself (rejected with
  `400` over HTTP).
- **Depth cap** — every delegation hop increments the job's `delegation_depth`;
  beyond `MAX_DELEGATION_DEPTH` (**3**) the request is refused. Because cycles
  (A→B→A) increment depth each hop, the cap also breaks loops.
- **Non-blocking orchestrator** — the orchestrator never blocks waiting on a
  sub-job. It validates + enqueues + returns the sub-job id; the *caller* polls
  for the result (with a timeout), so the single-threaded orchestrator loop
  stays responsive and can process the worker's updates. No self-deadlock.
- **Timeout** — `COOP_DELEGATE_TIMEOUT_SECS` (default `180`) bounds the wait. A
  busy worker queues the sub-job and runs it once it frees up; if that exceeds
  the timeout the caller gives up gracefully (the sub-job may still complete).
- **Audit** — every delegation emits a `delegated` event on `/api/v1/watch`.

## Limits & future work

- Delegation is **synchronous** from the caller's view (it waits for the
  result). Fire-and-forget / fan-out orchestration is a future phase.
- It is strictly **in-farm**. Dispatching work to a Hen on *another* farm is the
  separate cross-farm leasing path, not this tool.
- There is no automatic manager↔worker memory sharing beyond each Hen recording
  its own episodes; use `inherit_from` (see [memory](memory.md)) if you want a
  worker to start from a manager's context.
