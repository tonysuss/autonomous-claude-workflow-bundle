# Evaluation

This is the harness for the design's §13 evaluation and the baseline its P0 spike S3 asks for. It runs three conditions on the same frozen tasks and judges every output with executable checks the agents never see.

The task set here is a **stand-in**. The design's S3 asks for 10 to 15 closed issues from a real repository, and that repository has not been chosen yet. The stand-in has the same shape, so the real set can replace it without changing the harness ([Swapping in the real S3 set](#swapping-in-the-real-s3-set)).

## Conditions

| Condition | What runs | Status |
| --- | --- | --- |
| `plain` | The host's ordinary headless workflow: `claude -p` (or `copilot -p`) with the task's prompt. No interlock. | Run on Claude Code |
| `skills` | The same, with a skills directory loaded (`--skills-dir`) and no runtime enforcement | Plumbing only: the generated skills do not exist yet |
| `interlock` | `interlock run <task> --host <host>`: worker and verifier sessions, hooks, baselines, guards | Run on Claude Code |

What is held equal across conditions:

- **The problem statement.** Each task's `prompt.md` is byte for byte the `intent` in its `task.toml`; `selftest` fails if they differ. A plain run's prompt adds one paragraph asking for a closing `STATUS: DONE` or `STATUS: NOT DONE: <reason>` line, which is how a plain run states completion.
- **Host, model, effort and limits.** Host version and model are pinned and recorded. Effort is pinned through `CLAUDE_CODE_EFFORT_LEVEL`, the variable Claude Code reads. (`CLAUDE_EFFORT` is only what it exports to Bash and hooks; setting it does nothing.) Both conditions get the same `--max-turns` and per-session timeout.
- **Tools and settings.** A plain Claude Code run gets what interlock gives a worker: `--permission-mode dontAsk`, `--setting-sources ""`, `--no-session-persistence`, and `Read Grep Glob Edit Write NotebookEdit Bash` allowed. It does not get interlock's hooks plugin; that is part of what is being measured.
- **Environment.** Variables that tie a child process to the session that started the harness (`CLAUDE_CODE_SESSION_ID`, the messaging socket and token, and similar) are dropped for every condition.
- **Starting point.** Every run starts from a fresh copy of the frozen repository, in a directory whose name says nothing about the condition.

What differs is the intervention itself: interlock gets the task's criteria and checks, runs baselines, starts an independent verifier, and decides when the task is done.

Run order interleaves tasks and shuffles the conditions within each task with a fixed seed, so neither condition always runs first.

## The stand-in task set

Two small projects written for this purpose: `tally` (Python, an expense splitter with a CLI) and `kvconf` (Go, an INI-style config reader with a CLI). Each task's repository is built deterministically, so its commit ids are the same on any machine; `taskset/frozen.lock.json` pins them and every run checks the lock.

| Task | Kind | Language | The issue | Visible check | What the hidden checks add |
| --- | --- | --- | --- | --- | --- |
| `py-split-remainder` | bug fix | Python | `split_evenly(100, 3)` loses a cent | `sh checks/split-repro.sh` | Negative totals mirror positive ones, the sum and spread property over 840 inputs, `ValueError` for `n <= 0`, ledger balances |
| `py-parse-amount` | bug fix | Python | `parse_amount("12.5")` reads 12.05 | `sh checks/parse-repro.sh` | The sign covers the fraction, more than two decimals is an error, existing behaviour kept, CSV rows |
| `go-quoted-hash` | bug fix | Go | `#` inside quotes is cut as a comment | `sh checks/quoted-hash.sh` (runs the CLI) | Comments after a closing quote, `\"` and `\\` escapes, unquoted values, the reported CLI call |
| `py-csv-format` | cross-module feature | Python | Add `--format csv` to `balances` and `settle` | `sh checks/csv-format.sh` (`balances` only) | `settle` CSV, quoting, header-only output, unchanged text output, exit status 2 |
| `go-lookup-refactor` | refactor | Go | Extract one `lookup` helper for four getters | `go test ./...` | Every error message exactly as before (one getter's message differs from the rest), the API, and the refactor's shape read from the syntax tree |
| `py-bisect-settle` | investigation | Python | Which commit made `settle` print a 0.00 transfer? | none; the answer is a commit id | The `ANSWER:` line names the culprit among ten commits, where decoys touch the same files |
| `go-env-expand` | forced interruption | Go | Add `${NAME}` expansion across parser, API and CLI | `sh checks/expand.sh` | Fallbacks, `$$`, the error message with its line number, `ParseOptions`, `--no-expand` on two commands |

Every requirement a hidden check tests is stated in the task's prompt. The visible check covers the reported case only, as an issue's reproduction would. That gap is where missed defects show up.

Each task directory holds:

| File | Who sees it |
| --- | --- |
| `prompt.md` | The plain condition, as its prompt |
| `task.toml` | interlock (`interlock task create`); kept outside the agent's working copy |
| `start/` or `history.py` | Built into the repository: the starting state and the visible checks |
| `meta.toml` | The harness only: kind, scope, judge, interruption settings |
| `hidden/` | The judge only: hidden tests, `judge.py`, and a reference solution used by `selftest` and the Copilot plumbing runs |

### The forced interruption

For `go-env-expand` the harness kills the whole process tree at the first change to the agent's working files (polled every quarter second), or after 300 seconds. It freezes every descendant with SIGSTOP before sending SIGKILL, so nothing gets to clean up: a crash, not a cancel. For `interlock`, the tree is the supervisor, its host sessions and their hooks.

Then each condition gets its own ordinary recovery, once:

- `plain`: the same prompt again, in the same working copy, prefixed with one sentence saying a previous session was interrupted and its changes are still there.
- `interlock`: `interlock run` again. The supervisor takes over the dead controller's lock, reconciles the attempt left running (R3), and starts a fresh worker.

Recovery succeeds when the recovered run's output is accepted.

## Metrics

Each run records the following. A value the host does not expose is recorded as unavailable, never estimated.

| Metric | Definition |
| --- | --- |
| Accepted outcome | The run claimed completion and every hidden check passes on its output |
| Completion claim | `plain`: the last `STATUS:` line in the final message says `DONE` (no line counts as no claim). `interlock`: the task ended in `done` |
| Unsupported completion claim | A completion claim whose output fails any hidden check |
| Missed defects | The number of failing hidden check cases in an output that was claimed complete |
| Scope violation | The output changes a file outside the task's scope. Judged by the harness on the output tree for both conditions. An empty scope means no file may change (unlike interlock, where an empty scope allows everything) |
| Correct but not claimed | Hidden checks pass, but the run did not claim completion: work held back that was in fact right |
| Recovery success | For the interruption task: the run was interrupted and the recovered output is accepted |
| Human interventions | Always 0 in these headless runs, and recorded as such. The harness's single automatic restart after an interruption is counted separately as `restarts` |
| Ended needing an operator | The run ended without a completion claim, so a person would have to pick it up |
| Wall time | From copying the repository to the end of the last session, including interlock's baselines and the restart |
| Cost, tokens | Summed from each session's final `result` event (Claude Code: `total_cost_usd` and `modelUsage`). A session killed before its result event has no reported cost; the run's cost is then marked incomplete |
| Hidden material seen | A transcript mentions the hidden checks' location: a sign the judge's checks could have leaked |

Outputs are judged anonymously. The harness exports each output tree with `git archive` into `judge/<random id>/`, along with the `ANSWER:` lines from the final message, and runs the task's `hidden/judge.py` there. The judge restores the task's original tests before running them, so an agent cannot pass by weakening a test. The judged directory carries no condition label, and the harness scans it for words that would give the condition away.

An agent's confidence in its own work is not a metric. A completion claim is recorded only to count the claims that turn out to be false.

## Running it

Python 3.11 or later, git, Go and the host CLI. No third-party Python packages.

```bash
cd interlock
cargo build -p interlock-cli
H=eval/harness.py
W=/tmp/eval-work          # frozen repositories and per-run working copies

python3 $H build --work-dir $W                     # build and check against frozen.lock.json
python3 $H selftest --work-dir $W --interlock-bin target/debug/interlock --out selftest.json

# Pilot: one run per condition on one task, to estimate cost.
python3 $H run --host claude-code --model claude-sonnet-5-5 --effort medium \
  --conditions plain,interlock --tasks py-split-remainder --repeats 1 \
  --budget-usd 15 --work-dir $W --results results --label pilot

# Everything, twice. Runs already in `results` count toward --repeats.
python3 $H run --host claude-code --model claude-sonnet-5-5 --effort medium \
  --conditions plain,interlock --repeats 2 --budget-usd 15 --work-dir $W --results results

python3 $H report --results results                # results/report.json and report.md
python3 $H export --results results --dest evidence/evaluation/claude-code   # sanitized, no output trees
```

`selftest` makes no model calls. For each task it checks that the prompt matches the intent, that no hidden material or the word "interlock" is in the repository, that every visible check keeps its baseline promise (and that interlock's own baseline runs agree), that the hidden checks fail on the starting state and pass on the reference solution, and that the reference solution stays in scope. For the investigation it checks that the reproduction passes at the culprit's parent and fails at the culprit, and that the judge rejects the parent.

### The budget

`--budget-usd` caps total spend across every invocation that writes to the same results directory. Before each run the harness estimates its cost as the most any comparable run has cost so far (same task and condition, else same condition, else a default), and stops if spent plus estimate would exceed the cap. Plain Claude Code runs also get `--max-budget-usd` (`--per-run-usd`, default $3). A session killed before it reports a cost is charged a $0.75 reserve against the budget. That reserve is bookkeeping, not a measurement.

### Copilot CLI

`--host copilot` runs `copilot -p ... --output-format json --no-ask-user --allow-tool=write --allow-tool=shell` for `plain`, and `interlock run --host copilot` for `interlock`. With `--fake-model`, Copilot runs offline against `eval/fake_model.py`, a scripted model that applies the reference solution. That exercises the real CLI, permissions, hooks, output parsing, the interruption path and the judge. **It is plumbing, not evaluation evidence**: the scripted model always does the same thing.

Copilot CLI 1.0.91 reports premium requests and durations in its `result` event, not dollars or tokens, so cost and tokens are unavailable for it.

### The skills condition

`--conditions skills --skills-dir DIR` loads a skills directory with no runtime enforcement. If `DIR` is a plugin (it has `.claude-plugin/plugin.json`) it is used as is; otherwise the harness wraps `DIR/skills/` (or `DIR`) as a plugin and passes it with `--plugin-dir`, and allows the `Skill` tool. Without `--skills-dir` the condition refuses to run. Generated skills do not exist yet, so this condition has not been run. Its plumbing was checked with a placeholder skill: Copilot listed it as loaded from the plugin, and so did Claude Code (see the evidence index).

`interlock run` loads its own role prompts and hooks; it has no way to pass generated skills to its sessions. Until it does, the `interlock` condition measures the runtime with interlock's built-in prompts.

## Swapping in the real S3 set

For each closed issue with a known fix:

1. Make `taskset/tasks/<id>/`.
2. `meta.toml`: `id`, `kind`, `language`, `repo` (a directory name for the working copy), `scope`, `judge = "hidden/judge.py"`, and

   ```toml
   [source]
   git = "https://github.com/OWNER/REPO.git"   # or a local path
   commit = "<the fix commit's parent>"
   ```

   The harness clones at that commit, deletes every ref and the remote, and prunes unreachable objects, so the fix cannot be found in the agent's copy.
3. `prompt.md`: the issue text as filed. `task.toml`: the same text as `intent` (use a `'''` literal string), the criteria, and the same scope. A good scope is the files the fix touched plus the tests.
4. `start/` (optional): visible check scripts to add on top of the upstream commit, such as a reproduction from the issue.
5. `hidden/`: the tests the fix added, and a `judge.py` that runs them on the output. `eval/judgelib.py` has helpers for unittest and `go test`; other languages need a small runner that prints `{"cases": [{"id", "pass", "detail"}]}`. Put the fix's changed files in `hidden/solution/` so `selftest` can prove the judge passes on the known fix.
6. `python3 eval/harness.py build --update-lock`, then `selftest`. Remove the stand-in tasks, or select the real ones with `--tasks`.

Then run the conditions as above, with `--host copilot` and a pinned `--model` once Copilot CLI can sign in.

## Results, October 6, 2026

Claude Code 2.1.289, `claude-sonnet-5-5`, effort `medium`, on the stand-in set. Every task under `plain` and `interlock`, twice each: 28 runs. The raw records, transcripts and anonymized verdicts are in [evidence/evaluation/](../evidence/evaluation/README.md); the full tables are in its [report](../evidence/evaluation/claude-code/report.md).

| Metric | plain | interlock |
| --- | --- | --- |
| Accepted outcomes | 14/14 | 14/14 |
| Unsupported completion claims | 0 | 0 |
| Missed defects | 0 | 0 |
| Runs with scope violations | 0 | 0 |
| Correct output not claimed | 0 | 0 |
| Recovery after a forced interruption | 2/2 | 2/2 |
| Human interventions | 0 | 0 |
| Sessions per run, mean | 1.14 | 2.14 |
| Wall time per run, mean (median) | 22 s (19 s) | 46 s (39 s) |
| Cost per run, mean over the 12 runs with complete cost | $0.076 | $0.198 |
| Cost reported, all 14 runs | $1.07 | $2.87 |

Paired by task and repeat, interlock cost a median 2.4 times as much as plain (range 2.0 to 6.0, 12 pairs) and took a median 2.1 times as long (range 1.3 to 2.8, 14 pairs). The two interruption runs per condition are missing from the cost figures: each lost a session to the kill, and a killed session never reports its cost.

What happened inside the runs:

- **interlock never reworked a task.** Every task went G1, G2, G3, G4, G7: one worker, one verifier, every criterion passed on the first try. The interruption runs added R3 and a second worker.
- **The interruption.** Both conditions were killed 22 to 31 seconds in, after the agent had already edited three or four files. The plain recovery picked up the partial work in the same working copy and finished in about 16 seconds. interlock's restart reconciled the dead attempt (R3) and started a fresh worker from the input snapshot, discarding the partial work by design. The plain interruption runs took 39 seconds in all, the interlock ones 81 and 108.
- **Tests.** interlock's workers changed or added test files in 4 of the 6 uninterrupted tasks in repeat 1; plain runs did so in 1 of 6. The hidden checks do not reward tests, so this is an observation, not a metric.
- **Leaks.** No transcript mentions the hidden checks' location, and no judged directory contains a word that names a condition.
- **Cost.** $3.94 reported across the 28 runs, plus $0.14 in probes outside the harness: $4.09 reported in all, against a $15 cap. Four killed sessions and one probe session have no reported cost.

## What the data supports

The design's first goal is falsifiable: better reliability, with an overhead we understand.

**Reliability: no difference can be seen, because neither condition failed.** Both accepted 14 of 14. With no failures in 14 runs, a per-run failure rate for plain as high as 19% is still consistent with the data at 95% confidence (35% if the 7 tasks, not the 14 runs, are the independent units). So this run neither supports nor contradicts "interlock is more reliable". It does show that on these tasks interlock did not make outcomes worse, refuse correct work, or let a scope violation through.

The stand-in set is too easy for this model at this effort. Each hidden check was shown to catch a plausible partial fix (`evidence/evaluation/checks/near_miss.out.txt`), and the prompts state every requirement those checks test, but the agents met every requirement anyway, in both conditions. A set that can falsify the goal needs tasks where the ordinary workflow sometimes fails: real closed issues (S3), longer tasks, or a weaker or faster model setting. Without such failures, interlock's verifier and guards have nothing to catch.

**Overhead: measured, and it is roughly 2 to 2.5 times.** On small tasks, interlock costs a median 2.4 times as much and takes 2.1 times as long. Over the 12 uninterrupted task pairs, a plain run cost $0.076 on average; interlock's worker cost $0.105 and its verifier $0.093. So about three quarters of the extra cost is the second, independent session, and the rest is the worker doing more (running checks through interlock, recording claims, and often adding tests). The ratio might shrink on larger tasks, if the verifier's work grows with the checks rather than with the change; every task here is small, so this data cannot say.

**Recovery: both recover; they recover differently.** Both conditions recovered from a hard kill, 2 of 2 each. interlock's restart is principled, with the dead attempt reconciled as a recorded failure and the next worker starting clean, but on this task it cost about twice the time of simply carrying on in the dirty working copy. The plain approach's risk, finishing on top of a half-applied change it does not remember making, did not materialize here.

**What this does not show:**

- Anything about Copilot CLI with a real model. The Copilot runs used a scripted model; they show only that the harness's plumbing works on the real CLI.
- Anything about the skills condition. It has not been run.
- Code quality or speed of delivery. The design keeps both as hypotheses until a comparison supports them, and this one cannot.
- Generality. Seven small tasks of our own making, one host, one model, one effort level, two repeats.

## Limitations

- **Stand-in tasks.** Written for this harness, small, and at ceiling for the model used. They are not the S3 set.
- **Small n.** Two repeats per task and condition.
- **The plain condition is told how to report.** It is asked for a closing `STATUS:` line, so that its completion claim can be read without a model's judgement. Every plain run gave one.
- **interlock's criteria spell the task out.** Each task file breaks the prompt into criteria, some with checks. That is the intervention being measured, but it means interlock also gets the task stated twice.
- **The hidden checks could be found.** They live in this repository, outside the agent's working copy but on the same disk. The harness scans transcripts for their location and found none.
- **Killed sessions have no cost.** The host reports cost only at the end of a session.
- **Wall times are noisy.** The machine's 4 CPUs were shared with three other agents' builds and runs.
- **interlock has no scope for "change nothing".** An empty scope allows every path, so the investigation task leaves scope to the harness's check; interlock relied on the investigation workflow denying edit tools. No investigation run changed a file.
