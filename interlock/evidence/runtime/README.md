# Runtime evidence

These are the runs behind the execution-robustness work ([docs/runtime.md](../../docs/runtime.md)), done October 5 and 6, 2026, in the development container. The hosts were Copilot CLI 1.0.91, offline against a scripted model, and Claude Code 2.1.289 with a real model.

No file here holds a credential or an attempt token:

- The store keeps only token hashes. The supervisor keeps the live token in a per-user runtime directory outside the repository and deletes it when the attempt ends.
- Paths under this container's scratch directory are shown as `<scratch>`.

Two rounds are recorded:

- **Round 1:** the first implementation (commits `5d0d894` to `57b0cdb`).
- **Round 2:** the fixes after review (`9e587d5` onward).

Where round 1's notes no longer match the code, they are corrected below.

## 1. The workspace test suite

```bash
cd interlock
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0
export INTERLOCK_COPILOT_BIN=<path to node_modules/.bin/copilot, 1.0.91>
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

Outcome at the end of round 2 (01:47 UTC, October 6, cargo 1.97.0):

- Formatting is clean, and clippy reports no warnings.
- **172 tests passed, none failed, none ignored**, across 24 test binaries (doc tests included).
- The Copilot suites ran on the real CLI rather than skipping: `run_copilot` (6 tests) took 34 s and `run_robustness` (10 tests) took 99 s.
- `run_fake_host` ran 16 tests in 11 s.

Raw output: [`cargo-test-workspace.txt`](cargo-test-workspace.txt).

The tests this work added, by file:

| File | Tests | What they run |
| --- | --- | --- |
| `crates/interlock-cli/tests/run_fake_host.rs` | 16 | Whole `interlock run` processes against a scripted stand-in for Claude Code. They cover: processes that leave the session (`setsid`, double fork, an emptied environment); an escaped worker process that tries to forge the verifier's evidence; restart across tasks; `task cancel` with no supervisor; signals during the baseline and during inspection; SIGHUP; pausing safely with worker commits; config read after recovery; the environment allowlist and effort; `--max-sessions 0`; the OS lock under a race; finite budgets |
| `crates/interlock-cli/tests/run_robustness.rs` | 10 | Whole `interlock run` processes on the real Copilot CLI: `kill -9` and restart with the session alive (worker and verifier) and dead; SIGINT twice; SIGTERM; `task cancel` from another process; timeout; wall-clock and cost budgets; version pins; export, ancestry check and resume. The harness waits for a marked process inside the session, not for a fixed time, and fails at once, with the run's output, if `interlock run` exits early |
| `crates/interlock-store/tests/sessions.rs` | 8 | `reconcile_attempt` and `unended_sessions`, handoffs, classified ends, spending, budgets, closing attempts on terminal moves, the `task.resumed` event |
| `crates/interlock-adapter/src/session.rs` | 12 new | Real processes: the gate, an abandoned gate, a missing binary, `attach`, pid reuse, group stop, crash, sign-in failures (Claude Code's wording, and Copilot's captured message; a bare 401 is not one), a host's own exit 97, a cleared environment, a stray found by its marker, a re-attached host that died without its report |
| `crates/interlock-adapter/src/env.rs` | 3 | The allowlist per host, Copilot's session-binding variables, `[env] pass` |
| `crates/interlock-adapter/src/probe.rs` | 1 new | A `--help` that hangs once is tried again |
| `crates/interlock-supervisor/src/hook.rs` | 3 new | Paths into `.interlock/`, the token directory and `/proc/*/environ` are refused (symlinks resolved); workers may not commit; a verifier whose pass is too weak is held |
| `crates/interlock-supervisor/src/lock.rs` | 2 | The controller lock is exclusive; a stale pid in the file does not matter |
| `crates/interlock-supervisor/src/sessions.rs` | 1 | A session whose supervisor still runs is left to it; a reused pid does not count |
| `crates/interlock-cli/tests/cli.rs` | 1 new | `assess add` warns when a pass is weaker than the criterion needs |
| `crates/interlock-core/tests/budget.rs` | 3 | Budget totals and limits, `resume_from` |
| `crates/interlock-schema/tests/conformance.rs` | 2 new | `handoff`, `end`, `spent`, budget limits, the new event types |
| `crates/interlock-supervisor/src/config.rs` | 1 | Pins read and compared; unknown hosts refused |
| `crates/interlock-adapter/src/{copilot,claude_code}.rs` | 1 new, 3 extended | Claude Code's cost-cap result; `--session-id`, `--max-budget-usd`, `--effort` and `--reasoning-effort`; Copilot's premium requests |

## 2. The round-2 tests fail on the round-1 build

The round-1 build was checked out from `57b0cdb`, and `run_fake_host.rs` was run against it with `INTERLOCK_TEST_BIN` pointing at that build's binary. All 16 tests fail, each on the behavior it targets. Some examples:

- A stray process is still alive after its session ends.
- A forger process steals the verifier's token.
- A stale lock file naming a live pid keeps all six racing supervisors out.
- `--effort` is an unknown argument.
- SIGHUP kills the run without a report.
- A NaN budget is accepted. Log: [`run-fake-host-before-fixes.txt`](run-fake-host-before-fixes.txt).

## 3. `run_robustness`, three times in a row

```bash
cargo test -p interlock-cli --test run_robustness   # three times, back to back
```

On the final code, starting at 01:42:28 UTC, the suite passed three times in a row: 10 of 10 each time, in 102 s, 98 s and 98 s. The workspace gate above ran it a fourth time (10 of 10).

An earlier set of three, run while the last assertions were still being added, also passed 10 of 10 each time. Its logs were overwritten by the final set.

Logs: [`run-robustness-x3/`](run-robustness-x3/).

## 4. An intermittent failure, found and fixed

`a_session_that_died_with_its_supervisor_is_reconciled_and_retried` failed in run 11 of a loop of 12 ([`flake/restart-test-blocked-run.txt`](flake/restart-test-blocked-run.txt)).

The restarted supervisor's worker ran fine, but the task was then blocked with "no independent verifier: host is missing: tool restriction …; start a headless session; …". So the host's help text had come back empty. The run also took 38 s instead of about 12, which matches the 30 s probe timeout running out.

Running `copilot --help` alone, 80 times, gave one call that never finished and was killed after 40 s ([`flake/copilot-help-timing.txt`](flake/copilot-help-timing.txt), script [`flake/copilot-help-timing.sh`](flake/copilot-help-timing.sh)). The others took about 1 s each.

The fix has two parts:

- `--version` and `--help` are retried, three tries of 10 s each.
- The supervisor inspects the host once per run instead of before every session.

`probe::tests::a_query_that_hangs_once_is_tried_again` fails with a single try and passes with the retry. After the fix the same test passed 20 times in a row ([`flake/restart-test-20-runs-after-fix.txt`](flake/restart-test-20-runs-after-fix.txt)). Run 6 of the 20 took 23 s instead of about 11. That would fit one retried query, but the run kept no record of retries, so this is not confirmed.

## 5. Live: kill the supervisor during a real Claude Code session

```bash
sh live-claude-reattach/live_reattach.sh <evidence dir> <scratch dir> 6
```

The script:

1. builds a one-file repository with the `fix-add` bug-fix task (a $1.00 cost budget, Claude Code pinned to 2.1.289);
2. starts `interlock run fix-add --host claude-code --model haiku`;
3. waits for the worker's handoff, then lets the session run 6 s;
4. `kill -9`s the supervisor and checks the host process;
5. starts `interlock run` again.

It was run twice, once per round.

### Round 2 (after the fixes): reaches done

[`live-claude-reattach-2/`](live-claude-reattach-2/)

| Time (UTC) | What happened | File |
| --- | --- | --- |
| 01:34:11 | Supervisor 1 (pid 1140) starts. The worker's handoff is recorded: Claude Code is pid 1354, in its own group | [`00-timeline.txt`](live-claude-reattach-2/00-timeline.txt) |
| 01:34:18 | Before the kill, pid 1354's parent is 1140. `kill -9 1140` exits with status 137 | [`04-ps-before-kill.txt`](live-claude-reattach-2/04-ps-before-kill.txt) |
| 01:34:19 | Pid 1354 is still running, now a child of pid 1. The attempt is `running`, and its handoff holds the pid, group, kernel start time, supervisor pid and start time, starting commit, deadline, transcript, and the session id interlock chose | [`05-ps-after-kill.txt`](live-claude-reattach-2/05-ps-after-kill.txt), [`06-attempts-while-no-supervisor.json`](live-claude-reattach-2/06-attempts-while-no-supervisor.json) |
| 01:34:19 | Supervisor 2 starts, finds the session alive, and re-attaches (`handoff.reattached_at`) | [`07-run-2-report.json`](live-claude-reattach-2/07-run-2-report.json): `"reattached": ["att-d28b8d7fbcb54669"]`, `"reconciled": []` |
| 01:34:47 | The worker session ends: 34.6 s, $0.076, 15 turns, `completed`, no strays. Supervisor 2 submits its tree, and G3 accepts it | [`08-task-log.json`](live-claude-reattach-2/08-task-log.json) |
| 01:35:10 | One verifier session (22.8 s, $0.051) runs both checks. It records `tested` for `fixed` and `observed` for `verified`, and G4 and G7 take the task to **done** | [`10-events.json`](live-claude-reattach-2/10-events.json), [`transcripts/att-39aa318653194c0a.jsonl`](live-claude-reattach-2/transcripts/att-39aa318653194c0a.jsonl) |

The outcome:

- The task log is `G1 G2 G3 G4 G7`: one worker attempt, one verifier, and no R3.
- The output tree's `calc.py` returns `a + b`. The user's branch and `calc.py` are untouched, and `git status` is clean ([`12-output-and-user-branch.txt`](live-claude-reattach-2/12-output-and-user-branch.txt)).
- Claude Code's own `session_id` in the transcript matches the handoff's `host_session_id`.
- The pin shows `match`.
- Each attempt's `handoff.env` lists only allowlisted names: `PATH`, `HOME`, `SHELL`, `TERM`, the proxy and CA variables, `ANTHROPIC_BASE_URL`, `CLAUDE_CODE_USE_CCR_V2`, and interlock's own `INTERLOCK_*`. None of the parent agent's session variables are there, so Claude Code signs in with the allowlisted environment alone.

The verifier recorded `observed` on its first try, so the Stop hold was not needed. Total spend: **$0.127** (worker $0.076, verifier $0.051).

### Round 1 (before the fixes): blocked

[`live-claude-reattach/`](live-claude-reattach/)

The restart worked the same way: the host survived the `kill -9`, the second supervisor re-attached, and G3 accepted the worker's tree, with the task log `G1 G2 G3 block`.

Both haiku verifiers ran the checks (both passed) and then recorded `tested` for `verified`, which requires `observed`. G4 refused, and after two verifiers the supervisor blocked the task. Round 2's verifier guidance addresses this: each criterion's minimum and the meaning of `observed` go in the prompt, `assess add` warns, and the Stop hold names the gap.

Spend: $0.289 (worker $0.132, verifiers $0.102 and $0.054). That run's note said tokens "never leave `.interlock/`". They are no longer kept there; see the top of this page.

## 6. Host probes

| Probe | Command | Outcome | Raw output |
| --- | --- | --- | --- |
| Copilot reports premium requests and honors `--session-id` | [`host-probes/copilot-probe.sh`](host-probes/copilot-probe.sh), offline against [`copilot-probe-model.py`](host-probes/copilot-probe-model.py) | Exit 0. The `result` event carries the given `sessionId` and `usage.premiumRequests` (0 offline) | [`copilot-offline-session.jsonl`](host-probes/copilot-offline-session.jsonl), [`copilot-usage-output.json`](host-probes/copilot-usage-output.json) |
| Claude Code's cost cap and session id | `echo "Reply with the single word ok." \| claude -p --output-format stream-json --verbose --model haiku --max-budget-usd 0.000001 --setting-sources "" --no-session-persistence --session-id 6f1c0b8e-…` | Exit 1, with `subtype: error_max_budget_usd`, `terminal_reason: budget_exhausted`, `is_error: true`, `total_cost_usd: 0.000944`, and the given session id. Only the `result` event is kept | [`claude-max-budget-result.json`](host-probes/claude-max-budget-result.json) |
| Copilot's sign-in failure | [`host-probes/copilot-auth-failure.sh`](host-probes/copilot-auth-failure.sh): an empty home and no token variables; no model is called | Exit 1, "Error: No authentication information found." This is the wording `auth_failure` matches for Copilot | [`copilot-auth-failure.txt`](host-probes/copilot-auth-failure.txt) |

These probes shaped the adapters:

- Copilot's `usage.premiumRequests` is read as spending.
- Claude Code's `terminal_reason` and `subtype` become the summary's `stop_reason`, and a `budget_exhausted` stop is classified as `budget_exhausted`.
- A sign-in failure needs the host's own wording.

Before the environment allowlist was written, two console probes ran Claude Code with a reduced environment: one scrubbed of the parent agent's session variables ($0.039), and one with only the allowlist ($0.016). Both signed in. Their output was not kept; the round-2 live run above shows the same thing with records.

Model spend for this work, about $0.53 in all:

- **Round 1, $0.344:** the live demo ($0.289), the cost-cap probe ($0.0009), and two earlier headless probes, one with the full environment and one with a scrubbed one ($0.030 and $0.025). Round 1's notes left those two out, and their output is not kept.
- **Round 2, $0.183:** the two environment probes ($0.039 and $0.016) and the live demo ($0.127).
