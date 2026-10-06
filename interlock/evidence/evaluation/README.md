# Evaluation evidence

Runtime evidence for the §13 evaluation on the **stand-in** task set. How the harness works and what the results mean: [docs/evaluation.md](../../docs/evaluation.md).

Two stages:
- **After the review (October 6, later):** the harness and task set were reworked to address the review, and checked with no model calls.
- **The first live run (October 6):** 28 Claude Code runs on the first version of the harness and the seven original tasks.

The post-integration live run of all three conditions has not happened yet.

Paths in commands: `H=interlock/eval/harness.py`. `STATE`, `RESULTS` and `RUNS` are the harness's three separate roots, outside the repository. Every file here is under 200 KB. Transcripts are gzipped, with the init event cut to its model, version, tools and working directory, long tool outputs truncated, and environment secrets redacted.

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

Prepared for the live rerun, not run (no model calls were allowed in this stage):

- `checks/effort_probe.py`: shows a non-default effort (`low`) reaches the session through both mechanisms the harness uses.
- `checks/skills_probe_claude.py`: rechecks the skills plugin on Claude Code with the new harness.

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

No model call has been made since the review; the scripted-model runs cost nothing.

| Item | USD |
| --- | --- |
| Reported by Claude Code in the 28 runs of October 6 (pilot included) | 3.94 |
| Reported in probes outside the harness (two connectivity probes, two effort probes, one skills probe) | 0.14 |
| **Total reported** | **4.09** |
| Not reported: four sessions killed by the forced interruption (22 to 31 s each), and one skills probe session whose result was not parsed | unavailable |

About $10.9 of the $15 cap remains. docs/evaluation.md estimates the post-integration run.

## Secrets scan

Every file here, transcripts decompressed, was searched for the values of all environment variables whose names mention a token, key, secret, email, UUID or auth; for API key, bearer token and GitHub token patterns; for email addresses other than `example.org`; for the launching session's id; and for 64-hex strings. 707 files, no hits; the only 64-hex strings are digests (policy digests, artifact references, lock and binary hashes).
