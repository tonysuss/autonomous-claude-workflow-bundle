# Schema changelog

Every record schema has its `$id` under `https://schemas.interlock.dev/<version>/`, and `$ref`s are relative to it, so one version's files only refer to each other.

## v1

October 6, 2026. The records as the P2 to P4 builds grew them. They kept the `v0` ids while they grew; v1 names what they had become.

v1 adds to v0:

| Record | Added |
| --- | --- |
| Check run | A new, tenth record: interlock's own execution of a criterion's check on one tree (`output`) or on the input snapshot (`base`): who asked for it (`producer`), the exit code, `timed_out`, `vacuous` (the detector that found the check ran nothing), the duration, and the output as a content-addressed artifact (`output_ref`) plus its tail. Append-only |
| Criterion | `baseline`: `fails` (the check must reproduce the problem on the input snapshot), `passes` (it must already hold there, as a regression guard) or `any` (the default) |
| Task | Budget limits beside `max_attempts`: `max_wall_secs`, `max_cost_usd`, `max_premium_requests` |
| Task, snapshot | `protected_paths`: the files the task's checks run, which no result may change |
| Attempt | `binding`: how interlock knows who holds the attempt (`interlock_launched`, `subagent`, `session`, `unbound`), with the host session and subagent |
| Attempt | `handoff`: how to find its headless session again (process and group ids, kernel start times, deadline, transcript, the host's session id, the supervisor, the commit the worktree started at, the names of the variables the session got, effort, re-attach times) |
| Attempt | `end`: why its session ended (`completed`, `rejected`, `timeout`, `host_error`, `auth_failure`, `crash`, `cancelled`, `budget_exhausted`), and whether interlock wrote that itself (`synthetic`) |
| Attempt | `spent`: wall time, cost, premium requests, turns, and the model the host reported using |
| Claim, assessment | `bound_via`: for an assessment, how its verifier attempt was bound when it was recorded |
| Result | Status `rejected`: a result that changes files outside the task's scope or a file its checks run |
| Event | Types `attempt.cancelled` and `task.resumed` |
| Operation | Kind `disarm_auto_merge` |

v1 removes nothing and makes nothing required that a v0 record could leave out: every addition is an optional property, a new enum value, or the new check-run record. So a record valid under v0 is valid under v1.

Nothing else changes. Records in the store are kept as JSON without their `$id`, the store's own migrations are unchanged, and the policy digest and currency keys do not depend on the schema version, so evidence recorded under v0 stays current. A store written by the last v0 build opens and reads as it is, and every record in it validates against v1: `crates/interlock-store/tests/v0_store.rs`.

## v0

October 5, 2026 (commit 5834c31). Spike S4: one schema for each record in the design's §5 (task, criterion, attempt, result, claim, assessment, event, grant, operation) and a common file for the shared definitions.
