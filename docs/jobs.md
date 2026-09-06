# Jobs and recovery

Jobs are the durable work history for in-process Hens. The Farm UI's **Jobs**
tab is separate from **Sessions** (interactive terminals) and **Tasks** (work
sent to terminal-hosted CLI agents).

Each Hen executes at most one job at a time. Additional submissions enter a
first-in, first-out queue. Both the Hen and the job are claimed in one storage
transaction before execution starts. Completion persists the outcome, releases
the Hen, records episodic memory, and advances its queue.

## Find and inspect work

Use the Jobs tab to search, filter by Hen or status, and browse newest-first
pages. A job's details contain its original prompt, result or error, timestamps
and reason-loop turn count. Page counts describe the displayed history, not
the farm's lifetime activity.

```bash
coop job list --status failed --limit 50 --order desc
coop job list --hen-id local.coop/aria --limit 50 --offset 50 --order desc
coop job get <job-id>
coop job run local.coop/aria "Summarize the README" --wait
```

`coop job wait <job-id>` prints the terminal record. Failed and cancelled jobs
exit nonzero, as do unavailable APIs and expired waiting deadlines. Existing
`coop job run` without `--wait` still returns immediately with a job ID.

## Retry or cancel

```bash
coop job retry <failed-job-id>
coop job cancel <queued-job-id>
```

A retry creates a **new job** with the same Hen, prompt and delegation depth,
plus a `retry_of` link to the original. It does not change the original result
or error. Current prompt, lifecycle, network and lease policies are applied
again; a retry cannot bypass a changed policy or re-activate an archived Hen.
Only `FAILED` and `CANCELLED` jobs can be retried.

Cancellation applies **only to queued work**. It never claims to stop a running
model request or process. Repeating the cancellation of an already cancelled
job is safe; cancelling running or completed work returns HTTP 409. A Hen with
running work cannot be put to sleep, woken or deleted. Cancel its remaining
queued work and wait for active work to finish before deleting the Hen.

## HTTP API

`GET /api/v1/jobs` retains its original JSON array response and, without query
parameters, the full oldest-first history. Opt into bounded discovery:

| Parameter | Meaning |
|-----------|---------|
| `hen_id` | Exact Hen ID; URL-encode the slash |
| `status` | `QUEUED`, `RUNNING`, `DONE`, `FAILED`, `CANCELLED`; case-insensitive |
| `q` | Case-insensitive search over ID, Hen, prompt, result and error; at most 256 UTF-8 bytes after trimming |
| `limit` | 1-500 matching jobs |
| `offset` | Nonnegative number of matching jobs to skip |
| `order` | `asc` (default) or `desc`, in UUIDv7 creation order |

Filters are applied before pagination. Bounded queries do not materialize the
entire history. Offset pages can shift when new work arrives; refresh the first
page for the latest jobs.

`POST /api/v1/jobs/:id/cancel` returns the updated job (200).
`POST /api/v1/jobs/:id/retry` returns `{"job_id":"..."}` (202).
Invalid input returns 400, unknown jobs 404, state conflicts 409, and oversized
prompts 413. Protected farms require the same authentication as other APIs.

`GET /api/v1/readyz` checks orchestrator responsiveness and returns 503 when it
cannot respond. `/api/v1/healthz` remains the lightweight process liveness
endpoint.

## Restart behavior

On restart, queued or running work left by the previous process is marked
`FAILED` with `interrupted at restart`. Coop does **not** silently replay it:
jobs may have performed external side effects. Inspect the result and choose
an explicit retry when appropriate. Existing stored jobs without `retry_of`
remain readable, and no database migration is required.
