# Persistent Hen Memory

> Status: **v1 SHIPPED** — episodic memory (record + replay + retention +
> inheritance). Semantic (LLM-summarized) memory is a documented future phase.

By default an agent that runs one job and then another has no idea the first
job happened — every task is a fresh, from-scratch conversation. Coop's memory
layer fixes that for **Hens**: it turns a stateless job-runner into something
with continuity.

## What it does

After a Hen finishes a job — **whether it succeeded or failed** — Coop records
a single compact **episode**:

| field | meaning |
|---|---|
| `id` | UUIDv7 (sorts chronologically) |
| `hen_id` / `job_id` | what produced the episode |
| `at` | when it was recorded (RFC 3339, UTC) |
| `prompt` | the task, truncated to a budget |
| `summary` | short outcome (result text, or `(failed) <error>`) |
| `turns` | reason/tool turns the job consumed |
| `outcome` | `done` or `failed` |

On the **next** job, Coop loads that Hen's most recent episodes and prepends a
`## Memory — your recent episodes` section to the system prompt, oldest first.
The Hen reads its own history and continues from context instead of repeating
work.

This is *episodic* memory: literal, deterministic traces of what happened. No
model call is involved in recording or replaying it, so it works offline and
costs nothing extra.

## Enabling it

Add a `memory:` block to a Hen's `agent.yaml`:

```yaml
memory:
  # Prune episodes older than this many days. Omit or 0 = keep indefinitely.
  episodic_retention_days: 30
  # Copy another Hen's episodes at creation time (see "Inheritance").
  # inherit_from: local.coop/aria
```

Memory is **on by default** once a Hen exists — even with no `memory:` block a
Hen accumulates and replays episodes (with no retention pruning). The block
only configures retention and inheritance.

### Tuning context injection

The number of recent episodes injected into the prompt is capped (default
**8**). Override per-daemon with an environment variable:

```bash
COOP_MEMORY_CONTEXT_ENTRIES=12 coopd serve   # inject up to 12
COOP_MEMORY_CONTEXT_ENTRIES=0  coopd serve   # disable injection (still records)
```

## Retention (governance)

When `episodic_retention_days` is set, Coop prunes episodes older than that
window every time a new episode is recorded. This keeps the working set — and
the injected context — bounded, and gives operators a clear data-retention
knob. Pruning is enforced server-side in the orchestrator, not the client.

## Inheritance (lineage)

Set `inherit_from: <hen-id>` on a **new** Hen's manifest and, at creation,
Coop copies the parent Hen's episodes (under fresh ids) into the child and
records lineage on the child:

- `lineage.parent` — the parent Hen id
- `lineage.generation` — parent generation + 1 (originals are generation 1)

This lets you "fork" a Hen that has accumulated useful context. A missing or
invalid `inherit_from` is non-fatal — the Hen simply starts fresh.

## Inspecting & forgetting

CLI:

```bash
coop hen memory local.coop/aria            # recent episodes (JSON)
coop hen memory local.coop/aria --limit 5  # only the 5 most recent
coop hen forget local.coop/aria            # delete ALL episodes for this hen
```

HTTP:

```
GET    /api/v1/hens/:id/memory?limit=N   -> [MemoryEntry]
DELETE /api/v1/hens/:id/memory           -> { "forgotten": <count> }
```

Deleting a Hen (`coop hen delete`) also purges its memory — the right-to-forget
is automatic.

## Auditability

Every recorded episode emits a `memory_recorded` orchestrator event
(`{ hen_id, entry_id }`) on the `/api/v1/watch` stream, so memory growth is
observable alongside hen/job lifecycle events.

## What's deliberately *not* here yet

`semantic_summarize_every` in the manifest is **reserved**. True semantic
memory — periodically distilling many episodes into a smaller learned summary —
requires an LLM call and a storage format for the distilled knowledge. That is
a future phase; Coop does not fake it. Today's memory is honest, deterministic,
and free.

## Storage

Episodes live in the same `redb` database as Hens and Jobs, in a
`memories_v1` table keyed `"<hen-id>\0<episode-id>"`. Because episode ids are
UUIDv7, a single Hen's episodes form a contiguous, chronologically ordered key
range — listing, limiting, pruning, and per-Hen isolation are all range scans.
