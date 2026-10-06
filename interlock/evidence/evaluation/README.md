# Evaluation evidence

Runtime evidence for the §13 evaluation on the **stand-in** task set. How the harness works and what the results mean: [docs/evaluation.md](../../docs/evaluation.md).

Four stages, newest first:
- **v4, after the fixes (October 6, last):** the worker and verifier prompts and standalone skills (commit `882f3cb`), then `skills` and interlock rerun on the harder tasks and a second repeat of all three conditions on the original tasks: 34 counted runs.
- **v3, after integration (October 6):** all three conditions, `plain`, `skills` and `interlock` with skills, on Claude Code with the integrated interlock: 45 counted runs.
- **After the review (October 6, later):** the harness and task set were reworked to address the review, and checked with no model calls.
- **The first live run (October 6):** 28 Claude Code runs, `plain` and `interlock` only, on the first version of the harness and the seven original tasks.

Paths in commands: `H=interlock/eval/harness.py`. `STATE`, `RESULTS` and `RUNS` are the harness's three separate roots, outside the repository. Every file here is under 200 KB. Transcripts are gzipped, with the init event cut to its model, version, tools and working directory, long tool outputs truncated, and environment secrets redacted.

## v4: after fixing what v3 exposed

interlock-foundation at `a2ec7b9`, plus commit `882f3cb` on this branch: the worker and verifier prompts, standalone `implement` and `investigate` skills, and the harness changes. What changed and why: docs/evaluation.md, "Results v4".

**v4's harder-task runs are in-sample, not a held-out test.** The new prompt sentences were written after reading v3's failures and the tasks' hidden-requirement labels, one per category, and the host (2.1.289 to 2.1.291) and the skill text in every interlock session changed at the same time. docs/evaluation.md says what would test them.

| What | Command | Outcome | Raw output |
| --- | --- | --- | --- |
| interlock's own tests, hosts required | `CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 INTERLOCK_REQUIRE_HOSTS=1 INTERLOCK_COPILOT_BIN=<copilot 1.0.91> cargo test --workspace --no-fail-fast` | 320 passed, 0 failed, 1 ignored (the live delivery test). Includes the new prompt tests, the generator's standalone tests, `guided_cli` (15), `guided_copilot` (2) and `skills_load` (5, one new) | [checks/cargo_test_hosts_v4.out.txt](checks/cargo_test_hosts_v4.out.txt) |
| The harness's unit tests | `python3 interlock/eval/test_harness.py -v` | 41 of 41 pass. New: Copilot skill calls are counted; the scripted model invokes a listed skill first; Copilot-generated skills are wrapped as a plugin; `--first-repeat` continues the ABBA order; a repeat is skipped only when that repeat has counted | [checks/unit_tests.out.txt](checks/unit_tests.out.txt) |
| Task set selftest, new binary | `python3 $H selftest --state-dir STATE --interlock-bin interlock/target/debug/interlock --out selftest.json` | 11 of 11 tasks pass | [selftest.json](selftest.json) |
| Worker tools for plain and skills, against v3 | `python3 checks/tools_parity.py interlock/target/debug/interlock claude-code-v3/runs` | Identical on all 11 tasks | [checks/tools_parity_v4.out.json](checks/tools_parity_v4.out.json) |
| A standalone skill in a plain Copilot session, scripted model | `skills_load`'s `a_standalone_skill_reaches_copilots_model_in_a_plain_session_with_no_interlock` | Copilot lists `interlock-implement` with the new description; the skill tool loads it; its "Without interlock" steps reach the model | in the cargo log above |
| The skills condition on Copilot, scripted model (**plumbing, not evaluation evidence**) | `python3 $H run --host copilot --fake-model --conditions plain,skills --skills-generate --tasks py-split-remainder py-bisect-settle --repeats 1 --budget-usd 1 --session-timeout-min 3 --copilot-bin .../copilot --state-dir STATE --results R --run-base RUNS --interlock-bin .../interlock --label plumbing` | `skills` runs invoked `interlock-implement` and `interlock-investigate`; `plain` runs none; 4 of 4 accepted. Before the harness fix in this commit, Copilot-generated skills were wrapped one level too deep and never loaded | [copilot-skills-v4/report.md](copilot-skills-v4/report.md), `copilot-skills-v4/runs/` |
| Live probes: does Claude Code invoke a skill on a plain bug report? | `python3 checks/skills_invoked_probe.py interlock/target/debug/interlock` (12 turns, $0.08 cap), then the same with `3 0.05` | Neither invoked one; both listed the six skills and fixed the one-line bug directly. $0.0426 and $0.0410. Probe 1 had the first description ("Fix a bug, add or change a feature, or refactor code. ..."), probe 2 the second ("Use when asked to fix a bug, ..."); the committed one ("Use this skill whenever you are asked to ...") was tested only by the run | [checks/skills_invoked_probe_1.out.json](checks/skills_invoked_probe_1.out.json), [checks/skills_invoked_probe_2.out.json](checks/skills_invoked_probe_2.out.json) |
| Verifier writes into the tree under verification | `python3 checks/verifier_writes.py claude-code-v3` and `... claude-code-v4` (the reviewer's scan of every verifier's shell commands, then read by hand) | Three, all on `go-duration-days`: v3 repeat 2 wrote `zz_test.go` into its worktree; v4 repeat 1 copied a probe test in, and v4 repeat 2 wrote one. Each deleted its file; no judged output contains one. The other matches are writes under /tmp or text inside notes. The verifier prompt and agent now say to probe in a copy under /tmp | [checks/verifier_writes.out.txt](checks/verifier_writes.out.txt) |
| **The run, part 1** | `$H run --host claude-code --model claude-sonnet-5-5 --effort medium --conditions skills,interlock --skills-generate --interlock-skills --tasks go-dotted-section go-duration-days py-thousands py-date-filter --repeats 2 --budget-usd 6.91 --state-dir STATE --results R4 --interlock-bin .../interlock --label main` | 16 runs: skills 8/8, interlock 8/8 (v3: 8/8 and 5/8). No run sent back by the verifier; no skill invoked | [claude-code-v4/report.md](claude-code-v4/report.md) |
| **The run, part 2** | The same with `--conditions plain,skills,interlock --repeats 1 --first-repeat 2` and the seven original tasks | 18 runs, all accepted. The budget stopped it before `py-split-remainder` (its interlock run was estimated at $0.66) | [claude-code-v4/report.md](claude-code-v4/report.md), [claude-code-v4/report.json](claude-code-v4/report.json), [claude-code-v4/manifest.json](claude-code-v4/manifest.json), `claude-code-v4/runs/`, `claude-code-v4/judge/` |
| v4 next to v3 | `python3 checks/compare_v3_v4.py claude-code-v3 claude-code-v4` | Fisher two-sided: interlock on the harder tasks, v4 against v3, 8/8 against 5/8, p = 0.20; every other comparison p = 1.0. Paired within v4: interlock 2.43x the cost of skills, 2.44x the time | [checks/compare_v3_v4.out.json](checks/compare_v3_v4.out.json) |

**Leak-scan flag, reviewed by hand.** `go-dotted-section.skills.r2.444b86` is flagged for `/tmp/agent-ws/checks/dotted.conf`: the same false positive as v3's two, a relative path in a Go test the agent wrote.

### Pinned for v4

| | |
| --- | --- |
| Host | **Claude Code 2.1.291**: it updated itself after v3, which ran on 2.1.289. Every session also loaded this environment's three `cc-plugin-*` plugins |
| Model, effort, limits | As v3: `claude-sonnet-5-5`, `medium`, 80 turns and 15 minutes per session, at most 6 sessions per interlock run, `--max-budget-usd 3` per plain or skills run |
| interlock | Built from `882f3cb`; binary SHA-256 in the manifest |

### After the v4 review: no model calls

Commit `88df4e3`, on interlock-foundation `764518b`, which runs every check in a fresh checkout of the tree it is recorded against. Nothing here was run live, so the v4 numbers above come from the prompts as they were at `882f3cb`. docs/evaluation.md, "Results v4", shows how they have changed since.

| What | Command | Outcome | Raw output |
| --- | --- | --- | --- |
| The gate | `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets`; `CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 INTERLOCK_REQUIRE_HOSTS=1 INTERLOCK_COPILOT_BIN=<copilot 1.0.91> cargo test --workspace --no-fail-fast` | fmt clean; clippy 0 warnings; 324 passed, 0 failed, 2 ignored (the live Claude Code and live GitHub tests) | [checks/cargo_test_hosts_v4_review.out.txt](checks/cargo_test_hosts_v4_review.out.txt) |
| `interlock where` | `cli.rs::where_finds_the_store_every_command_uses_and_creates_none` | It finds the same store from the root, a subdirectory, an attempt's worktree, and an `INTERLOCK_DB` store. It reports a missing store with exit 3 and creates nothing | in the log above |
| The skill text uses it | `skills_load.rs::a_standalone_skill_reaches_copilots_model_in_a_plain_session_with_no_interlock`; the generator's standalone test | The text Copilot's model receives says "`interlock where` exits 0" and names `INTERLOCK_DB` | in the log above |
| A gap no worker can close | `run_fake_host.rs::a_gap_the_verifier_records_as_blocked_keeps_the_task_from_done_and_reaches_the_operator` | A verifier records `blocked` with a reason. A second verifier is asked, then the task is blocked, never done, and `status` shows the reason | in the log above |
| The prompts' structure | `prompts::tests` (4) | Each prompt's steps in order; the goal, gap and scratch texts; and the guided verifier agent carrying the same four texts verbatim | in the log above |
| The harness's unit tests | `python3 interlock/eval/test_harness.py -v` | 41 of 41 pass | [checks/unit_tests.out.txt](checks/unit_tests.out.txt) |
| Task set selftest | `python3 $H selftest ... --interlock-bin <absolute path>/interlock` | 11 of 11 tasks pass | [selftest.json](selftest.json) |

## v3: all three conditions on Claude Code, after integration

interlock-foundation at `858879e`, fast-forwarded into this branch; harness at `fe4597d`. No file under `interlock/eval/` or `interlock/crates/` changed between that commit and the end of the run; the manifest's `fe4597ddb023-dirty` is this documentation being edited while the second invocation ran.

| What | Command | Outcome | Raw output |
| --- | --- | --- | --- |
| interlock's own tests | `CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 INTERLOCK_REQUIRE_HOSTS=1 INTERLOCK_COPILOT_BIN=<copilot 1.0.91> cargo test --workspace` | 308 passed, 0 failed, 1 ignored (the live delivery test), with the host CLIs required so that no host test passes by skipping. The same suite without the hosts required also passed | [checks/cargo_test_hosts.out.txt](checks/cargo_test_hosts.out.txt) |
| The harness's unit tests | `python3 interlock/eval/test_harness.py -v` | 36 of 36 pass | [checks/unit_tests.out.txt](checks/unit_tests.out.txt) |
| Task set selftest against the integrated binary | `python3 $H selftest --state-dir STATE --interlock-bin interlock/target/debug/interlock --out selftest.json` | 11 of 11 tasks pass, with the lock updated for `py-bisect-settle`'s comment (its base commit is unchanged) | [selftest.json](selftest.json) |
| Effort pin, both mechanisms | `python3 checks/effort_probe.py low` (2 sessions) | `--effort low` and `CLAUDE_CODE_EFFORT_LEVEL=low` each gave a session whose active effort was `low`, under the harness's environment allowlist | [checks/effort_probe_low.out.json](checks/effort_probe_low.out.json) |
| Generated skills on Claude Code | `python3 checks/skills_probe_claude.py` on the output of `interlock skills generate --target claude-code` (1 session) | The init event lists the plugin `interlock`, its six skills (`interlock:design`, `implement`, `investigate`, `review`, `route`, `verify`) and the agent `interlock:verifier` | [checks/skills_probe_generated.out.json](checks/skills_probe_generated.out.json) |
| **The run, part 1** | `$H run --host claude-code --model claude-sonnet-5-5 --effort medium --conditions plain,skills,interlock --skills-generate --interlock-skills --tasks go-dotted-section go-duration-days py-thousands py-date-filter --repeats 2 --budget-usd 8.41 --state-dir STATE --results R --interlock-bin .../interlock --label main` | 24 runs. Accepted: plain 7/8, skills 8/8, interlock 5/8. All 6 forced interruptions landed and recovered | [claude-code-v3/report.md](claude-code-v3/report.md) |
| **The run, part 2** | The same, with the seven original tasks and `--repeats 1`, into the same results directory | 21 counted runs, all accepted. One `skills` interruption did not land (a test finished first); it was re-run and is not counted | [claude-code-v3/report.md](claude-code-v3/report.md), [claude-code-v3/report.json](claude-code-v3/report.json), [claude-code-v3/manifest.json](claude-code-v3/manifest.json), `claude-code-v3/runs/`, `claude-code-v3/judge/` |
| Paired cost and time ratios | `python3 checks/paired_ratios.py claude-code-v3` | interlock against plain: median 2.53x the cost (12 pairs), 2.33x the wall time (15 pairs). skills against plain: 1.11x and 1.10x. The verifier is about three quarters of interlock's extra cost | [checks/paired_ratios_v3.out.json](checks/paired_ratios_v3.out.json) |

Totals, 15 counted runs per condition:

| | plain | skills | interlock |
| --- | --- | --- | --- |
| Accepted | 14/15 | 15/15 | 12/15 |
| False completion claims | 1 | 0 | 3 |
| Claims without evidence | 0 | 0 | 0 |
| Missed defects | 1 | 0 | 4 |
| Scope violations | 0 | 0 | 0 |
| Recovery | 3/3 | 3/3 | 3/3 |
| Runs that invoked a skill | 0 | 0 | 0 |

Every failure was on `go-duration-days` or `go-dotted-section`. docs/evaluation.md describes each one, and what the data does and does not support.

**Leak-scan flags, reviewed by hand.** `go-dotted-section.skills.r1.708554` and `go-dotted-section.plain.r2.c6a447` are flagged as reaching `/tmp/agent-ws/checks/dotted.conf`, outside their root. In both, the flagged string is `../../checks/dotted.conf` inside a Go test the agent wrote to `cmd/kvconf/main_test.go`: relative to that package, it is the repository's own `checks/dotted.conf`. The scanner resolved it against the shell's working directory. No such path was opened. Both flags are false positives and stay in the records.

### Pinned for that run

| | |
| --- | --- |
| Host | Claude Code 2.1.289. Every session also loaded three plugins installed in this environment (`cc-plugin-agents-md`, `cc-plugin-plugin-authoring`, `cc-plugin-telemetry`), in every condition |
| Model | `claude-sonnet-5-5` |
| Effort | `medium`, through `--effort` for `plain` and `skills` and `interlock run --effort` for `interlock` |
| Skills | `interlock skills generate --target claude-code`; plugin SHA-256 in the manifest. `skills` gets it with no interlock runtime; `interlock` gets it through `interlock run --skills` |
| Limits | 80 turns per session, 15 minutes per session, at most 6 sessions per interlock run, `--max-budget-usd 3` per plain or skills run |
| interlock | Built from `858879e`; binary SHA-256 in the manifest |
| Environment | Python 3.11.15, Go 1.24.7, git 2.43.0, Linux, 4 CPUs shared with other work (wall times are noisy) |

## After the review: no model calls

| What | Command | Outcome | Raw output |
| --- | --- | --- | --- |
| The harness's unit tests | `python3 interlock/eval/test_harness.py -v` | 36 of 36 pass, including the review's four leak transcripts (and its control), which the first scanner missed; cd, glob, code-string and cross-call paths; the interruption watcher with inotify and with polling; landing rules; re-runs and give-up for interruptions that did not land; the environment allowlist; ABBA order | [checks/unit_tests.out.txt](checks/unit_tests.out.txt) |
| Frozen task set, built twice | `python3 $H build --state-dir STATE` (first without, then with `--update-lock`), then `build` into a second directory | Without `--update-lock` the changed task set fails the build (exit 1). With it, the lock records 11 base commits, 11 task-directory hashes and the judge library's hash. A rebuild elsewhere matches all of them | `interlock/eval/taskset/frozen.lock.json` |
| Task set selftest | `python3 $H selftest --state-dir STATE --interlock-bin interlock/target/debug/interlock --out selftest.json` | 11 of 11 tasks pass. All 29 near misses fail at least one hidden case; 20 of the 24 code near misses pass every visible check. None of 55 cross-task transplants passes another task's hidden checks, so no other repository and no pristine source holds a fix. interlock's own operator baseline runs agree with every visible check | [selftest.json](selftest.json) |
| Run refusals | `sh checks/refusals.sh ...` | `run` refuses, before any session: an interlock binary not named `interlock`; `--interlock-skills` and `--skills-generate` on a binary without them; the skills condition with no skills; overlapping run base, state and results | [checks/refusals.out.txt](checks/refusals.out.txt) |
| October 6 transcripts, rescanned | `python3 checks/rescan_oct6.py RESULTS WORK` | In all 28 runs, no tool call reaches the frozen repositories, start trees, results, task set, harness, another run or hidden material, and no suspicious search | [checks/rescan_oct6.out.json](checks/rescan_oct6.out.json) |
| Real-repository task isolation | `python3 checks/clone_test.py` | After building from an upstream repository at the commit before a fix, the fix's commit object, refs and log are gone | [checks/clone_test.out.txt](checks/clone_test.out.txt) |
| All three conditions on Copilot CLI with a scripted model (**not evaluation evidence**) | `python3 $H run --host copilot --fake-model --conditions plain,skills,interlock --repeats 2 --skills-dir <placeholder> --budget-usd 1 --session-timeout-min 3 --copilot-bin .../copilot --state-dir STATE --results R --interlock-bin .../interlock --label plumbing` | 66 runs on the real Copilot CLI 1.0.91, offline: all judged correctly, condition order reversed in repeat 2, every run's working root deleted afterwards. All 12 forced interruptions landed between the first edit and the first test command, and recovered. The investigation's plain and skills runs got exactly interlock's investigation-worker tools (`shell`, `url`; `write` and git commit and push denied). Runs saw only allowlisted environment names. The leak scan found nothing. The six "claims without evidence" are the investigation runs, where the scripted model ran no check | [copilot-plumbing/report.md](copilot-plumbing/report.md) and `copilot-plumbing/runs/` |

`checks/effort_probe.py` and `checks/skills_probe_claude.py` were prepared in this stage and run after integration (above).

## The first live run, October 6: Claude Code

First version of the harness (commit `3f930ee`; the pilot ran at `6df995f`) and the seven original tasks. The review then found its isolation, tool parity, interruption point and lock enforcement wanting; see docs/evaluation.md. The rescan above found no agent reaching the material it could have reached.

| What | Command | Outcome | Raw output |
| --- | --- | --- | --- |
| Effort pin (old mechanism) | `checks/effort_probe.py` as it was then (2 sessions) | The active effort was `medium` with no pin and with `CLAUDE_CODE_EFFORT_LEVEL=medium` | [checks/effort_probe.out.json](checks/effort_probe.out.json) |
| Cost pilot | `run --host claude-code --model claude-sonnet-5-5 --effort medium --conditions plain,interlock --tasks py-split-remainder --repeats 1 --budget-usd 14.88 --work-dir W --results R --label pilot` | plain $0.064, 11 s; interlock $0.382, 29 s; both accepted | `claude-code/runs/py-split-remainder.*.r1.*` |
| **Main run** | Same, with `--repeats 2` and no `--tasks`, into the same results directory | 28 runs, 14 per condition. Both conditions 14/14 accepted; no false claims, missed defects or scope violations; recovery 2/2 each. interlock: median 2.4x the cost and 2.1x the wall time of plain | [claude-code/report.md](claude-code/report.md), [claude-code/report.json](claude-code/report.json), [claude-code/manifest.json](claude-code/manifest.json), `claude-code/runs/`, `claude-code/judge/` |
| Skills plugin on Claude Code (old harness) | the old `checks/skills_probe_claude.py` (1 short session, plus an earlier attempt whose result was not parsed) | The init event listed the wrapped plugin and the placeholder skill | [checks/skills_probe_claude.out.json](checks/skills_probe_claude.out.json) |

That run's report and records use the old metric name "unsupported completion claims"; it means what the report now calls "false completion claims".

### Pinned for that run

| | |
| --- | --- |
| Host | Claude Code 2.1.289 |
| Model | `claude-sonnet-5-5` (Claude Code also used `claude-haiku-4-5-20251001` for small background calls in both conditions; its cost is included) |
| Effort | `medium`, through `CLAUDE_CODE_EFFORT_LEVEL` (the host's default for this model here) |
| Limits | 80 turns per session, 15 minutes per session, at most 6 sessions per interlock run, `--max-budget-usd 3` per plain run |
| interlock | Built from `d65d750`; binary SHA-256 in the manifest |
| Environment | Python 3.11.15, Go 1.24.7, git 2.43.0, Linux, 4 CPUs shared with other work (wall times are noisy) |

## Spend

| Item | USD |
| --- | --- |
| Reported by Claude Code in the 28 runs of October 6 (pilot included) | 3.94 |
| Reported in probes before the review (two connectivity probes, two effort probes, one skills probe) | 0.14 |
| Reported in the 46 runs after integration (45 counted, one re-run interruption) | 4.81 |
| Reported in the three probes after integration (two effort, one skills) | 0.08 |
| Reported in the 34 v4 runs | 4.20 |
| Reported in the two v4 skill probes | 0.08 |
| **Total reported** (the rows are rounded) | **13.27** |
| Not reported: sessions killed by forced interruptions (seven in v4, ten in v3, four in v1), and one skills probe session whose result was not parsed | unavailable |

For the v3 step, the cap was $8.50 of reported spend, with $0.30 reserved for each killed session: $4.89 reported plus $3.00 reserved for the ten killed sessions, **$7.89**. The budget never stopped a run. The v3 report's own line ("$7.81 charged against the $8.41 budget") covers the runs only: the three probe sessions ($0.08) ran first, outside the harness's budget, which was set to what was left of the $8.50.

For v4, the cap was $7.00 on the same terms: $4.29 reported plus $2.10 reserved for seven killed sessions, **$6.39**. The budget stopped the run before its last task. The v4 report's own line ("$6.30 charged against the $6.91 budget") likewise covers the runs only: the two skill probes ($0.084) ran first, outside the harness's budget, which was set to $7.00 less the probes.

The reports' row "Runs whose transcripts reach hidden material" counts leak-scan flags: in v3, two on go-dotted-section (one plain, one skills); in v4, one (skills, repeat 2). All three are false positives: the agent's Go test named `../../checks/dotted.conf`, which the scanner resolved against the shell's directory rather than the test's. docs/evaluation.md explains them; the scanner and the reports were left unchanged.

## Secrets scan

Every file here, transcripts decompressed, was searched for the values of all environment variables whose names mention a token, key, secret, email, UUID, session or auth; for API key, bearer token and GitHub token patterns; for unredacted interlock attempt tokens (`--token`, `INTERLOCK_TOKEN`, a `"token"` field); for email addresses other than `example.org`; for the launching sessions' ids; and for 64-hex strings. Rerun after the v4 review: 1,337 files, no hits. The only 64-hex strings are digests: SHA-256 references, the lock's task-directory and judge-library hashes, the binary and plugin hashes, and Copilot's `skillNameHash`.
