# Evidence: forge adapter, pinned delivery, operation reconcile

Runtime evidence for `interlock-forge` (see [docs/forge.md](../../docs/forge.md)), collected October 5–6, 2026 in the development container. Rows 2–5 were regenerated on October 6 from the code in the commit that adds this file.

Row 6 ran after the review fixes, but before one last change: G5 now fails landings planned before an R2 ("superseded before any call"). That run's path never reaches the change, because nothing was planned before its G5.

**Live GitHub was not reachable.** `gh` 2.89.0 is installed, but `gh auth status` reports that the container's GH_TOKEN is invalid ([gh-auth-status.txt](gh-auth-status.txt)). Every delivery below ran the real `GhForge`, with its actual `gh` and `git` command lines, against a fake `gh` and a local bare git repository standing in for GitHub. Nothing here was merged on github.com. What a live run would add is listed in [docs/forge.md](../../docs/forge.md#what-a-live-run-would-add).

All commands run from `interlock/` with:

```bash
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0
export INTERLOCK_COPILOT_BIN=<path to Copilot CLI 1.0.91>/node_modules/.bin/copilot
```

## Index

| # | What | Command | Outcome | Raw output |
| --- | --- | --- | --- | --- |
| 1 | Live GitHub access | `gh auth status` | Exit 1: "Failed to log in to github.com using token (GH_TOKEN) ... The token in GH_TOKEN is invalid." | [gh-auth-status.txt](gh-auth-status.txt) (redacted; no token was printed) |
| 2 | Whole workspace | `cargo fmt --all --check`; `cargo clippy --workspace --all-targets`; `cargo test --workspace` | fmt clean; clippy 0 warnings; tests: 189 passed, 0 failed, 1 ignored (the opt-in live test, row 6), across 28 test binaries including doc-tests | [cargo-test-workspace.txt](cargo-test-workspace.txt) (compile lines removed) |
| 3 | Fault tests as processes (invariant 7, P4 gates, review r6) | `INTERLOCK_EVIDENCE_OUT=$PWD/evidence/forge/fault-tests cargo test -p interlock-cli --test forge_cli` | 9 passed. Seven of the tests keep files, one folder per scenario. The other two (`a_moved_head_r2_needs_no_verifier_session` and `r3_forge_commands_are_the_operators_inside_an_attempt`) only assert | [fault-tests/](fault-tests/) |
| 4 | Walkthrough step 7 on Copilot CLI (offline, scripted model) | `INTERLOCK_EVIDENCE_OUT=$PWD/evidence/forge/copilot-e2e cargo test -p interlock-cli --test deliver_copilot` | 3 passed | [copilot-e2e/](copilot-e2e/), one folder per scenario |
| 5 | Each review fix undone, one at a time, with its covering tests run | `python3 evidence/forge/mutations/mutate.py` | 16 mutations. 15 make every covering test fail. One (m08b) does not, and that is expected; see below | [mutations/](mutations/) |
| 6 | Live run on Claude Code 2.1.289 with a real model, after the review | `INTERLOCK_LIVE=1 INTERLOCK_LIVE_OUT=$PWD/evidence/forge/live-claude-code cargo test -p interlock-cli --test deliver_live -- --ignored --nocapture` | 1 passed in 21.9 s: G1–G6 | [live-claude-code/](live-claude-code/) |
| 7 | The same live run before the review, kept for the record | (same command, on the code committed as `3f969c6`) | 1 passed in 27.3 s: G1–G6 | [live-claude-code-before-review/](live-claude-code-before-review/) |

The tests behind rows 3–6 also assert everything described below. The JSON files are what interlock printed or stored.

## 3. Fault tests as processes

Each folder holds the `interlock` command's JSON output, the task's transition log (`transitions.json`), its operation rows (`operations.json`), and every call the fake `gh` received (`gh-calls.json`). Every scenario grants landing authority to `coordinator`, so interlock merges itself, except `p4-no-landing-authority`, which grants none.

| Scenario | What happened |
| --- | --- |
| [invariant-7-crash-then-reconcile](fault-tests/invariant-7-crash-then-reconcile/) | The fake `gh` merged, then SIGKILLed `interlock integrate run` before it heard back. [1-integrate-run-killed.json](fault-tests/invariant-7-crash-then-reconcile/1-integrate-run-killed.json) shows signal 9 and no output. Main already held a merge of the verified head, and the merge row still said `started`. `interlock reconcile fix-add` asked the forge, found the merged head, checked with git that main contains the merge on the snapshot base with the verified tree, and applied G6 ([2-reconcile.json](fault-tests/invariant-7-crash-then-reconcile/2-reconcile.json)). There was one `gh pr merge` call in total |
| [invariant-7-crash-then-restarted-run](fault-tests/invariant-7-crash-then-restarted-run/) | The same crash, then a fresh `interlock run fix-add --host copilot`. Its startup reconcile settled the merge (G6) before the loop, so the run finished with no session. [2-interlock-run.json](fault-tests/invariant-7-crash-then-restarted-run/2-interlock-run.json) lists the operation under `reconciled` |
| [p4-moved-head](fault-tests/p4-moved-head/) | Someone pushed to `interlock/fix-add` while the merge request was in flight. The call carried `--match-head-commit <verified head>`, so GitHub (the fake) refused it with "Head branch was modified". interlock applied R2, and main never moved ([remote.json](fault-tests/p4-moved-head/remote.json)) |
| [p4-changed-base](fault-tests/p4-changed-base/) | Main moved after verification. interlock recorded the tree that would land on the new base, failed the landing operation, and applied R2. Every criterion's evidence is now stale (`status-after.json`: `all_pass: false`, `stale > 0`). No merge was attempted: there is no `gh pr merge` call |
| [p4-no-landing-authority](fault-tests/p4-no-landing-authority/) | No grant. G5 blocked the task at verified with "no landing authority is granted for this task", and its next move is back to verified. The fake `gh` received no call, and there are no operation rows |
| [integrate-run-delivers](fault-tests/integrate-run-delivers/) | The plain path: G5, push, PR #1, readiness, `gh pr merge 1 --merge --match-head-commit <head>`, G6. `integrate-run.json` lists each step |
| [r6-tree-changed-while-integrating](fault-tests/r6-tree-changed-while-integrating/) | Review finding 1. The task is integrating, its PR open and checks pending. `interlock task tree` then records a tree holding an unverified file, and checks turn green. `interlock run --max-sessions 0` applied R2 (integrating → awaiting verification) instead of merging. There is no `gh pr merge` call, and main is still the snapshot base. The landing planned at G5 is still `planned` in `operations.json`: it was never called. The next G5 fails it with "superseded before any call", which `regressions.rs` checks |

## 4. Walkthrough step 7 on Copilot CLI

A bug-fix task (`add()` returns the difference) with `integration_required = true` and a coordinator grant. The whole task runs through `interlock run fix-add --host copilot` on the real Copilot CLI 1.0.91. It runs offline against the scripted model in `crates/interlock-cli/tests/fake_model`, with the fake `gh` and a bare repository as `origin`.

| Scenario | Moves | What landed |
| --- | --- | --- |
| [copilot-walkthrough-step-7](copilot-e2e/copilot-walkthrough-step-7/) | G1 G2 G3 G4 G5 G6 (worker and verifier sessions, then delivery) | Main gained a merge whose second parent is the verified head `99b13180…`, the verified tree on the snapshot base. The merge was made with `--match-head-commit 99b13180…` ([gh-calls.json](copilot-e2e/copilot-walkthrough-step-7/gh-calls.json), [remote.json](copilot-e2e/copilot-walkthrough-step-7/remote.json)) |
| [copilot-no-landing-authority](copilot-e2e/copilot-no-landing-authority/) | G1 G2 G3 G4 block | Nothing. `stopped_because` is "blocked: no landing authority is granted for this task", and there was no `gh` call |
| [copilot-run-killed-mid-merge-then-restarted](copilot-e2e/copilot-run-killed-mid-merge-then-restarted/) | G1 G2 G3 G4 G5 (the fake `gh` killed the run mid-merge), then G6 from the second run | The second run reconciled the open merge operation. It made no session and no model call, and it never merged twice |

### What equal head ids here do and do not show

Both Copilot runs that delivered pinned the same head, `99b13180…`, and the three `fault-tests` scenarios that merged all pinned `b76ef6e3…`. That does not prove determinism by itself. The verified head is dated with the task's creation time, and tests running in parallel create their fixtures and tasks within the same second. So equal ids across fixtures show only that the inputs were equal.

An earlier version of this README claimed exactly that ("the commit is deterministic"), from an equal pair of ids. The claim is now backed by a dedicated test instead: `the_verified_head_is_the_same_commit_in_another_clone_a_second_later_and_never_signed` in `crates/interlock-forge/tests/deliver.rs`. It computes the head in a repository and again, 1.1 s later, in a clone of it. Both have `commit.gpgsign=true` and a signing program that always fails. The test checks that the ids match and that there is no `gpgsig` header. Mutations m08a and m08c show it catching a clock-dated head and a signed head.

## 5. Mutations: each review fix undone

`mutate.py` applies each mutation as an exact-text replacement in the source. It then runs the covering tests, records the result and restores the file from memory. Each `mNN-*.json` holds the replaced text, the replacement, each test's exit code and the tail of its output. [summary.json](mutations/summary.json) collects them. Every failure recorded is a test failure, not a compile error.

| Mutation | Fix undone | Covering tests | Result |
| --- | --- | --- | --- |
| [m01](mutations/m01-stale-evidence-merges.json) | Finding 1: the `all_pass` and tree checks in `start_operation`, advance at the top of `land()`, R2 on a re-planned head, and advance before every supervisor delivery step | `a_tree_recorded_while_integrating_is_r2_never_a_merge`, `r6_a_tree_recorded_while_integrating_is_r2_under_interlock_run` | both fail |
| [m02](mutations/m02-no-landing-checks.json) | Findings 2 and 3 (r1, lie): any merge at the pinned head lands, whatever git shows | `an_armed_auto_merge_that_lands_on_a_moved_base_blocks_instead_of_g6`, `a_reported_merge_git_cannot_see_on_the_base_branch_never_lands`, `an_operator_merge_onto_a_moved_base_is_not_g6` | all 3 fail |
| [m03](mutations/m03-forge-commands-inside-attempts.json) | Finding 3: forge commands allowed inside an attempt, and `integrate run` and `reconcile` no longer operator-only | `r3_forge_commands_are_the_operators_inside_an_attempt`, `classify::tests::forge_commands_belong_to_the_operator` | both fail |
| [m04](mutations/m04-retargeted-base.json) | Finding 4: readiness ignores the pull request's base branch | `a_pull_request_retargeted_to_another_branch_is_not_merged` | fails |
| [m05](mutations/m05-unanswered-merge-failed.json) | Finding 5: an unanswered merge call recorded as failed | `an_unanswered_merge_call_stays_open_until_reconcile_sees_the_merge` | fails |
| [m06](mutations/m06-push-lag-blocks.json) | Finding 6: a lagging `headRefOid` right after interlock's own push blocks | `a_lagging_head_after_interlocks_own_push_is_read_again_not_blocked` | fails |
| [m07a](mutations/m07a-no-disarm.json) | Finding 7: armed auto-merge left armed after the grant is revoked | `a_grant_revoked_while_auto_merge_is_armed_disarms_it` | fails |
| [m07b](mutations/m07b-merge-after-grant-ended.json) | Finding 7: a merge made after the grant ended gets G6 | `a_merge_made_after_the_grant_ended_is_not_g6` and the store's `a_merge_made_after_landing_authority_ended_is_recorded_but_not_g6` | both fail |
| [m08a](mutations/m08a-no-fixed-dates.json) | Finding 8: the head takes the clock's date | `the_verified_head_is_the_same_commit_in_another_clone_a_second_later_and_never_signed` | fails |
| [m08b](mutations/m08b-signing-allowed.json) | Finding 8: `--no-gpg-sign` removed | the same test | **passes**. git 2.43's `commit-tree` does not apply `commit.gpgsign` and signs only with `-S`, so dropping the flag changes nothing on this git. The flag stays as a guard |
| [m08c](mutations/m08c-asked-to-sign.json) | Finding 8: `commit-tree -S`, under the failing signer | the same test | fails |
| [m09](mutations/m09-base-not-read-before-merge.json) | Finding 9: the base not read with git right before the merge | `the_base_is_read_with_git_right_before_the_merge` | fails |
| [m10](mutations/m10-no-checks-settle.json) | Finding 10: no checks at all counts as ready at once | `no_checks_at_all_is_not_ready_until_ci_has_had_time` | fails |
| [m11](mutations/m11-verifier-after-moved-head.json) | Finding 11: the supervisor starts a verifier without advancing first | `a_moved_head_r2_needs_no_verifier_session` | fails |
| [m12](mutations/m12-unbounded-output.json) | Finding 12: unbounded wait on a child holding gh's output | `a_hung_gh_times_out_and_a_lingering_child_does_not_hold_interlock` | fails |
| [m13](mutations/m13-planned-landing-left-dangling.json) | Finding 1: a landing planned before an R2 is left `planned` after the next G5 | `a_tree_recorded_while_integrating_is_r2_never_a_merge` | fails |

The tests from finding 13 (branch-name injectivity, ref lifetime, final settled operations, abbreviated SHAs) and the operator-authority tests have no mutation entry. They run in row 2.

## 6. Live run on Claude Code, after the review

This is the design's own walkthrough, `examples/export-retry` ("Fix duplicate rows when an export retries"), with `integration_required = true` and a grant with `--landing coordinator`. It ran through `interlock run export-retry --host claude-code` with a real model, then delivered to the fake `gh` and a bare repository.

| Step | Record |
| --- | --- |
| Baseline | `repro` failed on the input snapshot (exit 1), and `regression` passed |
| Worker, 10.8 s | Fixed `export` to carry on from the failed row on a retry, and claimed both criteria ([run-report.json](live-claude-code/run-report.json)) |
| Verifier, 8.6 s | Had interlock run both checks on the same tree, and assessed both |
| Delivery | G5, push to `interlock/export-retry`, PR #1, `gh pr merge 1 --merge --match-head-commit 82ee5c1b…`, G6 ([gh-calls.json](live-claude-code/gh-calls.json), [operations.json](live-claude-code/operations.json)) |
| Landed | Main (`13226440…`) is a merge whose parents are the snapshot base `e63e7166…` and the verified head `82ee5c1b…` ([summary.json](live-claude-code/summary.json)). The diff from the base is [verified-head.diff](live-claude-code/verified-head.diff) |

The transitions were G1, G2, G3, G4, G5 and G6 ([transitions.json](live-claude-code/transitions.json)). The session transcripts stayed in the test's temporary directory and are not kept here.

## 7. The live run before the review

[live-claude-code-before-review/](live-claude-code-before-review/) is the first live run, made before the review on the code committed as `3f969c6`. It delivered G1–G6 and landed `72f74093…` onto `f267177f…`. Its grant used `--landing operator`, and at that commit operator authority let interlock merge. After the review it does not: with `operator`, interlock opens the pull request and waits for the operator to merge. The run is kept because it shows that earlier code working, but it does not exercise the fixed code. Row 6 does.
