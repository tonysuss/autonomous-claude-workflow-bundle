# Runtime: sessions that survive, stop, and spend within limits

This is how `interlock run` keeps every headless session accounted for when things go wrong: the supervisor dies, someone presses Ctrl-C, the operator cancels from another terminal, a session hangs, or a task runs out of budget. It builds the supervisor patterns in design §2 (re-attach by run ID, mark orphans, write the handoff before the state change, deduplicate kickoffs, classify failures, plus lease epochs), §5 (resume from records; pause-safely's `wip:` commit plus resume note as the export format), §1 and §9 (pin the host version), and §12's P3 gate.

Everything here works the same on Copilot CLI and Claude Code. Copilot's path is tested end to end on the real CLI, offline against a scripted model; Claude Code's was shown live once (below).

## A session's handoff is written before it starts

A session starts in three steps:

1. **Spawn, held at a gate.** The runner starts `/bin/sh` in a new process group with a one-line gate script: it waits for `go` on stdin, then `exec`s the host with its real stdin. Its process id, group, and the kernel's start time for that process (from `/proc/<pid>/stat`) are known now, and the host is not running yet.
2. **Record the handoff**, in its own transaction, on the attempt: host, pid, process group, process start time, start time, deadline, whether the deadline comes from the task's budget, transcript path, the host's session id, and the supervisor's pid. interlock chooses the host session id itself and passes it with `--session-id`; both hosts accept one.
3. **Release the gate.** The host replaces the gate process, keeping its pid.

A supervisor that dies before step 3 leaves no host running: the gate reads end of input and exits. The host writes its event stream straight to the transcript file, not through a pipe, so it keeps running if the supervisor dies.

**One session per attempt.** The store refuses a second handoff for an attempt, and refuses one for an attempt that is no longer running. A second `interlock run` in the same checkout is refused while the first is alive: the lock file is now created atomically, and a holder that is gone (or a zombie) is replaced.

## Restart: re-attach or reconcile

When `interlock run` starts, it looks at every attempt of the task still marked running:

| Found | What happens |
| --- | --- |
| A handoff whose process is alive (same pid and same kernel start time, not a zombie) | **Re-attach.** The supervisor notes the time in `handoff.reattached_at`, waits for the process, enforces the original deadline, honors cancellation, then reads the transcript and finishes the attempt exactly as the first supervisor would have: a worker's tree is submitted (G3) and a verifier's attempt is ended and the task advanced. |
| A handoff whose process is gone | A **synthetic failure report**: the attempt ends `failed` with reason `crash`, `synthetic: true`, and the time its transcript was last written as its wall-clock use. A worker's unfinished worktree is salvaged to `interlock/wip/<task>`. Then R3. |
| No handoff | The supervisor died before the session started. Same synthetic failure, then R3. |

A re-attaching supervisor needs the attempt's token to submit its result. The store keeps only token hashes, so the supervisor keeps each running attempt's token in `.interlock/sessions/<attempt>.token` (mode 0600) and deletes it when the attempt ends. Tokens are bookkeeping, not security, as before. If that file is lost, the live session is stopped and reconciled rather than left running unseen.

The exit code of a process the supervisor did not start cannot be read, so a re-attached session's transcript decides whether it completed: both hosts end with a `result` event, and no `result` means it failed.

## Stopping a session

| Trigger | Session | Attempt | Task | `interlock run` exits |
| --- | --- | --- | --- | --- |
| SIGINT or SIGTERM to `interlock run` | Process group gets SIGTERM, then SIGKILL after 3 s | `cancelled`, reason `cancelled` | R3 back to ready (a verifier's task stays awaiting verification) | 6, with the report on stdout and `{"interrupted": "SIGINT", "resume": "interlock run ..."}` on stderr |
| `interlock task cancel <task>` from another terminal | Seen within about 250 ms, then stopped the same way | `cancelled`, reason `cancelled` | cancelled | 5 |
| Anyone ends the attempt (`task retry`, `task fail`) | Stopped the same way | keeps the status they gave it | as they left it | |
| Session timeout | Stopped | `failed`, reason `timeout` | R3, or failed when the attempts are spent | |

The terminal sends Ctrl-C to the supervisor's process group, never to the host's, so the host is always stopped by the supervisor and the attempt is always recorded. A second signal does nothing more: exiting early would leave the host running unseen, and stopping it takes at most a few seconds. Signals are handled with `signal-hook`; the workspace still forbids unsafe code. After any session ends, the whole process group is killed, so nothing the agent started in the background outlives it.

A cancelled worker's unfinished work is exported before its worktree is removed (see pausing safely below).

## Why an attempt ended

Every session end is classified, recorded once on the attempt (`end.reason`, `end.detail`, `end.synthetic`), written as an event (`attempt.completed`, `attempt.failed`, or the new `attempt.cancelled`, with the same fields and what was spent), and reported in the run report's `sessions[].ended_because`.

| Reason | When |
| --- | --- |
| `completed` | The host finished and reported success |
| `rejected` | It finished, but its result changed files it may not change |
| `timeout` | The session ran past its time limit |
| `host_error` | The host could not start, exited non-zero, or reported an error |
| `auth_failure` | As `host_error`, with sign-in wording in its output ("Not logged in", "Invalid API key", "unauthorized", ...) |
| `crash` | The host was killed by a signal it did not get from interlock, or its supervisor died and the session was gone on restart |
| `cancelled` | A signal, `task cancel`, or another end of the attempt stopped it |
| `budget_exhausted` | The task's wall-clock budget set the deadline it hit, or the host stopped at its own cost cap (Claude Code's `error_max_budget_usd`) |

`interlock task events <task>` lists them. When the operator cancels a running task, the attempt keeps the `cancelled` status `task cancel` gave it, and the supervisor still records the end reason and spending once.

## Budgets

A task's `[budget]` may now set, besides `max_attempts`:

```toml
[budget]
max_attempts = 3
max_wall_secs = 1800         # total session time, summed over attempts
max_cost_usd = 2.50          # where the host reports dollars (Claude Code's total_cost_usd)
max_premium_requests = 10    # where the host reports them (Copilot CLI's usage.premiumRequests)
```

Each attempt records what its session used (`spent`: `wall_ms`, `cost_usd`, `premium_requests`, `turns`). Measures stay in each host's own unit; a limit applies only where a host reports that measure. The run report shows the task's total.

- **Wall clock** is enforced inside a session: its deadline is the session timeout or what is left of the budget, whichever is sooner.
- **Dollars** are passed to Claude Code as `--max-budget-usd <what is left>`, which stops it mid-session. interlock also checks the total after every session.
- **Premium requests** are checked after every session. Copilot's own cap (`--max-ai-credits`) counts a different unit, so it is not used.

When any limit is reached the task fails, in one transaction, with the reason, for example `budget exhausted: the cost budget of $0.50 is spent ($0.6000 used)`. The supervisor also checks before starting each session.

## Pinning host versions

```toml
# .interlock/config.toml
[pins]
copilot = "1.0.91"
claude-code = "2.1.289"
```

When the run's host is pinned and the installed version differs, `interlock run` blocks the task before any session, with the reason (`copilot 1.0.92 is installed, but .interlock/config.toml pins 1.0.91; install the pinned version or change the pin`). Unblock it once the versions agree. `interlock host inspect` adds a `pin` object to each host: `unpinned`, `match`, `mismatch`, or `unknown` (pinned, but the version could not be read). Other sections in the file are ignored, so it can be shared. The store's `.gitignore` ignores everything in `.interlock/`; commit the config with `git add -f .interlock/config.toml`.

## Pausing safely

```bash
interlock task export <task>                 # wip: commit on interlock/wip/<task>, plus a resume note
interlock task resume <task> [--from <rev>]  # the next worker starts from it
interlock run <task> --host <host>
```

`task export` reads the newest worker worktree that still exists (a live session's, mid-run) or else the last accepted output, and writes its tree, through a temporary index, as a commit whose parent is the task's base commit. The message starts `wip: <task>: <intent>` and carries a resume note: where it came from, how to resume, and the brief `interlock brief` builds from records. The commit goes on `refs/heads/interlock/wip/<task>`, interlock's own ref; no other branch, the user's index, or the user's files change. The note is also written to `.interlock/exports/<task>.md`.

An interrupted or orphaned worker's work is exported the same way, automatically, when it changed anything.

`task resume` needs the task ready (for example after Ctrl-C). It makes the export's tree the starting point of the next worker, the way the last accepted output already is for rework. It is not a move and changes no evidence: the tree satisfies nothing until a worker's result built on it is accepted, through G3 and G4 as usual. That worker's prompt says it starts from exported work in progress, and says how the previous attempt ended.

## Records

All additions are optional, so earlier records still validate.

- `attempt`: `handoff`, `end`, `spent`.
- `task.budget`: `max_wall_secs`, `max_cost_usd`, `max_premium_requests`.
- `event.type`: `attempt.cancelled`.
- Any move into done, failed, or cancelled now closes the task's open attempts in the same transaction: running ones are cancelled and submitted ones, whose results were judged, complete. Before, a worker whose result was verified stayed `submitted` after the task was done.

## Tests

| P3 gate item | Test |
| --- | --- |
| Supervisor restart, session alive | `run_robustness::a_supervisor_killed_mid_session_reattaches_and_carries_on`: `kill -9` mid-session; the host keeps running; a second supervisor is refused meanwhile; the restart re-attaches and the log is `G1 G2 G3 G4 G7`, with one worker session. `a_verifier_session_is_reattached_too` does the same mid-verification |
| Supervisor restart, session dead | `a_session_that_died_with_its_supervisor_is_reconciled_and_retried`: synthetic `crash` report and event, salvaged export, `G1 G2 R3 G2 G3 G4 G7` |
| Termination | `sigint_cancels_the_session_and_the_exported_work_resumes`: two quick SIGINTs, exit 6, process group gone, attempt cancelled, task ready; `task resume` and a second run finish on the exported tree. `sigterm_stops_a_run_the_same_way`. `task_cancel_from_another_terminal_stops_the_running_session`, which also exports the live worktree by hand mid-session |
| Timeout | `a_session_timeout_retries_until_the_attempts_are_spent`: `G1 G2 R3 G2 fail` |
| Budgets and pins | `a_spent_wall_clock_budget_fails_the_task`, `a_spent_cost_budget_fails_the_task` (a scripted stand-in for Claude Code; also checks `--max-budget-usd` and `--session-id` reach the host), `a_host_version_that_differs_from_its_pin_blocks_the_run` |
| Every started attempt ends in a terminal row | Every test above ends with it, including a recorded end reason |
| Late completion | `sessions::reconcile_running_ends_every_running_attempt_with_a_synthetic_report` (a reconciled worker's late result is superseded), with the existing invariant 4 tests |
| Policy denial, denied tool unavailable | The existing Copilot tests (`the_stop_guard_holds_the_worker_and_hooks_deny_what_it_was_not_granted`) |

Store tests (`crates/interlock-store/tests/sessions.rs`) cover `reconcile_running`, which had none, plus handoffs, ends, budgets, and closing attempts on terminal moves. Session runner tests use real processes: the gate holds the host until released, an abandoned gate never starts it, `attach` follows a process another supervisor started, a reused pid is told apart, stopping kills the whole group, and each end reason is classified.

Recorded runs, including one live Claude Code session whose supervisor was killed and restarted, are in [`evidence/runtime/`](../evidence/runtime/README.md).

## Limits

- A supervisor killed with SIGKILL cannot stop its session: the host keeps running, and spending, until the next `interlock run` re-attaches to it or its recorded deadline passes. Use SIGINT or SIGTERM to stop a run.
- Liveness uses `/proc` (Linux). Elsewhere it falls back to `kill -0`, which cannot tell a reused pid from the session.
- A session that finished while no supervisor was watching is reconciled as a failure, even if its transcript shows it completed. Its work is salvaged to `interlock/wip/<task>`, and `task resume` can pick it up.
- Restart reconciles only the task being run. Another task's orphaned session waits for that task's next run.
- Cost and premium-request limits are checked between sessions; only Claude Code enforces a dollar cap mid-session. A Copilot session can overrun its premium-request budget before it ends.
- Budgets are enforced by `interlock run`. `interlock attempt start`, used by hand, does not check them.
- Baseline checks run before any session and do not watch for signals; a long one finishes or times out before an interrupted run exits.
- `task resume` is not a move, so it is not in `task log`; `task show` shows the new starting tree.
- A worker whose result is awaiting verification stays `submitted`, an open attempt, until the task is decided.
