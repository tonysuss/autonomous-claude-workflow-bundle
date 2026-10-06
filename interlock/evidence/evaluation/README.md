# Evaluation evidence

Runtime evidence for the §13 evaluation on the **stand-in** task set, October 6, 2026. How the harness works and what the results mean: [docs/evaluation.md](../../docs/evaluation.md).

Paths in commands: `H=interlock/eval/harness.py`, `W` is the harness work directory (frozen repositories and per-run working copies, kept outside the repository). Every file here is under 200 KB. Transcripts are gzipped, with the init event cut to its model, version, tools and working directory, long tool outputs truncated, and environment secrets redacted. A scan of every file found no account email, no environment token or key, and no attempt token (`checks/` has the scripts; the scan itself is described below).

## Index

| What | Command | Outcome | Raw output |
| --- | --- | --- | --- |
| Frozen task set build, twice in different directories | `python3 $H build --work-dir $W --update-lock`, then `build` elsewhere | Same seven base commits both times; pinned in `interlock/eval/taskset/frozen.lock.json` | The lock file |
| Task set selftest, no model calls | `python3 $H selftest --work-dir $W --interlock-bin interlock/target/debug/interlock --out selftest.json` | 7 of 7 tasks pass every check: visible checks keep their baselines (interlock's own operator baseline runs agree), hidden checks fail on every start state and pass on every reference solution, solutions stay in scope, the investigation's reproduction singles out the culprit and the judge rejects its parent | [selftest.json](selftest.json) |
| Hidden checks against near-miss fixes | `python3 checks/near_miss.py` | Three plausible partial fixes pass every visible check and fail 2 to 4 hidden cases each | [checks/near_miss.out.txt](checks/near_miss.out.txt) |
| Real-repository task isolation | `python3 checks/clone_test.py` | After building from an upstream repository at the commit before a fix, the fix's commit object, refs and log are gone | [checks/clone_test.out.txt](checks/clone_test.out.txt) |
| Effort pin | `python3 checks/effort_probe.py` (2 sessions) | The active effort is `medium` both with no pin and with `CLAUDE_CODE_EFFORT_LEVEL=medium` | [checks/effort_probe.out.json](checks/effort_probe.out.json) |
| Cost pilot, Claude Code | `python3 $H run --host claude-code --model claude-sonnet-5-5 --effort medium --conditions plain,interlock --tasks py-split-remainder --repeats 1 --budget-usd 14.88 --work-dir $W --results R --label pilot` | plain $0.064, 11 s; interlock $0.382, 29 s; both accepted. Projected about $6 for the full run, $12 with a 2x margin | `claude-code/runs/py-split-remainder.*.r1.*` |
| **Main evaluation, Claude Code** | Same, with `--repeats 2` and no `--tasks`, into the same results directory (the pilot counts as repeat 1 of its task) | 28 runs, 14 per condition. Both conditions 14/14 accepted; no unsupported claims, missed defects or scope violations; recovery 2/2 each. interlock: median 2.42x the cost and 2.06x the wall time of plain | [claude-code/report.md](claude-code/report.md), [claude-code/report.json](claude-code/report.json), [claude-code/manifest.json](claude-code/manifest.json), `claude-code/runs/`, `claude-code/judge/` |
| Copilot CLI plumbing, scripted model (**not evaluation evidence**) | `python3 $H run --host copilot --fake-model --conditions plain,interlock --repeats 1 --budget-usd 1 --session-timeout-min 3 --copilot-bin .../copilot --work-dir $W --results R2 --label plumbing`, then `--tasks go-env-expand --repeats 2` | 16 runs on the real Copilot CLI 1.0.91, offline. Every output judged correctly; both interruption paths killed and recovered; Copilot's `result` event carries no dollars or tokens, so cost is unavailable. An earlier attempt showed Copilot denying a script that read outside the working directory, and the harness scoring that run's false `STATUS: DONE` as an unsupported claim | [copilot-plumbing/plain-and-interlock/report.md](copilot-plumbing/plain-and-interlock/report.md) and `runs/` |
| Skills condition plumbing, Copilot (**not evaluation evidence**) | `python3 $H run --host copilot --fake-model --conditions skills --skills-dir <placeholder> --tasks py-parse-amount ...` | Copilot's `session.skills_loaded` event lists the placeholder skill with source `plugin` | [copilot-plumbing/skills/](copilot-plumbing/skills/) |
| Skills condition plumbing, Claude Code | `python3 checks/skills_probe_claude.py <placeholder skills dir>` (1 short session; an earlier attempt's session ran but its result was not parsed) | The init event lists the wrapped plugin and the skill `interlock-skills-eval:eval-dummy`. The plugin has since been renamed `workflow-skills`, so agents do not see the word "eval" | [checks/skills_probe_claude.out.json](checks/skills_probe_claude.out.json) |

The skills condition itself has **not been run**: the generated skills do not exist yet.

## Pinned for the Claude Code runs

| | |
| --- | --- |
| Host | Claude Code 2.1.289 |
| Model | `claude-sonnet-5-5` (Claude Code also used `claude-haiku-4-5-20251001` for small background calls in both conditions; its cost is included) |
| Effort | `medium`, through `CLAUDE_CODE_EFFORT_LEVEL` (the host's default for this model here) |
| Limits | 80 turns per session, 15 minutes per session, at most 6 sessions per interlock run, `--max-budget-usd 3` per plain run |
| interlock | Built from `d65d750` (crates unchanged since); binary SHA-256 in the manifest |
| Harness | `6df995f` for the pilot, `3f930ee` for the main run (no change in run behaviour between them) |
| Environment | Python 3.11.15, Go 1.24.7, git 2.43.0, Linux, 4 CPUs shared with three other agents (wall times are noisy) |

## Spend

| Item | USD |
| --- | --- |
| Reported by Claude Code in the evaluation's 28 runs (pilot included) | 3.94 |
| Reported in probes outside the harness (two connectivity probes, two effort probes, one skills probe) | 0.14 |
| **Total reported** | **4.09** |
| Not reported: four sessions killed by the forced interruption (22 to 31 s each), and one skills probe session whose result was not parsed | unavailable |

The harness charged the budget $6.94: the reported $3.94 plus a $0.75 reserve for each killed session. The cap was $15 ($14.88 given to the harness after the first probes). It never came close to stopping.

## Secrets scan

Every exported file, transcripts decompressed, was searched for the values of all environment variables whose names mention a token, key, secret, email, UUID or auth; for API key, bearer token and GitHub token patterns; for email addresses other than `example.org`; and for interlock's 64-hex attempt tokens. The only 64-hex strings present are digests: policy digests, artifact references and the binary's SHA-256.
