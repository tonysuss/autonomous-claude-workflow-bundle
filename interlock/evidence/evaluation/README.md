# Evaluation evidence

Runtime evidence for the §13 evaluation on the **stand-in** task set. How the harness works and what the results mean: [docs/evaluation.md](../../docs/evaluation.md).

Three stages, newest first:
- **After integration (October 6, last):** all three conditions, `plain`, `skills` and `interlock` with skills, on Claude Code with the integrated interlock: 45 counted runs.
- **After the review (October 6, later):** the harness and task set were reworked to address the review, and checked with no model calls.
- **The first live run (October 6):** 28 Claude Code runs, `plain` and `interlock` only, on the first version of the harness and the seven original tasks.

Paths in commands: `H=interlock/eval/harness.py`. `STATE`, `RESULTS` and `RUNS` are the harness's three separate roots, outside the repository. Every file here is under 200 KB. Transcripts are gzipped, with the init event cut to its model, version, tools and working directory, long tool outputs truncated, and environment secrets redacted.

## After integration: all three conditions on Claude Code

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
| **Total reported** (the rows are rounded) | **8.98** |
| Not reported: ten sessions killed by forced interruptions after integration, four before, and one skills probe session whose result was not parsed | unavailable |

For the post-integration step, the cap was $8.50 of reported spend, with $0.30 reserved for each killed session: $4.89 reported plus $3.00 reserved for the ten killed sessions, **$7.89**. The budget never stopped a run.

## Secrets scan

Every file here, transcripts decompressed, was searched for the values of all environment variables whose names mention a token, key, secret, email, UUID, session or auth; for API key, bearer token and GitHub token patterns; for unredacted interlock attempt tokens (`--token`, `INTERLOCK_TOKEN`, a `"token"` field); for email addresses other than `example.org`; for the launching sessions' ids; and for 64-hex strings. Rerun after the post-integration export: 1,043 files, no hits. The only 64-hex strings are digests: SHA-256 references, the lock's task-directory and judge-library hashes, and the binary and plugin hashes.
