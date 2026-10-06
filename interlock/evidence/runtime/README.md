# Runtime evidence

What was run for the execution-robustness work ([docs/runtime.md](../../docs/runtime.md)), October 5 and 6, 2026, in the development container: Copilot CLI 1.0.91 (offline, scripted model) and Claude Code 2.1.289 (real model). No file here holds a token or credential; attempt tokens never leave `.interlock/`.

## 1. The workspace test suite

```bash
cd interlock
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0
export INTERLOCK_COPILOT_BIN=<path to node_modules/.bin/copilot, 1.0.91>
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

Outcome: formatting clean, clippy with no warnings, 141 tests passed and none failed. The Copilot suites took 45 s (`run_copilot`, 6 tests) and 120 s (`run_robustness`, 10 tests), so they ran on the real CLI rather than skipping. Raw output: [`cargo-test-workspace.txt`](cargo-test-workspace.txt).

The tests this work added, by file:

| File | Tests | What they run |
| --- | --- | --- |
| `crates/interlock-cli/tests/run_robustness.rs` | 10 | Whole `interlock run` processes: `kill -9` and restart with the session alive (worker and verifier) and dead, SIGINT twice, SIGTERM, `task cancel` from another process, timeout, wall-clock and cost budgets, version pins, export and resume |
| `crates/interlock-store/tests/sessions.rs` | 8 | `reconcile_running` (no tests before), handoffs, classified ends, spending, budgets, closing attempts on terminal moves |
| `crates/interlock-adapter/src/session.rs` | 8 new | Real processes: the gate, an abandoned gate, a missing binary, `attach`, pid reuse, group stop, crash, sign-in failure and cost-cap classification |
| `crates/interlock-core/tests/budget.rs` | 3 | Budget totals and limits, `resume_from` |
| `crates/interlock-schema/tests/conformance.rs` | 2 new | `handoff`, `end`, `spent`, budget limits, `attempt.cancelled` |
| `crates/interlock-supervisor/src/config.rs` | 1 | Pins read and compared |
| `crates/interlock-adapter/src/{copilot,claude_code}.rs` | 1 new, 3 extended | Claude's cost-cap result; `--session-id`, `--max-budget-usd`, Copilot's premium requests |

## 2. Live: kill the supervisor during a real Claude Code session

```bash
sh live-claude-reattach/live_reattach.sh <evidence dir> <scratch dir> 6
```

The script builds a one-file repository with the `fix-add` bug-fix task (a $1.00 cost budget, Claude Code pinned to 2.1.289), starts `interlock run fix-add --host claude-code --model haiku`, waits for the worker's handoff, lets the session run 6 s, `kill -9`s the supervisor, checks the host process, and starts `interlock run` again.

| Time (UTC) | What happened | File |
| --- | --- | --- |
| 00:08:31 | Supervisor 1 (pid 6931) starts; the worker's handoff is recorded, Claude Code is pid 7001 in its own group | [`00-timeline.txt`](live-claude-reattach/00-timeline.txt) |
| 00:08:38 | Before the kill: pid 7001's parent is 6931. `kill -9 6931`, exit status 137 | [`04-ps-before-kill.txt`](live-claude-reattach/04-ps-before-kill.txt) |
| 00:08:39 | Pid 7001 still running, now a child of pid 1. The attempt is `running`, with its handoff: pid, group, kernel start time, deadline, transcript, and the session id interlock chose | [`05-ps-after-kill.txt`](live-claude-reattach/05-ps-after-kill.txt), [`06-attempts-while-no-supervisor.json`](live-claude-reattach/06-attempts-while-no-supervisor.json) |
| 00:08:39 | Supervisor 2 starts, finds the session alive, and re-attaches (`handoff.reattached_at`) | [`07-run-2-report.json`](live-claude-reattach/07-run-2-report.json): `"reattached": ["att-1eec0a79aaa645d1"]`, `"reconciled": []` |
| 00:09:03 | The worker session ends (31.6 s, $0.13, 14 turns); supervisor 2 submits its tree and G3 accepts it, as the first supervisor would have. The output fixes `add()`; the user's branch and files are untouched | [`08-task-log.json`](live-claude-reattach/08-task-log.json), [`12-output-and-user-branch.txt`](live-claude-reattach/12-output-and-user-branch.txt) |
| 00:09:51 | Two verifier sessions run; the task is blocked | [`10-events.json`](live-claude-reattach/10-events.json) |

The task log is `G1 G2 G3 block`: no R3 and no second worker attempt, because the restarted supervisor carried on with the session that was already running. Claude Code's own `session_id` in the transcript matches the id in the handoff (`3a0d88ab-…`), which interlock passed with `--session-id`. `host inspect` and the run report show the pin as `match`, and the run report totals the spending recorded on each attempt. (That `--max-budget-usd` reaches the host is shown by `a_spent_cost_budget_fails_the_task`, not by this run.)

**Why it ended blocked, not done.** Both haiku verifiers ran interlock's checks (both passed) and then assessed the `verified` criterion as `tested`. That criterion requires `observed`, so G4 correctly refused, and after two verifiers without decisive evidence the supervisor blocked the task. This is the evidence policy working, and unrelated to the restart. The verifier tool calls are in [`transcripts/att-12e9a17dbcd643f2.jsonl`](live-claude-reattach/transcripts/att-12e9a17dbcd643f2.jsonl).

Supervisor 1's report files (`03-run-1-*`) are empty: it was killed before printing. Total model spend for this demo: $0.289 (worker $0.132, verifiers $0.102 and $0.054), plus $0.0009 for the probe below.

## 3. Host probes

| Probe | Command | Outcome | Raw output |
| --- | --- | --- | --- |
| Copilot reports premium requests and honors `--session-id` | [`host-probes/copilot-probe.sh`](host-probes/copilot-probe.sh): `copilot -p "say done" --output-format json --session-id <uuid> --usage-output-file …` against [`copilot-probe-model.py`](host-probes/copilot-probe-model.py), offline | Exit 0; the `result` event carries the given `sessionId` and `usage.premiumRequests` (0 offline) | [`copilot-offline-session.jsonl`](host-probes/copilot-offline-session.jsonl), [`copilot-usage-output.json`](host-probes/copilot-usage-output.json) |
| Claude Code's cost cap and session id | `echo "Reply with the single word ok." \| claude -p --output-format stream-json --verbose --model haiku --max-budget-usd 0.000001 --setting-sources "" --no-session-persistence --session-id 6f1c0b8e-…` | Exit 1; `subtype: error_max_budget_usd`, `terminal_reason: budget_exhausted`, `is_error: true`, `total_cost_usd: 0.000944`, the given session id. Only the `result` event is kept | [`claude-max-budget-result.json`](host-probes/claude-max-budget-result.json) |

These shaped the adapters: Copilot's `usage.premiumRequests` is read as spending, Claude Code's `terminal_reason` and `subtype` become the summary's `stop_reason`, and a `budget_exhausted` stop is classified as `budget_exhausted`.
