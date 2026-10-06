# Evidence: forge adapter, pinned delivery, operation reconcile

Runtime evidence for `interlock-forge` (see [docs/forge.md](../../docs/forge.md)), collected October 5–6, 2026 in the development container.

**Live GitHub was not reachable.** `gh` 2.89.0 is installed, but `gh auth status` reports that the container's GH_TOKEN is invalid ([gh-auth-status.txt](gh-auth-status.txt)). Every delivery below ran the real `GhForge` (its actual `gh` and `git` command lines) against a fake `gh` and a local bare git repository standing in for GitHub. Nothing here was merged on github.com. What a live run would add is listed in [docs/forge.md](../../docs/forge.md#what-a-live-run-would-add).

All commands run from `interlock/` with:

```bash
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0
export INTERLOCK_COPILOT_BIN=<path to Copilot CLI 1.0.91>/node_modules/.bin/copilot
```

## Index

| # | What | Command | Outcome | Raw output |
| --- | --- | --- | --- | --- |
| 1 | Live GitHub access | `gh auth status` | Exit 1: "Failed to log in to github.com using token (GH_TOKEN) ... The token in GH_TOKEN is invalid." | [gh-auth-status.txt](gh-auth-status.txt) (redacted; no token was printed) |
| 2 | Whole workspace | `cargo fmt --all --check`; `cargo clippy --workspace --all-targets`; `cargo test --workspace` | fmt clean; clippy 0 warnings; 150 passed, 0 failed, 1 ignored (the opt-in live test, row 5) | [cargo-test-workspace.txt](cargo-test-workspace.txt) |
| 3 | Fault tests as processes (invariant 7, P4 gates) | `INTERLOCK_EVIDENCE_OUT=$PWD/evidence/forge/fault-tests cargo test -p interlock-cli --test forge_cli` | 6 passed | [fault-tests/](fault-tests/), one folder per scenario |
| 4 | Walkthrough step 7 on Copilot CLI (offline, scripted model) | `INTERLOCK_EVIDENCE_OUT=$PWD/evidence/forge/copilot-e2e cargo test -p interlock-cli --test deliver_copilot` | 3 passed | [copilot-e2e/](copilot-e2e/), one folder per scenario |
| 5 | Live run on Claude Code 2.1.289 with a real model | `INTERLOCK_LIVE=1 INTERLOCK_LIVE_OUT=$PWD/evidence/forge/live-claude-code cargo test -p interlock-cli --test deliver_live -- --ignored --nocapture` | 1 passed in 27.3 s: G1–G6 | [live-claude-code/](live-claude-code/) |

The tests behind rows 3–5 also assert everything described below; the JSON files are what interlock printed or stored.

## 3. Fault tests as processes

Each folder holds the `interlock` command's JSON output, the task's transition log (`transitions.json`), its operation rows (`operations.json`), and every call the fake `gh` received (`gh-calls.json`).

| Scenario | What happened |
| --- | --- |
| [invariant-7-crash-then-reconcile](fault-tests/invariant-7-crash-then-reconcile/) | The fake `gh` merged, then SIGKILLed `interlock integrate run` before it heard back ([1-integrate-run-killed.json](fault-tests/invariant-7-crash-then-reconcile/1-integrate-run-killed.json): signal 9, no output). Main already holds a merge of the verified head, and the merge row still says `started`. `interlock reconcile fix-add` asked the forge, found the merged head, and applied G6 ([2-reconcile.json](fault-tests/invariant-7-crash-then-reconcile/2-reconcile.json)). One `gh pr merge` call in total |
| [invariant-7-crash-then-restarted-run](fault-tests/invariant-7-crash-then-restarted-run/) | Same crash, then a fresh `interlock run fix-add --host copilot`. Its startup reconcile settled the merge (G6) before the loop, so the run finished with no session ([2-interlock-run.json](fault-tests/invariant-7-crash-then-restarted-run/2-interlock-run.json): `reconciled` lists the operation) |
| [p4-moved-head](fault-tests/p4-moved-head/) | Someone pushed to `interlock/fix-add` while the merge request was in flight. The call carried `--match-head-commit <verified head>`, so GitHub (the fake) refused it: "Head branch was modified". interlock applied R2; main never moved ([remote.json](fault-tests/p4-moved-head/remote.json)) |
| [p4-changed-base](fault-tests/p4-changed-base/) | Main moved after verification. interlock recorded the tree that would land on the new base, failed the landing operation, and applied R2. Every criterion's evidence is now stale (`status-after.json`: `all_pass: false`, `stale > 0`), and no merge was attempted |
| [p4-no-landing-authority](fault-tests/p4-no-landing-authority/) | No grant: G5 blocked the task at verified with "no landing authority is granted for this task", and its next move is back to verified. The fake `gh` received no call |
| [integrate-run-delivers](fault-tests/integrate-run-delivers/) | The plain path: G5, push, PR #1, readiness, `gh pr merge 1 --merge --match-head-commit <head>`, G6. `integrate-run.json` lists each step |

## 4. Walkthrough step 7 on Copilot CLI

A bug-fix task (`add()` returns the difference) with `integration_required = true`. The whole task runs through `interlock run fix-add --host copilot` on the real Copilot CLI 1.0.91, offline against the scripted model in `crates/interlock-cli/tests/fake_model`, with the fake `gh` and a bare repository as `origin`.

| Scenario | Moves | What landed |
| --- | --- | --- |
| [copilot-walkthrough-step-7](copilot-e2e/copilot-walkthrough-step-7/) | G1 G2 G3 G4 G5 G6 (worker and verifier sessions, then delivery) | Main gained a merge whose second parent is the verified head `380c855e…`: the verified tree on the snapshot base. The merge was made with `--match-head-commit 380c855e…` |
| [copilot-no-landing-authority](copilot-e2e/copilot-no-landing-authority/) | G1 G2 G3 G4 block | Nothing; `stopped_because`: "blocked: no landing authority is granted for this task"; no `gh` call |
| [copilot-run-killed-mid-merge-then-restarted](copilot-e2e/copilot-run-killed-mid-merge-then-restarted/) | G1 G2 G3 G4 G5 (run killed by the fake `gh` mid-merge), then G6 from the second run | The second run reconciled the open merge operation, made no session and no model call, and never merged twice |

The verified head is the same commit, `380c855e…`, in both runs that delivered, though they used separate repositories: the commit is deterministic.

## 5. Live run on Claude Code

The design's own walkthrough, `examples/export-retry` ("Fix duplicate rows when an export retries"), with `integration_required = true` and an operator grant with landing authority. It ran through `interlock run export-retry --host claude-code` with a real model, then delivered to the fake `gh` and a bare repository. One live run was made.

| Step | Record |
| --- | --- |
| Baseline | `repro` failed on the input snapshot (exit 1), `regression` passed |
| Worker, 16.4 s | Fixed `export` to resume after the last row written; claims for both criteria ([run-report.json](live-claude-code/run-report.json)) |
| Verifier, 9.0 s | Had interlock run both checks on the same tree; assessments for both |
| Delivery | G5, push to `interlock/export-retry`, PR #1, `gh pr merge 1 --merge --match-head-commit 72f74093…`, G6 ([gh-calls.json](live-claude-code/gh-calls.json), [operations.json](live-claude-code/operations.json)) |
| Landed | Main is a merge of `72f74093…` onto the snapshot base `f267177f…` ([summary.json](live-claude-code/summary.json)); the diff from the base is [verified-head.diff](live-claude-code/verified-head.diff) |

Transitions: G1, G2, G3, G4, G5, G6 ([transitions.json](live-claude-code/transitions.json)). The session transcripts stayed in the test's temporary directory and are not kept here.
