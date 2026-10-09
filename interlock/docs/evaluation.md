# Evaluation

This is the harness for the design's §13 evaluation and the baseline its P0 spike S3 asks for. It runs three conditions on the same frozen tasks and judges every output with executable checks the agents never see.

The task set here is a **stand-in**. The design's S3 asks for 10 to 15 closed issues from a real repository, and that repository has not been chosen yet. The stand-in has the same shape, so the real set can replace it without changing the harness ([Swapping in the real S3 set](#swapping-in-the-real-s3-set)).

## Conditions

| Condition | What runs | Status |
| --- | --- | --- |
| `plain` | The host's ordinary headless workflow: `claude -p` (or `copilot -p`) with the task's prompt. No interlock. | Run on Claude Code, October 6: v1, v3, and the original tasks in v4 |
| `skills` | The same, with interlock's generated skills loaded as a plugin and **no interlock runtime**: no `interlock` on `PATH`, no task store, no hooks, no supervisor, no verifier session. The skills are instructions the agent may invoke through the `Skill` tool; nothing enforces them. `--skills-generate` runs `interlock skills generate --target <host>`; `--skills-dir DIR` uses a directory that already exists | Run on Claude Code, October 6: v3, and with standalone skills in v4 |
| `interlock` | `interlock run <task> --host <host>`. With `--interlock-skills`, the same plugin is passed to `interlock run --skills`, making this "the skills plus the interlock runtime" | Run on Claude Code, October 6: without skills in v1, with them in v3, with the new prompts in v4 |

The generated plugin (`interlock skills generate --target claude-code`) holds six skills, `design`, `implement`, `investigate`, `review`, `route` and `verify`, and an agent, `verifier`. In the `skills` condition the agent has no tool to start a subagent, so the `verifier` agent is listed but cannot run. Since v4, `implement` and `investigate` work without interlock (docs/skills.md, "Without interlock"): with no task id, no `INTERLOCK_ATTEMPT` and no store, they follow their playbooks and record nothing. In v3 every step of every skill called `interlock`, which the skills condition does not have. The condition separates what the instructions do from what the runtime enforces.

What is held equal across conditions:

- **The problem statement.** Each task's `prompt.md` is byte for byte the `intent` in its `task.toml`; `selftest` fails if they differ. A plain or skills run's prompt adds two things: the task's scope in words ("Change only files that match these paths: ..." or "Do not change any files."), which interlock states in its own brief, and a request for a closing `STATUS: DONE` or `STATUS: NOT DONE: <reason>` line, which is how a plain run states completion.
- **Tools.** A plain or skills run gets exactly the host tools interlock grants that task's worker, read from `interlock host tools <task> --role worker --host <host>`, and recorded with every run. For the investigation that means read, shell and web tools, with edits and git commit and push denied; for the others, read, edit and shell tools, with git commit and push denied. The skills condition adds the `Skill` tool. Permission mode and settings isolation also mirror interlock's sessions: `--permission-mode dontAsk`, `--setting-sources ""`, `--no-session-persistence`. Plain runs do not get interlock's hooks plugin; that is part of what is measured.
- **Host, model, effort and limits.** Host version and model are pinned and recorded. Effort uses Claude Code's `--effort` flag for plain and skills, and `interlock run --effort` when the interlock binary has it; otherwise `CLAUDE_CODE_EFFORT_LEVEL`, the variable Claude Code reads. (`CLAUDE_EFFORT` is only what Claude Code exports to Bash and hooks; setting it does nothing.) Each run records which mechanism set its effort. Turn limits and per-session timeouts are the same.
- **Environment.** Every run's environment is built from an allowlist (system basics, proxy and CA settings, Go paths, and names added with `--pass-env`), never inherited wholesale, so the many `CLAUDE_CODE_*` variables that bind a child to the session that launched the harness, and any tokens, do not reach the agents. Only the `interlock` condition gets `INTERLOCK_CLAUDE_BIN` (or `INTERLOCK_COPILOT_BIN`). Each run records the names, not the values, of what it got. Before a live Claude Code run, the harness checks `claude auth status` with that environment.
- **Order.** Conditions run in ABBA order: a seeded order per task for odd repeats, reversed for even ones.

What differs is the intervention itself: interlock gets the task's criteria and checks, runs baselines, starts an independent verifier, and decides when the task is done. Its worker and verifier also get interlock's own instructions (`crates/interlock-supervisor/src/prompts.rs`), which changed between v3 and v4: see "Results v4".

## Isolation

The October 6 runs were not isolated well enough. The review found that each run's working copy sat next to the frozen repositories, the start trees, earlier runs' working copies and, a few directories up, the results, which hold every earlier run's judged output; that three tasks' pristine source files were other tasks' fixes; and that the leak scan only looked for the task set's absolute path. A rescan of those transcripts with the new scanner found no access to any of it (see Earlier results), but the harness now prevents what it can and detects the rest.

**Layout.** Three roots, which must not contain each other or the harness, checked before every run:

| Root | Holds | Option |
| --- | --- | --- |
| State | Frozen repositories, start trees, the run-roots log | `--state-dir` |
| Results | Run records, transcripts, judged trees and verdicts | `--results` |
| Run base | Nothing but the run in progress | `--run-base` |

Each run gets a fresh root in the run base, so the run base holds one directory while a run is in progress and none between runs. The working copy is created there from the locked commit, not copied from the frozen working tree: `git init`, a fetch of the frozen repository's `main`, a checkout of the locked commit, then the fetch record and reflogs are removed, so there is no remote and no trace of where the history came from. After the run is judged, the root is deleted; only the exported output tree, the transcripts and the record stay in the results.

**No task's source holds another task's fix.** The two sample projects carry every task's bug at once (both money bugs in `tally`, the quoted-`#` and dotted-name bugs in `kvconf`), each task's hidden checks avoid inputs another task's bug would affect, and each reference solution fixes only its own task. `selftest` proves it: for every task with a solution, it copies the same files from every other task's frozen repository, and from the pristine sample project, into the task's starting tree and runs its hidden checks; none may pass.

**Detection.** `leakscan.py` reads every tool call in a run's transcripts and resolves each path it names: relative paths against the session's working directory (from the session's own init event), `cd` within and across shell calls, `..`, `~`, the fixed prefix of a glob, and paths inside code strings such as `python3 -c "open('../x')"`. A call is flagged when it reaches:

- the state root, the results, the task set, the harness or the recorded evidence;
- any path shaped like a checkout of this harness (`*/interlock/eval/*`, `*/interlock/evidence/*`, `*/eval/taskset/*`) or a `hidden/` directory, so a second checkout of the repository is caught too;
- the run base outside the run's own root (another run);
- a string that names one of those places, another run's id or root, or, for a task built from an upstream repository, the upstream URL and the fix commit.

Listing or searching a directory that contains one of those places (`find / -name judge.py`) is reported separately as a suspicious search, since `ls /tmp` is also ordinary. The scanner is unit-tested, including on the four transcripts the review wrote to show the old scanner's blind spots.

**What cannot be prevented here.** Agents run as the same user as the harness, on the same machine, with network access. Anything that user can read, an agent can read: the task set in this repository, the state and results roots, other checkouts. The layout keeps those out of the agent's way and the scanner reports reaching them; neither stops a determined search. For the real S3 run, use a sandbox: run each session in a container (or a separate OS user) whose filesystem holds only the run's root, the host's own configuration and the toolchains, with network access limited to the model API. The harness's `--run-base` and allowlisted environment are designed to make that a mount and a user change, not a rewrite.

## The task set

Two small projects written for this purpose: `tally` (Python, an expense splitter with a CLI) and `kvconf` (Go, an INI-style config reader with a CLI). Each task's repository is built deterministically from the project, the task's overlay or history, and nothing else, so its commit ids are the same on any machine.

| Task | Kind | The issue | Visible check | What the hidden checks add |
| --- | --- | --- | --- | --- |
| `py-split-remainder` | bug fix | `split_evenly(100, 3)` loses a cent | `sh checks/split-repro.sh` | Negative totals mirror positive ones, the sum and spread property over 840 inputs, `ValueError` for `n <= 0` |
| `py-parse-amount` | bug fix | `parse_amount("12.5")` reads 12.05 | `sh checks/parse-repro.sh` | The sign covers the fraction, more than two decimals is an error, existing behaviour kept |
| `go-quoted-hash` | bug fix | `#` inside quotes is cut as a comment | `sh checks/quoted-hash.sh` | Comments after a closing quote, escapes, unquoted values, the reported CLI call |
| `go-dotted-section` | bug fix, **harder** | `kvconf get` fails for a section named `db.eu` | `sh checks/dotted.sh` | `Config.Lookup`, documented as taking the command line's names, has the same bug: a second call site the prompt does not name |
| `py-csv-format` | cross-module feature | `--format csv` for `balances` and `settle` | `sh checks/csv-format.sh` | `settle` CSV, quoting, header-only output, unchanged text, exit status 2 |
| `py-thousands` | feature, **harder** | Thousands separators in `balances` and `settle` | `sh checks/thousands.sh` | `export`, an untouched module, prints amounts through the same `format_cents` and must not change: the likely regression |
| `go-duration-days` | feature, **harder** | `GetDuration` should accept `30d` | `sh checks/days.sh` | `GetDuration` takes Go duration syntax, so the new unit should compose (`1d12h`) and take fractions (`1.5d`); bad values keep their message |
| `go-lookup-refactor` | refactor | Extract one `lookup` helper for four getters | `go test ./...` | Every error message exactly as before (one getter's differs), the API, and the refactor's shape read from the syntax tree |
| `py-bisect-settle` | investigation | Which commit made `settle` print a 0.00 transfer? | none; the answer is a commit id | The `ANSWER:` line names the culprit among ten commits, where decoys touch the same files |
| `go-env-expand` | forced interruption | Add `${NAME}` expansion across parser, API and CLI | `sh checks/expand.sh` | Fallbacks, `$$`, the error message with its line number, `ParseOptions`, `--no-expand` on two commands |
| `py-date-filter` | forced interruption | Add `--since` and `--until` across CLI, ledger and CSV reading | `sh checks/date-filter.sh` | Inclusive bounds, `settle`, invalid option dates, the file-and-line error for bad entry dates, no change without the options |

The October 6 run found the first seven tasks too easy for `claude-sonnet-5-5` at medium effort: neither condition failed once. The three **harder** tasks leave something for a careful engineer to find, as real issues do: a second call site, a regression in a module the task never mentions, an edge case implied by existing behaviour. Every hidden check still follows from the prompt or from documented existing behaviour, so the judgement stays defensible; the task's `meta.toml` says what was left out. Their `task.toml` criteria restate the prompt and add no hint the plain prompt lacks. In the post-integration run they did what they were meant to: four of the 45 runs failed, all on them (see Results).

Each task directory holds:

| File | Who sees it |
| --- | --- |
| `prompt.md` | The plain and skills conditions, as their prompt |
| `task.toml` | interlock (`interlock task create`); copied into the run's root, outside the working copy |
| `start/` or `history.py` | Built into the repository: the starting state and the visible checks |
| `meta.toml` | The harness only: kind, scope, judge, interruption settings, and what a harder task leaves out |
| `hidden/` | The judge only: hidden tests, `judge.py`, a reference solution, and `near_misses.toml` |

**Near misses.** Each task's `hidden/near_misses.toml` lists plausible partial fixes, such as fixing only the reported call site, grouping digits in the shared `format_cents`, special-casing a trailing `d`, or the parent commit as the investigation's answer. `selftest` applies each and requires it to fail at least one hidden case; most of them pass every visible check.

### The forced interruptions

`go-env-expand` and `py-date-filter` are killed mid-change: at a point between the agent's first edit and its first test run after that edit. The harness watches the working files with inotify (through `ctypes`; a 0.1-second poll where inotify is unavailable) and confirms each edit with `git status`, and it tails the session's transcript as it is written, where a tool call appears before it executes. After the first edit it kills at a delay drawn from [0, 20] seconds, deterministic per seed, task, condition and repeat, or as soon as a test command is issued, whichever is first. If a test command was already waiting when the edit landed, the kill is immediate.

The kill freezes every process in the run's tree with SIGSTOP, then sends SIGKILL: a crash, not a cancel. For `interlock`, the tree is the supervisor, its host sessions and their hooks.

Then each condition gets its own ordinary recovery, once:

- `plain` and `skills`: the same prompt again, in the same working copy, prefixed with one sentence saying a previous session was interrupted and its changes are still there.
- `interlock`: `interlock run` again. The supervisor takes over the dead controller's lock, reconciles the attempt left running (R3), and starts a fresh worker.

A run is *interrupted as designed* only if the kill came after an edit and before any test command finished after it. A run that never reached an edit, or whose kill came too late, is kept in the results, flagged with the reason, and re-run (up to `--interrupt-retries` times, default 2). Flagged runs are reported in their own row and never counted as passes or failures. Recovery succeeds when an as-designed run's recovered output is accepted.

## Metrics

Each run records the following. A value the host does not expose is recorded as unavailable, never estimated.

| Metric | Definition |
| --- | --- |
| Accepted outcome | The run claimed completion and every hidden check passes on its output |
| Completion claim | `plain` and `skills`: the last `STATUS:` line in the final message says `DONE` (no line counts as no claim). `interlock`: the task ended in `done` |
| False completion claim | Claimed done, and a hidden check fails |
| Claim without evidence | Claimed done, and no session in the run ran any test or check command to a result, read from the transcripts. A tool call counts as a check if it runs a test runner, a `checks/` script, `interlock check run`, or the project's CLI |
| Missed defects | The number of failing hidden check cases in an output that was claimed complete |
| Scope violation | The output changes a file outside the task's scope, judged on the output tree for every condition. An empty scope means no file may change, for the harness and, since integration, for interlock |
| Correct but not claimed | Hidden checks pass, but the run did not claim completion: work held back that was in fact right |
| Recovery success | For the interruption tasks: interrupted as designed, and the recovered output is accepted |
| Human interventions | Not applicable: these runs are headless and nobody can intervene. Reported as "n/a (headless)". The harness's single automatic restart after an interruption is recorded separately as `restarts` |
| Ended needing an operator | The run ended without a completion claim, so a person would have to pick it up |
| Wall time | From materializing the working copy to the end of the last session, including interlock's baselines and the restart |
| Cost, tokens | Summed from each session's final `result` event (Claude Code: `total_cost_usd` and `modelUsage`). A session killed before its result event has no reported cost; the run's cost is then marked incomplete |
| Hidden material seen, suspicious searches | The leak scanner's findings, as above |

Outputs are judged anonymously. The harness exports each output tree with `git archive` into `judge/<random id>/`, along with the `ANSWER:` lines from the final message, and runs the task's `hidden/judge.py` on a scratch copy. The judge restores the task's original tests before running them, so an agent cannot pass by weakening a test. The judged directory carries no condition label, and the harness scans it for words that would give the condition away.

The report gives every figure for all runs and, when a pilot was run, again without the pilot's runs. When no run failed, it gives the exact (Clopper-Pearson) 95% upper bound on the failure rate, one-sided and two-sided.

An agent's confidence in its own work is not a metric. A completion claim is recorded only to count the claims that turn out to be false.

## Running it

Python 3.11 or later, git, Go and the host CLI. No third-party Python packages.

```bash
cd interlock
cargo build -p interlock-cli
H=eval/harness.py
STATE=/tmp/eval-state; RESULTS=/tmp/eval-results; RUNS=/tmp/agent-ws   # three separate roots

python3 -m unittest discover -s eval -p 'test_*.py'          # the harness's own tests
python3 $H build --state-dir $STATE                            # fails if anything differs from the lock
python3 $H selftest --state-dir $STATE --interlock-bin target/debug/interlock --out selftest.json

# The post-integration run: the harder tasks and py-date-filter twice, in ABBA
# order, then the seven original tasks once, into the same results directory.
# --budget-usd caps the spend charged across both invocations.
RUN="python3 $H run --host claude-code --model claude-sonnet-5-5 --effort medium \
  --conditions plain,skills,interlock --skills-generate --interlock-skills \
  --budget-usd 8.41 --state-dir $STATE --results $RESULTS --run-base $RUNS --label main"
$RUN --repeats 2 --tasks go-dotted-section go-duration-days py-thousands py-date-filter
$RUN --repeats 1 --tasks go-lookup-refactor go-quoted-hash go-env-expand py-bisect-settle \
  py-csv-format py-parse-amount py-split-remainder

python3 $H report --results $RESULTS                         # report.json and report.md
python3 $H export --results $RESULTS --dest evidence/evaluation/claude-code-v3   # sanitized, no output trees
python3 evidence/evaluation/checks/paired_ratios.py evidence/evaluation/claude-code-v3

# v4, into a fresh results directory: the conditions the changes touch on the
# harder tasks, then a second repeat of everything on the original tasks.
RUN4="python3 $H run --host claude-code --model claude-sonnet-5-5 --effort medium --skills-generate \
  --interlock-skills --budget-usd 6.91 --state-dir $STATE --results $RESULTS4 --run-base $RUNS --label main"
$RUN4 --conditions skills,interlock --repeats 2 --tasks go-dotted-section go-duration-days py-thousands py-date-filter
$RUN4 --conditions plain,skills,interlock --repeats 1 --first-repeat 2 --tasks go-lookup-refactor go-quoted-hash \
  go-env-expand py-bisect-settle py-csv-format py-parse-amount py-split-remainder
python3 evidence/evaluation/checks/compare_v3_v4.py evidence/evaluation/claude-code-v3 evidence/evaluation/claude-code-v4
```

`--first-repeat N` numbers an invocation's repeats from N, so a later invocation continues the ABBA order; a (task, condition, repeat) is skipped only when a counted run of that repeat is already in the results directory.

A pilot (`--tasks py-split-remainder --repeats 1 --label pilot`) estimates cost before a larger run; its runs do not count toward the main run's repeats, and the report gives every figure with and without them.

`selftest` makes no model calls. For each task it checks:
- the frozen repository against the lock;
- that the prompt matches the intent;
- that no hidden material, and not the word "interlock", is in the repository;
- that every visible check keeps its baseline promise, and that interlock's own operator baseline runs agree;
- that the hidden checks fail on the starting state and pass on the reference solution, which stays in scope;
- that every near miss fails a hidden case;
- that no other task's repository and no pristine source holds the fix;
- for the investigation, that the reproduction passes at the culprit's parent and fails at the culprit.

### The lock

`taskset/frozen.lock.json` holds, for each task, the base commit of its frozen repository and a hash of its whole task directory (prompt, task file, metadata, start overlay or history, hidden checks, judge, solution and near misses), plus a hash of `judgelib.py`. `build` fails when anything differs from the lock, unless it is run with `--update-lock`. `selftest` and `run` refuse a frozen repository whose `HEAD` is not the locked commit or whose tree is not clean, and a task directory or judge library that no longer matches its hash.

### The budget

`--budget-usd` caps total spend across every invocation that writes to the same results directory. Before each run the harness estimates its cost as the most any comparable run has cost so far (same task and condition, else same condition, else a default), and stops if spent plus estimate would exceed the cap. Runs go repeat by repeat, so a budget stop leaves whole repeats behind it. Plain Claude Code runs also get `--max-budget-usd` (`--per-run-usd`, default $3). A session killed before it reports a cost is charged a reserve against the budget, $0.30 by default (`--unreported-reserve-usd`); on October 6 a whole plain session averaged $0.076. That reserve is bookkeeping, not a measurement.

### Copilot CLI

`--host copilot` runs `copilot -p ... --output-format json --no-ask-user`, with the worker's tools as `--allow-tool` and `--deny-tool`, for `plain` and `skills`, and `interlock run --host copilot` for `interlock`. With `--fake-model`, Copilot runs offline against `eval/fake_model.py`, a scripted model that applies the reference solution, runs the visible check, and (for interlock) records claims and assessments. When Copilot lists the generated skills (the `skills` condition), it first invokes the one an ordinary request of the task's kind would reach: `interlock-implement`, or `interlock-investigate` for the investigation. That exercises the real CLI, permissions, hooks, the skills plugin, output parsing, the interruption path and the judge. **It is plumbing, not evaluation evidence**: the scripted model always does the same thing.

Copilot CLI 1.0.91 reports premium requests and durations in its `result` event, not dollars or tokens, so cost and tokens are unavailable for it. It has no effort setting the harness knows of.

### Skills

- **`skills`**: pass `--skills-generate`, which runs `interlock skills generate --target <host> --out <state>/skills/<host>` (change the command with `--skills-generate-cmd`), or `--skills-dir DIR` for a directory that already exists. A directory that is already a plugin (`.claude-plugin/plugin.json`) is used as is; otherwise the harness wraps its skills as a plugin named `workflow-skills`. The run gets the worker's tools plus `Skill`, and nothing of interlock's runtime.
- **`interlock` with skills**: add `--interlock-skills`. The harness then passes the same plugin to `interlock run --skills`.
- Each run gets its own copy of the plugin inside its root, so an agent reading a skill file does not leave the run.
- The harness refuses each of these when the interlock binary lacks the matching subcommand or flag, so a run cannot silently measure the wrong thing.
- Each run records, descriptively, which skills it invoked and how many shell commands called `interlock`. These are not metrics: they show whether the skills were used at all.

## Swapping in the real S3 set

For each closed issue with a known fix:

1. Make `taskset/tasks/<id>/`.
2. `meta.toml`: `id`, `kind`, `language`, `repo` (a directory name for the working copy), `scope`, `judge = "hidden/judge.py"`, `fix_commit = "<the fix>"` (the leak scanner looks for it), and

   ```toml
   [source]
   git = "https://github.com/OWNER/REPO.git"   # or a local path
   commit = "<the fix commit's parent>"
   ```

   The harness clones at that commit, deletes every ref and the remote, and prunes unreachable objects, so the fix cannot be found in the agent's copy; the scanner also flags any mention of the upstream URL.
3. `prompt.md`: the issue text as filed. `task.toml`: the same text as `intent` (use a `'''` literal string), criteria that restate the issue without adding hints, and the same scope. A good scope is the files the fix touched plus the tests.
4. `start/` (optional): visible check scripts to add on top of the upstream commit, such as a reproduction from the issue.
5. `hidden/`: the tests the fix added, a `judge.py` that runs them on the output (`eval/judgelib.py` has helpers for unittest and `go test`; other languages need a runner that prints `{"cases": [{"id", "pass", "detail"}]}`), the fix's changed files in `hidden/solution/`, and near misses.
6. `python3 eval/harness.py build --update-lock`, then `selftest`. Remove the stand-in tasks, or select the real ones with `--tasks`.

Then run the conditions as above, in a sandbox (see Isolation), with `--host copilot` and a pinned `--model` once Copilot CLI can sign in.

## Results v4: after fixing what v3 exposed (October 6, 2026)

v3 exposed two problems (see "Results v3" below). interlock failed three of eight runs on the harder tasks, each one on a requirement the goal implies but the criteria do not spell out, and in all three the independent verifier passed the work. And no session invoked a skill, in any condition. Both were changed (commit `882f3cb`), and the conditions the changes touch were run again.

**This is not a held-out test, and three things confound it.**

1. **The wording is in-sample.** The prompts name no task, but they were written after reading v3's failures and the harder tasks' hidden-requirement labels (`harder` in each task's `meta.toml`), and each new sentence answers one of them:

   | New sentence | What it answers |
   | --- | --- |
   | "search for other call sites and code paths with the same defect" | `go-dotted-section`: a second call site |
   | "the inputs the goal implies, such as the forms the existing code already accepts" | `go-duration-days`: an edge case implied by existing code |
   | "A gap the worker's summary names is still a gap" | v3's `go-duration-days` repeat 1, where the verifier read the worker's stated limitation and passed it |
   | "Change nothing the goal does not need" | `py-thousands`: a regression in a module the task never mentions |

   So v4's runs on the harder tasks measure the prompts on the very tasks they were tuned against.
2. **The host changed.** Claude Code updated itself from 2.1.289 to 2.1.291 between v3 and v4.
3. **The skill text changed.** Every interlock session loads the skills plugin, so the new `implement` description ("fixes the root cause wherever it occurs") was in each worker's context, though no session invoked it. And in v3, `skills` and `plain` sessions found `Config.Lookup` with no prompt change at all (2 of 2 each).

The prompts are general and reasonable; they are not shown to work. A real test needs held-out tasks whose hidden requirements are of categories none of these sentences names, and an ablation: v3's prompts on 2.1.291 with the v4 plugin, to separate the prompts from the host and the skill text.

### What changed

**The worker and verifier prompts** (`prompts.rs`). The worker was sent to the smallest change that meets the criteria; it is now sent to the goal, the cause, and every place the cause reaches. The verifier judged the criteria only; it now judges the work against the goal too, and fails a gap with its reason. The guided verifier agent (`agents/verifier/AGENT.md`) got the same step, and two skill texts that said to build the least that meets the criteria (the feature playbook and subtract-before-you-add) now say "the whole goal".

| | Before (v3) | After (v4) |
| --- | --- | --- |
| Worker, step 1 | "You are in a fresh git worktree at the task's starting point. Make the smallest change that meets the criteria, inside the task's scope." | "You are in a fresh git worktree at the task's starting point. Work inside the task's scope. The goal is the target; the criteria are how its evidence is recorded, not a narrower target. Fix the cause, not the symptom, and fix it wherever it occurs inside the task's scope: search for other call sites and code paths with the same defect. Handle the inputs the goal implies, such as the forms the existing code already accepts. Change nothing the goal does not need. If you leave part of the goal undone, say which part and why." |
| Worker, step 5 | "Criteria that need an independent producer are checked by a separate verifier. Your claims inform it but do not satisfy them." | "... checked by a separate verifier, which judges the work against the goal as well as the criteria. ..." |
| Worker, step 6 | "Finish with two or three sentences: what you changed and how you checked it." | "... what you changed, where the cause was, and how you checked it." |
| Verifier, system prompt | "You decide, with evidence you observed yourself, whether each criterion holds for the exact files in your working directory." | "You decide, with evidence you observed yourself, whether the exact files in your working directory achieve the task's goal and whether each criterion holds." |
| Verifier, new step 3 | (none) | "Judge the work against the task's goal, not only the criteria's wording. A criterion that restates part of the goal holds only if the change achieves that part wherever it applies. Read the change, search the code around it for other call sites and code paths with the same defect, and try inputs the goal implies, such as the forms the existing code already accepts. A gap the worker's summary names is still a gap. A gap the task's scope does not let the worker close is not a failure: mention it in your note. Do not fail work for what the goal does not ask for, such as style or extra features." |
| Verifier, new step 4 | (none) | "When you find a gap, record `failed` on the criterion it bears on, or on the criterion closest to it if none names it, with the gap in the note: what is missing, where, and the input or command that shows it." |

That is what v4 ran with. The steps that run checks and record claims and assessments are unchanged.

**After the review of v4** (not yet run live), the verifier prompt changed again, and the guided verifier agent (`agents/verifier/AGENT.md`) now carries the same four texts verbatim, which a test checks:

| | v4 run (`882f3cb`) | Now |
| --- | --- | --- |
| Step 1, where it may write | "Do not modify, commit or push anything here. Put any scratch files under /tmp." | "Do not commit or push anything. Never create, change or delete a file in your working directory, not even for a moment: its files are what you are judging. To try inputs or run a probe of your own, copy the tree first (`cp -r . /tmp/<name>`) and write only in the copy, or put scratch files under /tmp." |
| Step 2, how a check runs | "interlock runs the check itself and records the output; read it." | "interlock runs the check itself, always in a clean checkout of the tree in your working directory, so files the tree leaves out (ignored or generated ones) cannot affect it, and records the output; read it." Since `764518b`, every check runs in a fresh checkout of the tree it is recorded against |
| Step 3, a gap outside the scope | "... is not a failure: mention it in your note." | Removed: a note is invisible to the evidence policy, so the task would reach done with a known gap |
| Step 4, recording a gap | "record `failed` on the criterion it bears on, or on the criterion closest to it if none names it" | "Record every gap; none is only a remark in a note. When a criterion's statement covers the missing part and the worker can close it inside the task's scope, record `failed` on that criterion. When no criterion covers it, or the task's scope does not let the worker close it, record `blocked` on the criterion closest to it: only a person can settle that. Either way, the note says what is missing, where, and the input or command that shows it; for `blocked`, it also says why the worker cannot close it." |

`failed` sends the work back for rework (R1). `blocked` keeps the criterion from passing; headless, a second verifier session is asked, and when it ends the same way interlock blocks the task for the operator, whose `interlock status` shows the verifier's reason (a new test in `run_fake_host.rs`).

Unit tests pin each prompt's steps in order, the new sentences, the rule against failing work for what the goal does not ask, and the scratch rule. They also check that neither prompt contains a few words from these tasks ("section", "duration", "dotted", "Lookup", "day", "Go", "Python", "CLI's"). That is a guard against copying task details, not proof that the wording is task-independent: see the in-sample note above.

**The skills** (`skills/`, the generator). Why none was invoked in v3: the transcripts show the six skills listed in every `skills` and interlock session's init event, and no `Skill` call in any of them. The descriptions the model saw said "Make the smallest change that meets an interlock task's criteria ..." and "Start here for any request ... in a repository that uses interlock". The task prompts never mention interlock (selftest checks the word is absent from every repository), so no description matched. Even an invoked skill could not have worked: every step called `interlock`, which the `skills` condition does not have. In v4:

- `implement` and `investigate` are standalone. Their descriptions trigger on ordinary requests ("Use this skill whenever you are asked to fix a bug, add or change a feature, or refactor code, however small the change looks: load it before your first edit"; "... whenever you are asked how something works, why it was built that way, or which change caused a behavior").
- Each body says when interlock applies: the agent was given a task id, `INTERLOCK_ATTEMPT` is set, or the repository has a store. (v4 ran with a test for `.interlock/state.db`; after the review the skills ask `interlock where` instead, which finds the store the way every command does, `INTERLOCK_DB` included, and creates nothing.) Otherwise the agent follows a `## Without interlock` section: the same playbooks, checks run by the agent itself, and nothing recorded.
- The generator checks the promise when it loads the catalog: `standalone = true` in `skill.toml` requires that section.
- `route` says it is for repositories set up for interlock. `verify`, `review` and `design` are unchanged.
- The guided tests (`guided_cli`, `guided_copilot`, `skills_load`) pass with `INTERLOCK_REQUIRE_HOSTS=1`, as does the whole workspace: 320 passed, 1 ignored (`evidence/evaluation/checks/cargo_test_hosts_v4.out.txt`).

**Is a skill now invoked in a plain session?**

| Check | Result |
| --- | --- |
| Copilot CLI 1.0.91, scripted model, a test in `skills_load.rs` | In a plain session with no store and no attempt, Copilot lists `interlock-implement` with the new description, the skill tool loads it, and its "Without interlock" steps reach the model |
| The harness on Copilot, scripted model (`copilot-skills-v4/`) | The `skills` runs invoked `interlock-implement` (a bug fix) and `interlock-investigate` (the investigation), and the `plain` runs invoked none, all four accepted. That required a harness fix: `--skills-generate` on Copilot had wrapped the generated `.github/skills` one level too deep, so no Copilot session had loaded the skills before |
| Two live Claude Code probes on a one-line bug, $0.084 together (`checks/skills_invoked_probe_{1,2}.out.json`) | Not invoked. The model listed all six skills and fixed the bug directly (Grep, Edit, Bash). The probes used the first two wordings of the description; the final, more directive one was tested only by the v4 run |
| The v4 run: 28 `skills` and interlock runs on real tasks, 48 sessions | Not invoked, once |

The scripted checks show that the plumbing and the text work. They do not show that a model chooses to use them, because the script chooses for it. The live runs show that `claude-sonnet-5-5` at medium effort, in `claude -p`, did not choose to use them, even with a description that says to load the skill before the first edit. So **the v4 `skills` condition still measured the skills' descriptions in context, not skills in use**, and the fix is unverified where it matters. What would test it: a host or model that follows skill descriptions more readily, or a prompt that names the skill. The second changes the problem statement, which the harness holds equal across conditions.

### What was run

Claude Code updated itself between v3 and v4, from 2.1.289 to **2.1.291**. Every v4 run used 2.1.291, `claude-sonnet-5-5` at medium effort, the same harness settings as v3, and ABBA order. Records, transcripts and verdicts are in `evidence/evaluation/claude-code-v4/`.

1. `skills` and `interlock` on the three harder tasks and `py-date-filter`, two repeats each: 16 runs. `plain` was not rerun. Nothing interlock changed reaches it: it loads no plugin, its prompt and harness path are the same, and its tools came out identical on all 11 tasks (`interlock host tools` on the new binary against v3's records). The host did change under it, though, so these runs are compared with v3's `plain` runs on 2.1.289, across a host version.
2. Then a second repeat of all three conditions on the seven original tasks, run as repeat 2 (`--first-repeat 2`), so each task's conditions ran in the reverse of v3's order. The budget stopped this before its last task: the next run, `py-split-remainder` under interlock, was estimated at $0.66 (the costliest interlock run so far, an interruption with its reserve), which would have passed the cap. So six of the seven original tasks now have two runs per condition; `py-split-remainder` still has one.

| Accepted | plain | skills | interlock (with skills) |
| --- | --- | --- | --- |
| Harder tasks, v3 (two repeats) | 7/8 | 8/8 | 5/8 |
| **Harder tasks, v4** (two repeats) | not rerun | **8/8** | **8/8** |
| Original tasks, v3 (repeat 1) | 7/7 | 7/7 | 7/7 |
| **Original tasks, v4** (repeat 2, six tasks) | **6/6** | **6/6** | **6/6** |

| v4, all 34 counted runs | plain (6) | skills (14) | interlock (14) |
| --- | --- | --- | --- |
| False completion claims | 0 | 0 | 0 |
| Claims without evidence | 0 | 0 | 0 |
| Missed defects | 0 | 0 | 0 |
| Runs with scope violations | 0 | 0 | 0 |
| Recovery after a forced interruption | 1/1 | 3/3 | 3/3 |
| Runs that invoked a skill | 0 | 0 | 0 |
| Sessions per run, mean | 1.17 | 1.21 | 2.21 |
| Wall time per run, mean | 19 s | 21 s | 50 s |
| Cost per run, mean over runs with complete cost | $0.064 (5) | $0.075 (11) | $0.177 (11) |

Paired by task and repeat within v4 (`checks/compare_v3_v4.out.json`): interlock cost a median 2.43 times as much as skills (11 pairs, range 1.77 to 3.08) and took 2.44 times as long (14 pairs). Against plain, on the original tasks only, the medians are 2.79 times the cost (5 pairs) and 2.81 times the time (6 pairs). skills cost 1.08 times plain. Over the 11 uninterrupted interlock runs, the worker sessions cost $1.11 and the verifier sessions $0.84.

**What happened in interlock's 8 of 8.** No v4 run was sent back for rework (no R1): every uninterrupted run passed verification on its first attempt. On `go-dotted-section`, both workers fixed `Config.Lookup` as well as the command line. On `go-duration-days`, both workers accepted `1d12h` and `1.5d`. The verifiers did follow the new instructions, which shows in their transcripts:

- On `go-dotted-section`, they searched for every other place that splits a name ("I found no other splitting sites").
- On `go-duration-days`, they ran their own probes of `1d12h`, `1.5d`, `-2d` and malformed values.

They found nothing to fail. This run cannot say why the workers did better than v3's: the prompt, the host and the skill text changed together (see the in-sample note above). It does not exercise the verifier's new failure path.

**Verifiers wrote into the tree they were judging, three times.** On `go-duration-days` in v4, the repeat 1 verifier copied a probe test into its worktree and the repeat 2 verifier wrote one there; in v3, the repeat 2 verifier did the same. Each ran its probe and deleted the file, and no judged output contains one, but for that moment the files under verification were not the submitted ones, and no hook stopped it. The v4 step "try inputs the goal implies" invites exactly this. The verifier prompt and agent now say to copy the tree to /tmp and probe there, and never to write in their own working directory (above).

**Leak scan.** One run was flagged, `go-dotted-section` under `skills`, repeat 2. It is the same false positive as v3's two: a Go test the agent wrote opens `../../checks/dotted.conf`, relative to its own package, and the scanner resolved that against the shell's working directory.

**Spend for v4.** Claude Code reported $4.204 for the 34 runs and $0.084 for the two probes: **$4.29 reported**. Seven sessions were killed by forced interruptions and reported no cost. At $0.30 each, $2.10 more was charged against the budget, for **$6.39 charged against the $7.00 cap**.

## What the data supports

The design's first goal is falsifiable: better reliability, with an overhead we understand.

**Did the v4 changes help? On these tasks interlock went from 5 of 8 to 8 of 8 on the harder tasks, and that is not significant** (Fisher's exact test, two-sided, p = 0.20). The direction is the one the change aimed at, and in the transcripts the workers fixed the second call site and handled the implied inputs. But this is in-sample (the wording answers these tasks' hidden requirements), the host and the skill text changed at the same time, and 8 runs per arm cannot tell a fix from luck. A failure rate near v3's 3 in 8 would give 8 of 8 about 2% of the time; one near the other conditions' 1 in 16 would give it about 60% of the time. Nothing else changed significantly either:

| Comparison, accepted | | p (two-sided) |
| --- | --- | --- |
| interlock, harder tasks: v4 against v3 | 8/8 against 5/8 | 0.20 |
| interlock v4 against plain v3, harder tasks | 8/8 against 7/8 | 1.0 |
| interlock v4 against skills v4, harder tasks | 8/8 against 8/8 | 1.0 |
| skills v4 against skills v3, harder tasks | 8/8 against 8/8 | 1.0 |
| interlock against plain, original tasks, v4 | 6/6 against 6/6 | 1.0 |

**Reliability: no condition is shown to be better than another.** Across v3 and v4 the harder tasks produced four failures, three of them interlock's in v3, and none since the change. With samples this small, and runs that share tasks, the honest statement is that interlock with these prompts did not do worse here, and that this data cannot show it does better. What v3 falsified still stands for v3's prompts: a verifier that checks only the criteria passes work that misses what the goal implies. Whether v4's verifier would catch such a gap is untested, because no v4 worker left one.

**Overhead: about 2.4 to 2.8 times, and the verifier is most of it.** In v4, interlock cost a median 2.43 times as much as skills and took 2.44 times as long; against plain, on six original tasks, 2.79 and 2.81 times. Its mean cost per run rose a little, from $0.166 in v3 to $0.177. The verifier sessions are 72% of the extra cost over plain (5 pairs) and 75% of the extra over skills (11 pairs): about three quarters, as in v3.

**Recovery: every forced interruption recovered**, in both runs and every condition: 3 of 3 per condition in v3, and 7 of 7 in v4.

**Claims without evidence, scope: no difference.** None in any condition in either run.

**Skills: still not in use.** The skills now trigger, on paper, on ordinary requests and work without interlock. The scripted checks show the plumbing and the text reach the model. But no live session chose to invoke one: 0 of the 28 v4 runs that loaded them, and 0 of 2 probes. So neither run measured skills in use. The `skills` condition is plain Claude Code with the skills' descriptions in context, which cost about 8 to 11% more than plain and did no worse.

**What this does not show:**

- That the v4 prompts work. Their test is in-sample, and confounded with the host update and the new skill text.
- Anything about skills in use (above).
- Whether v4's verifier catches gaps. No v4 run had one.
- Anything about Copilot CLI with a real model. Its runs used a scripted model.
- Code quality or speed of delivery.
- Generality. These are eleven small tasks of our own making, one host (whose version moved between runs), one model, one effort level, and one or two repeats.

**What would test the prompts:**

1. Held-out tasks whose hidden requirements fall in categories the new sentences do not name, run under every condition.
2. An ablation on the current host: v3's prompts with the v4 plugin on 2.1.291, against v4's prompts. That separates the prompts from the host update and the skill text.
3. A task where the worker reliably leaves a gap, to exercise the verifier's `failed` and `blocked` paths live.

## Results v3: all three conditions, after integration (October 6, 2026)

interlock at `858879e` (the integrated `interlock-foundation`), harness at `fe4597d`. Claude Code 2.1.289, `claude-sonnet-5-5`, effort `medium`, through Claude Code's `--effort` for `plain` and `skills` and `interlock run --effort` for `interlock`. `skills` loaded the plugin that `interlock skills generate --target claude-code` wrote; `interlock` ran with `--interlock-skills`, so its worker and verifier sessions loaded the same plugin. Conditions ran in ABBA order.

The plan was the reduced one: the seven original tasks once, and the three harder tasks and `py-date-filter` twice. That is 15 counted runs per condition, 45 in all. One more run, a `skills` run of `go-env-expand` whose kill came after a test had finished, was flagged, re-run and not counted. Records, transcripts and anonymized verdicts are in [evidence/evaluation/claude-code-v3/](../evidence/evaluation/README.md).

| Metric | plain | skills | interlock (with skills) |
| --- | --- | --- | --- |
| **Accepted outcomes** | **14/15** | **15/15** | **12/15** |
| On the three harder tasks and `py-date-filter` (two runs each) | 7/8 | 8/8 | 5/8 |
| On the seven original tasks (one run each) | 7/7 | 7/7 | 7/7 |
| False completion claims | 1 | 0 | 3 |
| Of those, the final message named the case the output does not handle | 1 | - | 2 |
| Claims without evidence | 0 | 0 | 0 |
| Missed defects (failed hidden cases in claimed outputs) | 1 | 0 | 4 (3 root causes) |
| Runs with scope violations | 0 | 0 | 0 |
| Recovery after a forced interruption | 3/3 | 3/3 | 3/3 |
| Human interventions | n/a (headless) | n/a (headless) | n/a (headless) |
| Sessions per run, mean | 1.2 | 1.2 | 2.2 |
| Wall time per run, mean (median) | 22 s (16 s) | 21 s (20 s) | 42 s (35 s) |
| Cost per run, mean over the 12 runs with complete cost | $0.066 | $0.072 | $0.166 |
| Runs that invoked a skill | 0/15 | 0/15 | 0/15 |

Paired by task and repeat (`checks/paired_ratios_v3.out.json`). Cost pairs leave out the interrupted runs, whose killed sessions report no cost.

| Pair | Cost ratio, median (range), 12 pairs | Wall-time ratio, median (range), 15 pairs |
| --- | --- | --- |
| skills / plain | 1.11 (0.81 to 1.25) | 1.10 (0.51 to 1.76) |
| interlock / plain | 2.53 (2.01 to 3.38) | 2.33 (1.10 to 3.14) |
| interlock / skills | 2.33 (1.96 to 2.95) | 1.92 (1.33 to 3.12) |

Over the 12 uninterrupted interlock runs, the worker sessions cost $1.09 and the verifier sessions $0.91. Plain runs of the same 12 tasks and repeats averaged $0.066, so of interlock's extra $0.100 per run, the verifier was $0.076: about three quarters, as in the first run.

**Every failure.** All four are false completion claims; no run failed without claiming done.

| Run | Hidden cases failed | What the output does | Said so in the final message? |
| --- | --- | --- | --- |
| `go-duration-days`, plain, repeat 1 | fractional days | Takes `1d12h`; rejects `1.5d` on purpose | Yes: "Fractional days (`1.5d`) ... are rejected" |
| `go-duration-days`, interlock, repeat 1 | days with other units | Takes `1.5d`; rejects `1d12h` on purpose, and adds a test that pins the rejection | Yes: "`1d12h` is still an error" |
| `go-duration-days`, interlock, repeat 2 | fractional days | Takes `1d12h`; rejects `1.5d` on purpose | Yes: "Day counts must be whole numbers, so `1.5d` is rejected" |
| `go-dotted-section`, interlock, repeat 2 | `Config.Lookup` with a dotted section, and its error message (one root cause) | Fixes the command line's `splitName` only. `Config.Lookup`, the second call site the prompt does not name, keeps the bug | No |

In all three interlock failures, the independent verifier ran the checks, examined the work and recorded every criterion as holding. On `go-duration-days` repeat 1 it noted that `1d12h` is rejected and passed `spec` anyway. That is consistent with the criterion as written ("accepts the unit d ... and everything it accepted before still works"), since Go's own syntax never accepted `1d12h`. On `go-dotted-section` repeat 2 it tested `splitName` and the command line and did not look at `Config.Lookup`. Neither session is told what the hidden checks add.

**Whether the hidden checks are fair.** The `go-dotted-section` case is clear: `Config.Lookup` is documented as taking the command line's names, and the prompt states the rule for every such name. The `go-duration-days` cases are a judgement the task embeds. `GetDuration` takes Go duration syntax, so the task treats a new unit as one that composes and takes fractions like the others. An engineer could reasonably decide otherwise and say so, as three of the runs did. Counting those three as false claims follows the metric's definition. A reader who disagrees with the task's judgement should count them as disclosed limitations instead, which leaves interlock with one undisclosed failure in 15 and the other two conditions with none.

**Leak scan.** Two runs (one `plain`, one `skills`, both `go-dotted-section`) were flagged as reaching outside their root. Both are false positives. In each, the agent wrote a Go test in `cmd/kvconf/` that opens `../../checks/dotted.conf`, which from that package's directory is the repository's own `checks/dotted.conf`; the scanner resolved the string against the shell's working directory instead. No tool call in any run reached the task set, the state, the results, another run or hidden material, and there was no suspicious search. The scanner is unchanged, and the flags stay in the records.

**Skills.** No session in any condition invoked a skill, though the plugin was loaded: every `skills` and interlock session listed its six skills and its `verifier` agent. The generated skills describe themselves as the steps of "an interlock task". A session with no interlock task has no reason to start them, and interlock's own sessions get their instructions from interlock's prompts. So in this run the `skills` condition measured plain Claude Code with the skills' descriptions in context, and the `interlock` condition measured the runtime. Neither measured skills in use.

**Spend for this step.** Claude Code reported $4.81 for the 46 runs, and $0.08 for three probe sessions (`checks/effort_probe_low.out.json`, `checks/skills_probe_generated.out.json`): **$4.89 reported**. Ten sessions were killed by the forced interruptions and reported no cost. At the $0.30 reserve each, another $3.00 was charged against the budget, for **$7.89 charged against the $8.50 cap**. The plan had estimated about $7.7, so the budget never had to stop a run.

### What v3 showed, at the time

The design's first goal is falsifiable: better reliability, with an overhead we understand. This run could have falsified the reliability half, and on these tasks it points the wrong way.

**Did any condition fail a harder task? Yes.** On the three harder tasks and `py-date-filter`, interlock failed 3 of 8 runs, plain 1 of 8, and skills none. Every failure was a completion claim the hidden checks reject. Plain's one failure and two of interlock's three named the unhandled case in the final message. The seven original tasks were at ceiling again: 7 of 7 in every condition.

**Reliability: no evidence that interlock helps here, and weak evidence that it hurts.** Over all 45 runs, interlock was accepted 12 of 15 times, plain 14 of 15 and skills 15 of 15. With samples this small, none of these differences is significant. Fisher's exact test, two-sided, gives p = 0.22 for interlock against skills and p = 0.60 against plain; on the eight harder runs alone, p = 0.20 and 0.57. The runs are not independent either: 11 tasks, four of them run twice. With no failures in 15 runs, skills' failure rate is bounded at 18.1% (exact one-sided 95%) or 21.8% (two-sided). So the data does not show that interlock is less reliable.

It does falsify a narrower claim: that interlock's verifier catches what the worker misses. In all three of interlock's failures, the verifier passed the work.

A plausible mechanism, which this data cannot confirm: interlock's worker prompt says "Make the smallest change that meets the criteria", and the verifier checks those criteria. The criteria restate the prompt, so whatever the prompt leaves implicit is outside what either session is asked to establish, whether that is a second call site or how a new unit composes. Plain and skills sessions get the prompt and its scope, with no such instruction, and in this run they failed less often. Three failures are too few to tell this from chance. A run that varies only that sentence, or one whose criteria state the implied requirements, would tell them apart.

**Overhead: measured again, at about 2.3 to 2.5 times.** interlock cost a median 2.53 times as much as plain and took 2.33 times as long, or 2.33 and 1.92 times skills. The verifier accounts for about three quarters of the extra cost. Loading the skills plugin cost a median 11% more than plain, with no skill ever invoked: the cost of their descriptions in context.

**Recovery: 3 of 3 in every condition.** Nine forced interruptions landed as designed; a tenth was re-run because a test had already finished. All nine recovered to an accepted outcome. Plain and skills carried on in the dirty working copy. interlock reconciled the dead attempt and started a fresh worker, which took longer: 65 s and 78 s mean wall time on the two interruption tasks, against 34 s and 71 s for plain.

**Claims without evidence and scope: no difference.** Every run claimed done, and every one of them ran at least one test or check. No run changed a file outside its scope, including the investigation, which now has an empty scope in interlock too.

**What this does not show:**

- Anything about skills in use. They were loaded and never invoked (see Results).
- Anything about Copilot CLI with a real model. Its runs used a scripted model and show only that the plumbing works.
- Code quality or speed of delivery. The design keeps both as hypotheses until a comparison supports them.
- Generality: eleven small tasks of our own making, one host, one model, one effort level, one or two repeats.

## Earlier results

The first live run, on the first version of the harness, compared `plain` and `interlock` only. The run above supersedes it; it is kept for the overhead comparison.

### The first live run, before the review (October 6)

These runs used the first version of the harness and task set (commit `3f930ee`): the seven original tasks, the old isolation, an environment denylist, effort through `CLAUDE_CODE_EFFORT_LEVEL`, and, for the investigation, plain tools that did not match interlock's worker (plain had edit tools and no web tools; interlock's worker the reverse). Claude Code 2.1.289, `claude-sonnet-5-5`, effort `medium`. Every task under `plain` and `interlock`, twice each: 28 runs, two of them the pilot. The raw records, transcripts and anonymized verdicts are in [evidence/evaluation/](../evidence/evaluation/README.md).

| Metric | plain | interlock |
| --- | --- | --- |
| Accepted outcomes | 14/14 | 14/14 |
| False completion claims | 0 | 0 |
| Missed defects | 0 | 0 |
| Runs with scope violations | 0 | 0 |
| Recovery after a forced interruption | 2/2 | 2/2 |
| Sessions per run, mean | 1.14 | 2.14 |
| Wall time per run, mean (median) | 22 s (19 s) | 46 s (39 s) |
| Cost per run, mean over the 12 runs with complete cost | $0.076 | $0.198 |

Paired by task and repeat, interlock cost a median 2.4 times as much as plain (range 2.0 to 6.0, 12 pairs) and took a median 2.1 times as long (range 1.3 to 2.8, 14 pairs). Of the extra cost, about three quarters was the verifier session.

With no failures in 14 runs, each condition's failure rate was bounded at 19.3% (exact one-sided 95%) or 23.2% (two-sided); excluding the two pilot runs, 20.6% and 24.7%. Neither condition was shown to be more reliable: the seven original tasks were too easy for this model at this effort.

The interruptions in these runs killed each session at its first edit, after three or four files had changed, rather than at a random point before a test run; the review's finding 3, fixed since.

**Isolation, checked after the fact.** The review showed these runs could have reached reference solutions and earlier runs' outputs. The new scanner, run over all 28 runs' transcripts in the layout they used (`evidence/evaluation/checks/rescan_oct6.out.json`), finds no tool call that reaches the frozen repositories, start trees, results, task set, harness, another run, or hidden material, and no suspicious search. That is evidence no agent used the opening, not proof: the scanner reads tool calls, and a path built at run time inside a script would escape it.

## Limitations

- **Stand-in tasks.** Written for this harness and small. They are not the S3 set.
- **Small n.** One or two repeats per task and condition: v3 had 15 runs per condition, and v4 reran the conditions it changed. `py-split-remainder` still has one run per condition.
- **v4 is in-sample.** Its prompt wording answers the harder tasks' hidden requirements, read from v3's failures and the tasks' labels. Held-out tasks with new requirement categories, and an ablation (v3's prompts on 2.1.291), are still to do.
- **The plain condition is told how to report.** It is asked for a closing `STATUS:` line, so that its completion claim can be read without a model's judgement.
- **interlock's criteria spell the task out.** Each task file breaks the prompt into criteria, some with checks. That is the intervention being measured, but it also means interlock gets the task stated twice. The criteria add no requirement or hint the prompt lacks.
- **No sandbox.** See Isolation: the layout and the scanner keep agents away from the answers and report attempts, but the agents could read them.
- **Killed sessions have no cost.** The host reports cost only at the end of a session.
- **Wall times are noisy.** The machine's 4 CPUs were shared with other work.
- **The host loads its own plugins.** Every session, in every condition, also listed three plugins installed in this Claude Code environment (`cc-plugin-agents-md`, `cc-plugin-plugin-authoring`, `cc-plugin-telemetry`), despite `--setting-sources ""`. They are the same in every condition.
- **The harder tasks' hidden checks embed judgements.** See Results v3: the `go-duration-days` cases are defensible, not inevitable.
- **The host moved.** Claude Code updated itself from 2.1.289 (v3) to 2.1.291 (v4), so v4's interlock and skills runs on the harder tasks are compared with plain runs on an older host.
- **The verifier's new failure path is untested live.** No v4 worker left a gap for it to find.
