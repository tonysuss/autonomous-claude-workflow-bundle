# Integration evidence

The four workstreams (forge, runtime, skills, evaluation) were merged into `interlock-foundation`, and the merged build was tested as a whole. Each workstream's own evidence is in its directory; this records the checks on the merged build.

| What | Command | Outcome | Raw output |
| --- | --- | --- | --- |
| Full workspace suite on the final merge (`1f0f104`): the audit's fixes, the check runner's fresh checkouts, the design-gap fixes and the evaluation follow-up | `INTERLOCK_REQUIRE_HOSTS=1 INTERLOCK_COPILOT_BIN=<copilot 1.0.91> cargo test --workspace` | 350 passed, 0 failed, 2 ignored (the opt-in live Claude Code walkthrough and the live GitHub delivery). With hosts required, no test can pass by skipping. Earlier runs: 314 after the audit fixes (`d9b226d`), 308 after the first merge (`858879e`) | [cargo-test-workspace.txt](cargo-test-workspace.txt) |
| Evaluation harness tests | `python3 eval/test_harness.py` | 41 passed | |
| Lint and format | `cargo clippy --workspace --all-targets`; `cargo fmt --all --check` | 0 warnings; clean | |
| Guided gates, live Claude Code, on the merged build (`858879e`) | see [skills evidence](../skills/README.md), rows 23 and 24 | Bug fix and investigation both `done`; verifier bound as a subagent; every tool call through the hook; $0.52 | [../skills/integration/](../skills/integration/) |
| Kill and re-attach, live Claude Code, on the merged build | see [runtime evidence](../runtime/README.md), section 7 | Supervisor killed 6 s into the worker session; the restarted one re-attached; `done` with `G1 G2 G3 G4 G7`; $0.13 | [../runtime/live-claude-reattach-integrated/](../runtime/live-claude-reattach-integrated/) |
| Three-condition evaluation on the merged build | see [evaluation evidence](../evaluation/README.md) | plain 14/15, skills 15/15, interlock with skills 12/15 accepted; not significant; interlock about 2.5x the cost | [../evaluation/claude-code-v3/](../evaluation/claude-code-v3/) |
| Conformance audit against the design | an independent agent, new to the project, at `68a3e86` | 165 items: 114 met, 4 met-untested, 25 partial, 14 deviate, 4 not met, 4 not verifiable here; then fixed what was code | [../../docs/conformance.md](../../docs/conformance.md) |

Integration changes made while merging, beyond each branch's own work: one operating-system controller lock for `run`, `integrate run` and `reconcile`; the hook's `.interlock/` read rule strict for headless sessions only; `interlock run --skills`; and, after the audit, operator commands refused to callers descended from a launched session, G1's untracked hash, G2's tool check, a configurable host policy, the reported model, and upstream results in the brief.
