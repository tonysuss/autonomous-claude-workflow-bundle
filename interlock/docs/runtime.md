# Runtime: sessions that survive, stop, and spend within limits

This is how `interlock run` keeps every headless session accounted for when things go wrong: the supervisor dies, someone presses Ctrl-C, the operator cancels from another terminal, a session hangs, a task runs out of budget, or a session tries to leave its process group. It builds the supervisor patterns in design §2 (re-attach by run ID, mark orphans, write the handoff before the state change, deduplicate kickoffs, classify failures, plus lease epochs), §5 (resume from records; pause-safely's `wip:` commit plus resume note as the export format), §1 and §9 (pin the host version), and §12's P3 gate.

Everything here works the same on Copilot CLI and Claude Code. Copilot's path is tested end to end on the real CLI, offline against a scripted model; most other paths are tested against a scripted stand-in for Claude Code; Claude Code itself was run live twice, once before and once after the review fixes, with the supervisor killed mid-session each time (see the evidence).

## A session's handoff is written before it starts

A session starts in three steps:

1. **Spawn, held at a gate.** The runner starts `/bin/sh` in a new process group with a one-line gate script: it waits for `go` on stdin, then `exec`s the host with its real stdin. Its process id, group, and the kernel's start time for that process (from `/proc/<pid>/stat`) are known now, and the host is not running yet.
2. **Record the handoff**, in its own transaction, on the attempt: host, pid, process group, process start time, start time, deadline, whether the deadline comes from the task's budget, transcript path, the host's session id, the supervisor's pid and start time, the commit the worktree started at, the reasoning effort, and the names (never the values) of the environment variables the session was given. interlock chooses the host session id itself and passes it with `--session-id`; both hosts accept one.
3. **Release the gate.** The host replaces the gate process, keeping its pid.

A supervisor that dies before step 3 leaves no host running: the gate reads end of input and exits. A signal that arrives while the handoff is written also stops the host from starting, and a signal that arrives while the host is being inspected, before the attempt is opened, opens nothing. The host writes its event stream straight to the transcript file, not through a pipe, so it keeps running if the supervisor dies.

**The host is inspected once per run.** Its version and help are read once, and every later check in the run (effort, pin, each session's capabilities) uses that reading. `--version` and `--help` are each tried up to three times, 10 seconds apiece: Copilot CLI 1.0.91's `--help` hung once in 80 calls here. Before the retry, and before inspection was done once, such a hang read as a host without any flags, so the verifier looked unavailable and the task was blocked; that was the intermittent failure of `run_robustness::a_session_that_died_with_its_supervisor_is_reconciled_and_retried`.

**One session per attempt, one controller per checkout.** The store refuses a second handoff for an attempt, and refuses one for an attempt that is no longer running. The controller lock is the operating system's: `interlock_supervisor::ControllerLock` holds an exclusive `flock` on `.interlock/supervisor.lock` for as long as the supervisor runs, and the kernel releases it however the process ends. The pid written in the file is only for people reading it. A second `interlock run` in the same checkout fails at once with "another supervisor … holds the controller lock".

**No store on a network filesystem.** That lock and SQLite's own locks hold on local disk; over NFS or SMB two machines can each believe they hold them. So every command that opens the store (`init`, every command that reads or moves a task, `run`, `verify`, `integrate`, `reconcile`, the hooks) first reads its directory's filesystem and refuses a network one. The command exits 2 with `{"error": "network_filesystem"}` and says how to move the store. **`INTERLOCK_ALLOW_NETWORK_FS=1` opens the store anyway**; sessions inherit it like other `INTERLOCK_*` variables, so their hooks can open it too. A store directory not made yet is judged by the nearest directory above it. What is refused:

- **On Linux**, by `statfs` magic number: NFS, SMB, SMB2, CIFS, AFS, Coda, NCP, Ceph, Lustre, GPFS, BeeGFS, OrangeFS, OCFS2 and GFS2. For FUSE, by the mount's type in `/proc/self/mountinfo`: `fuse.sshfs`, `fuse.rclone`, `fuse.s3fs`, `fuse.gcsfuse`, `fuse.gvfsd-fuse`, `fuse.mfs` (MooseFS), `fuse.lizardfs`, `fuse.s3ql`, `fuse.davfs`, `fuse.keybase`, `fuse.fuse-nfs` and the others listed in `crates/interlock-store/src/netfs.rs`. A FUSE filesystem layered on a directory, such as gocryptfs or bindfs, which name that directory as their mount's source, is judged by that directory too, up to four layers: gocryptfs over sshfs is refused. A layer whose source is not a path, such as encfs, is judged by its own type only, so encfs over sshfs is not caught. Other FUSE mounts (a container's fuse-overlayfs, say), 9p and virtiofs count as local.
- **On macOS**, by the type name `statfs` reports: `nfs`, `smbfs`, `afpfs`, `webdav`, `ftp`, `cifs`. macFUSE mounts report `macfuse` or `osxfuse` whatever they serve, so sshfs on macOS is not caught. This path is compiled for macOS here but has not run on it.
- **Elsewhere** nothing is checked.

The tests cover the classification of magic numbers, mount types and layers, the refusal's wording, and that a local directory opens. No network mount could be made in the development container, so a refusal of a real NFS or SMB mount has not been observed. As a manual check, FUSE filesystems served locally were mounted with chosen types: `fuse.sshfs` was refused with exit 2 and passed the check with the override, `fuse.fuse-overlayfs` passed it, and a `fuse.gocryptfs` mount whose source was the `fuse.sshfs` one was refused ([evidence](../evidence/runtime/README.md#8-no-store-on-a-network-filesystem)). That exercises `statfs` and the mount table on real mounts, not a network filesystem.

## Restart: re-attach or reconcile

When `interlock run` starts, it accounts for every session in the store, in any task, that has a handoff and no recorded end:

| Found | What happens |
| --- | --- |
| A handoff whose supervisor is still running (pid and start time match) | Left alone. If it is a running attempt of the task being run, `interlock run` refuses to start rather than run a second supervisor's work. |
| A running attempt of the task being run, whose host is alive (same pid, same kernel start time, not a zombie) | **Re-attach.** The supervisor notes the time in `handoff.reattached_at`, waits for the process, enforces the original deadline, honors cancellation, then reads the transcript and finishes the attempt exactly as the first supervisor would have: a worker's tree is submitted (G3), a verifier's attempt is ended and the task advanced. |
| Any other session: a running attempt whose host is gone, a running attempt of another task, or an attempt that `task cancel`, `retry` or `fail` closed while no supervisor ran | **Reconcile.** Whatever still runs is stopped (the process group, then everything carrying the session's marker; see containment). The end is recorded with a synthetic report: `crash` for a running attempt, `cancelled` for one the operator had already closed, `completed` for one whose result was accepted. Wall-clock use is the time to the transcript's last write; a host that finished reported its cost there. The token is dropped, and a worker's unfinished work is salvaged to `interlock/wip/<task>`. |
| A running attempt of the task being run with no handoff | The supervisor died before the session started: `crash`, synthetic. |

Then every task left running gets R3. Only after this is `.interlock/config.toml` read, so a broken config never stands in the way of recovery; its error is reported then, and the run stops.

`interlock task cancel`, `retry` and `fail` do the same for their task when they find a session whose supervisor is gone: they stop it, record why it ended, and print the attempt ids under `stopped_sessions`. A live supervisor's watcher stops its own session within about 250 ms instead.

A re-attaching supervisor needs the attempt's token to submit its result. The store keeps only token hashes, so the supervisor keeps each running attempt's token in a per-user directory outside the repository, `$XDG_RUNTIME_DIR/interlock/<hash>/` or `/tmp/interlock-<uid>/<hash>/` (directories mode 0700, files 0600), and deletes it when the attempt ends. If that file is lost, the live session is stopped and reconciled rather than left running unseen.

The exit code of a process the supervisor did not start cannot be read, so a re-attached session's transcript decides: a host that ended with its `result` event is judged by it, and one that ended without it died, which is a `crash`.

## Stopping a session

| Trigger | Session | Attempt | Task | `interlock run` exits |
| --- | --- | --- | --- | --- |
| SIGINT, SIGTERM or SIGHUP to `interlock run` | Process group gets SIGTERM, then SIGKILL after 3 s | `cancelled`, reason `cancelled` | R3 back to ready (a verifier's task stays awaiting verification) | 6, with the report on stdout and `{"interrupted": "<first signal>", "resume": "interlock run ..."}` on stderr |
| The same, during a baseline check | The check's process group is stopped; the cancelled run is not recorded | none opened | stays ready | 6 |
| `interlock task cancel <task>` from another terminal | Seen within about 250 ms, then stopped the same way | `cancelled`, reason `cancelled` | cancelled | 5 |
| Anyone else ends the attempt (`task retry`, `task fail`) | Stopped the same way | keeps the status they gave it | as they left it | |
| Session timeout | Stopped | `failed`, reason `timeout` | R3, or failed when the attempts are spent | |

The terminal sends Ctrl-C to the supervisor's process group, never to the host's, so the host is always stopped by the supervisor and the attempt is always recorded. Further signals do nothing more: exiting early would leave the host running unseen, and stopping it takes at most a few seconds. The report names the first signal that arrived. Signals are handled with `signal-hook`; the workspace still forbids unsafe code.

A cancelled worker's unfinished work is exported before its worktree is removed (see pausing safely below).

## Containment: what holds and what does not

Tokens are bookkeeping, not security. Any process running as the same user can read another process's environment through `/proc/<pid>/environ`, and every session's environment holds its attempt's token; it can read the token directory too. Containment narrows what an agent can reach; the operating system's user boundary is the only hard line.

What interlock does:

- **Every process of a session carries a marker**, `INTERLOCK_SESSION=<attempt id>`, inherited by everything the host starts.
- **The supervisor is a child subreaper** (Linux `PR_SET_CHILD_SUBREAPER`, through the `nix` crate). A session's process that loses its parent is re-parented to the supervisor instead of to init.
- **When a session ends**, interlock kills its process group, then every process carrying its marker, then every orphan the supervisor adopted that started after the session did, repeating until nothing is left. The run report lists them under `sessions[].stopped_strays`. So a process that escapes with `setsid`, or with `setsid` and an emptied environment, does not outlive its session, and cannot wait for the verifier to start and read its token: a worker's escaped process is gone before the verifier's session begins.
- **On restart**, and when `task cancel`, `retry` or `fail` find a session whose supervisor is gone, everything carrying the session's marker is killed, and its process group too if one of its members carries the marker (a bare group id may have been reused).
- **The hooks refuse paths into interlock's own state.** For every tool call, each path field and every path-like word of a shell command is resolved, `.` and `..` folded and symlinks followed, and the call is denied if it lands in the token directory or in any `/proc/*/environ`, or, for a headless session, in `.interlock/` outside the attempt's own worktree (which lives there). A guided session a person drives may read under `.interlock/`, where its skills keep their references; changing anything there is refused for every session. A command naming `INTERLOCK_DB` or `state.db` is denied too. The shell words are split on whitespace and punctuation, not parsed as shell grammar.
- **Workers may not `git commit`** (denied in the bug-fix, feature and refactor workflows, as verifiers already were); interlock records the worktree's files, and a worker's own commits are not its output.

What does not hold:

- A same-user process that clears its environment *and* escapes while no supervisor is alive (the supervisor was killed with SIGKILL) is re-parented to init, carries no marker, and is not found later.
- A process the session started can still read other processes' environments and the token directory if it gets past the hooks, for example with a shell construct the word splitter does not resolve (`$(dirname "$HOME")/…`, `eval`, a script file). The hooks deny what they can see; the hosts' own tool filters and the operating system do the rest.
- A supervisor killed with SIGKILL cannot stop its session: the host keeps running, and spending, until the next `interlock run`, `task cancel`, `retry` or `fail` finds it, or its recorded deadline passes.

## Each session's environment

A session does not inherit interlock's environment. It gets an allowlist, and the names it got are recorded on the handoff (`handoff.env`, names only):

| Who | Variables |
| --- | --- |
| Every session | `PATH` (with interlock's own directory first), `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `LC_*`, `TERM`, `TZ`, `TMPDIR`, `HTTPS_PROXY`, `HTTP_PROXY`, `NO_PROXY` and their lowercase forms, `SSL_CERT_FILE`, `NODE_EXTRA_CA_CERTS`, `REQUESTS_CA_BUNDLE` |
| Claude Code sessions | `ANTHROPIC_*`, and the provider-selection `CLAUDE_CODE_USE_*` |
| Copilot CLI sessions | `COPILOT_*` except the variables that bind a process to a running Copilot session (`COPILOT_LOADER_PID`, `COPILOT_SUPERVISED`, `COPILOT_RUN_APP`, `COPILOT_AGENT_SESSION_ID`, `COPILOT_CONNECTION_TOKEN`, `COPILOT_DETACHED_*`; Copilot hides the same ones from its own children), and the GitHub token variables Copilot signs in with: `COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN` |
| interlock | its own `INTERLOCK_*` variables, except the per-session ones it sets itself for each attempt (`INTERLOCK_DB`, `_ATTEMPT`, `_TOKEN`, `_MODE`, `_HOST`, `_TREE`, `_SESSION`) |
| The operator | anything under `[env] pass` in `.interlock/config.toml`: exact names, or prefixes ending in `*` |

So a session started from inside another agent no longer sees that agent's session id, message socket and token, ingress token file, account details, or effort setting, and a Claude Code session never sees `GH_TOKEN`. The host's own shell commands, and anything they start, inherit what the host was given, credentials included.

```toml
# .interlock/config.toml
[env]
pass = ["MY_TOOL_HOME", "PIP_*"]
```

**Effort.** `interlock run --effort <level>` passes the reasoning effort to every session: Claude Code's `--effort` (low, medium, high, xhigh, max) and Copilot CLI's `--reasoning-effort` (none, minimal, low, medium, high, xhigh, max). It is recorded on each attempt's handoff. Inspection reports it as the `effort_selection` capability, detected from each CLI's help; a host without it, or a level it does not take, is refused before anything starts.

## Why an attempt ended

Every session end is classified, recorded once on the attempt (`end.reason`, `end.detail`, `end.synthetic`), written as an event (`attempt.completed`, `attempt.failed` or `attempt.cancelled`, with the same fields and what was spent), and reported in the run report's `sessions[].ended_because`.

| Reason | When |
| --- | --- |
| `completed` | The host finished and reported success |
| `rejected` | It finished, but its result changed files it may not change |
| `timeout` | The session ran past its time limit |
| `host_error` | The host could not start, exited with a code, or reported an error. Any exit code from a host that ran is the host's own, including 97 |
| `auth_failure` | As `host_error`, with the host's own sign-in error: Claude Code's "Invalid API key", "Please run /login", "Not logged in", "OAuth token has expired", `authentication_failed`; Copilot CLI's "No authentication information found" (captured from 1.0.91). A tool's own 401 does not count |
| `crash` | The host was killed by a signal interlock did not send; a re-attached host ended without its final report; or its supervisor died and the session was gone, or was stopped, on restart |
| `cancelled` | A signal, `task cancel`, or another end of the attempt stopped it |
| `budget_exhausted` | The task's wall-clock budget set the deadline it hit, or the host stopped at its own cost cap (Claude Code's `error_max_budget_usd`) |

`interlock task events <task>` lists them. Each session in the run report also carries its `cost_usd` and `premium_requests`.

## Budgets

A task's `[budget]` may set, besides `max_attempts`:

```toml
[budget]
max_attempts = 3
max_wall_secs = 1800         # total session time, summed over attempts
max_cost_usd = 2.50          # where the host reports dollars (Claude Code's total_cost_usd)
max_premium_requests = 10    # where the host reports them (Copilot CLI's usage.premiumRequests)
```

Amounts must be finite and above zero; `nan` or `inf` is refused when the task is created. Each attempt records what its session used (`spent`: `wall_ms`, `cost_usd`, `premium_requests`, `turns`). Measures stay in each host's own unit; a limit applies only where a host reports that measure. The run report shows the task's total.

- **Wall clock** is enforced inside a session: its deadline is the session timeout or what is left of the budget, whichever is sooner.
- **Dollars** are passed to Claude Code as `--max-budget-usd <what is left>` (the shortest exact decimal, so a small remainder is not rounded to zero), which stops it mid-session. interlock also checks the total after every session.
- **Premium requests** are checked after every session. Copilot's own cap (`--max-ai-credits`) counts a different unit, so it is not used.

When any limit is reached the task fails, in one transaction, with the reason, for example `budget exhausted: the cost budget of $0.50 is spent ($0.6000 used)`. The supervisor also checks before starting each session.

## Pinning host versions

```toml
# .interlock/config.toml
[pins]
copilot = "1.0.91"
claude-code = "2.1.289"
```

When the run's host is pinned and the installed version differs, `interlock run` blocks the task before any session, with the reason (`copilot 1.0.92 is installed, but .interlock/config.toml pins 1.0.91; install the pinned version or change the pin`). Unblock it once the versions agree. `[pins]` may name only known hosts; anything else, or an empty version, is refused. `interlock host inspect` adds a `pin` object to each host: `unpinned`, `match`, `mismatch`, or `unknown` (pinned, but the version could not be read). Other top-level sections in the file are ignored, so it can be shared. The store's `.gitignore` ignores everything in `.interlock/`; commit the config with `git add -f .interlock/config.toml`.

## Pausing safely

```bash
interlock task export <task>                 # wip: commit on interlock/wip/<task>, plus a resume note
interlock task resume <task> [--from <rev>]  # the next worker starts from it
interlock run <task> --host <host>
```

`task export` reads the newest worker worktree that still exists (a live session's, mid-run) or else the last accepted output, and writes its files, through a temporary index, as a commit whose parent is the task's base commit. The message starts `wip: <task>: <intent>` and carries a resume note: where it came from, how to resume, and the brief `interlock brief` builds from records. The commit goes on `refs/heads/interlock/wip/<task>`, interlock's own ref; no other branch, the user's index, or the user's files change. The note is also written to `.interlock/exports/<task>.md`.

An interrupted or orphaned worker's work is exported the same way, automatically, when its files differ from the commit the attempt started at (recorded on the handoff). Comparing with that commit rather than the worktree's `HEAD` keeps work the worker committed itself.

`task resume` needs the task ready (for example after Ctrl-C), and the commit must descend from the task's base commit. It makes the export's tree the starting point of the next worker, the way the last accepted output already is for rework, and writes a `task.resumed` event with the commit, the tree, and the tree it replaced. It is not a move and changes no evidence: the tree satisfies nothing until a worker's result built on it is accepted, through G3 and G4 as usual. The worker's brief labels it "Starting tree, exported work in progress that no result has been accepted for", and its prompt says how the previous attempt ended.

## Telling the verifier what each criterion needs

The verifier's prompt lists, per criterion, the least it must record: "you must record at least `observed`. `observed` means you saw the behavior itself on a real surface (UI, CLI or API); watching interlock run this criterion's check on this tree and pass counts when the check exercises the behavior. Anything lower will not satisfy this criterion." `interlock assess add` (and `claim add`) answers a passing strength below the minimum with a `warning`, and the verifier's Stop hook holds it once, naming the criterion and the gap, and saying to record the stronger strength only if that is what it saw, and `failed` or `blocked` otherwise.

## Baseline only

`interlock run <task> --max-sessions 0` records the input snapshot (G1), runs the baseline checks, reports them, and stops without starting a session.

## Records

All additions are optional, so earlier records still validate.

- `attempt`: `handoff` (with `supervisor_start`, `start_commit`, `env`, `effort`, `reattached_at`), `end`, `spent`.
- `task.budget`: `max_wall_secs`, `max_cost_usd`, `max_premium_requests`.
- `event.type`: `attempt.cancelled`, `task.resumed`.
- Any move into done, failed, or cancelled closes the task's open attempts in the same transaction: running ones are cancelled and submitted ones, whose results were judged, complete.

## Tests

| What | Test |
| --- | --- |
| Supervisor restart, session alive | `run_robustness::a_supervisor_killed_mid_session_reattaches_and_carries_on` (Copilot): `kill -9` mid-session; the host keeps running; a second supervisor is refused; the restart re-attaches and the log is `G1 G2 G3 G4 G7`, with one worker session. `a_verifier_session_is_reattached_too` does the same mid-verification |
| Supervisor restart, session dead | `run_robustness::a_session_that_died_with_its_supervisor_is_reconciled_and_retried`: synthetic `crash` report and event, salvaged export, `G1 G2 R3 G2 G3 G4 G7`. `run_fake_host::a_dead_sessions_children_are_stopped_at_restart`: what the dead host left in its group is stopped |
| Restart covers every task | `run_fake_host::a_restart_for_one_task_ends_sessions_left_by_another` |
| Operator ends a task while no supervisor runs | `run_fake_host::task_cancel_stops_a_session_whose_supervisor_is_gone`: the session and its stray are stopped, the end recorded, the token dropped |
| Termination | `run_robustness::sigint_cancels_the_session_and_the_exported_work_resumes` (two quick SIGINTs; export, ancestry check, `task.resumed`, resume to done), `sigterm_stops_a_run_the_same_way`, `task_cancel_from_another_terminal_stops_the_running_session`; `run_fake_host::sighup_stops_a_run_like_sigterm`, `a_signal_during_a_baseline_check_stops_it_and_the_first_signal_is_reported`, `a_signal_while_the_host_is_inspected_opens_no_attempt` |
| Containment | `run_fake_host::processes_that_leave_the_session_are_stopped_when_it_ends` (`setsid`, and `setsid` with an emptied environment), `a_reattached_session_leaves_no_strays`, `an_escaped_worker_process_cannot_forge_the_verifiers_evidence`; `hook::tests::interlocks_own_directory_tokens_and_other_environments_are_off_limits`, `workers_may_not_commit`; `session::tests::a_process_that_leaves_the_group_is_found_by_its_marker` |
| Timeout | `run_robustness::a_session_timeout_retries_until_the_attempts_are_spent`: `G1 G2 R3 G2 fail` |
| Budgets, pins, config | `run_robustness::a_spent_wall_clock_budget_fails_the_task`, `a_spent_cost_budget_fails_the_task`, `a_host_version_that_differs_from_its_pin_blocks_the_run`; `run_fake_host::budgets_must_be_finite_amounts`, `a_bad_config_does_not_stand_in_the_way_of_recovery` |
| Pausing safely | `run_fake_host::pausing_safely_keeps_work_the_worker_committed` |
| Environment and effort | `run_fake_host::sessions_get_an_allowlisted_environment_and_the_effort_asked_for`, `an_effort_the_host_cannot_take_is_refused_before_anything_starts`; `env::tests` |
| Controller lock | `run_fake_host::only_one_supervisor_gets_the_lock_however_many_race`; `lock::tests` |
| A live supervisor's session is left to it | `sessions::tests::a_session_whose_supervisor_still_runs_is_left_to_it` (a reused pid does not count); `run_robustness::task_cancel_from_another_terminal_stops_the_running_session` (`stopped_sessions` is empty) |
| Verifier guidance | `hook::tests::a_verifier_is_held_when_its_assessment_is_weaker_than_the_criterion_needs`; `cli::assessments_weaker_than_the_criterion_needs_come_back_with_a_warning` |
| Baseline only | `run_fake_host::max_sessions_zero_runs_the_baseline_only` |
| A host query that hangs | `probe::tests::a_query_that_hangs_once_is_tried_again` |
| Every started attempt ends in a terminal row, with a reason | Every `run_robustness` test ends with it; `sessions::every_session_without_an_end_is_listed_whatever_its_status_or_task` |
| Late completion | `sessions::reconcile_attempt_ends_a_running_attempt_with_a_synthetic_report` (a reconciled worker's late result is superseded), with the existing invariant 4 tests |
| Policy denial, denied tool unavailable | The existing Copilot test `the_stop_guard_holds_the_worker_and_hooks_deny_what_it_was_not_granted` |

Every `run_fake_host` test was also run against the build before these fixes (with `INTERLOCK_TEST_BIN`), where all of them fail; see the evidence.

Recorded runs are in [`evidence/runtime/`](../evidence/runtime/README.md). They include two live Claude Code sessions whose supervisor was killed and restarted. After the fixes, the second one re-attached and reached `done`: one worker, one verifier, $0.127.

## Limits

- Liveness uses `/proc` (Linux). Elsewhere it falls back to `kill -0`, which cannot tell a reused pid from the session, and the subreaper and marker sweep do nothing.
- A session that finished while no supervisor was watching is reconciled as a failure, even if its transcript shows it completed. Its work is salvaged to `interlock/wip/<task>`, and `task resume` can pick it up.
- Cost and premium-request limits are checked between sessions; only Claude Code enforces a dollar cap mid-session. A Copilot session can overrun its premium-request budget before it ends.
- Budgets are enforced by `interlock run`. `interlock attempt start`, used by hand, does not check them.
- `task resume` is not a move, so it is not in `task log`; the `task.resumed` event in `task events` records it.
- A worker whose result is awaiting verification stays `submitted`, an open attempt, until the task is decided.
- The containment limits above.
