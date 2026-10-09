# interlock 0.1.0

interlock makes a coding agent prove its work before you take it. You describe a task and how to tell it is done: a check that shows the bug, your test command. interlock gives the work to an agent such as GitHub Copilot CLI, runs your checks itself on exactly what the agent produced, has a second, separate agent session verify it, and sends it back with the reason if anything fails. Only when the evidence holds does it hand you the change, as one commit you apply.

Your own branch and working files are never touched while it works.

- [How it works](#how-it-works)
- [Install](#install)
- [Try it in ten minutes](#try-it-in-ten-minutes)
- [Use it on your own project](#use-it-on-your-own-project)
- [Writing a task](#writing-a-task)
- [Working with Copilot CLI: two ways](#working-with-copilot-cli-two-ways)
- [Reading what happened](#reading-what-happened)
- [Delivering to GitHub (optional)](#delivering-to-github-optional)
- [Commands](#commands)
- [When something goes wrong](#when-something-goes-wrong)
- [What interlock does not do](#what-interlock-does-not-do)
- [What is in this package](#what-is-in-this-package)

## How it works

```
  you: task.toml ──► interlock
                       │ 1. records your commit; runs your checks on it:
                       │    the bug check must fail, your tests must pass
                       ▼
                     Copilot CLI session: the WORKER
                       │ works in its own copy (a git worktree), only on the files you allowed
                       │ interlock runs the checks on its files; the worker cannot stop until it has
                       ▼
                     Copilot CLI session: the VERIFIER (separate, read-only)
                       │ interlock runs the checks again; the verifier looks for what the checks miss
                       ▼
                     evidence holds? ── no ──► back to a new worker, with the verifier's reason
                       │ yes
                       ▼
                     done: the change is kept as refs/interlock/tasks/<task>
  you: git cherry-pick ◄┘
```

Five ideas carry it:

1. **A task says what "done" means.** A small file, `task.toml`: what you want in plain words, which files may change, and one or more criteria. A criterion is a statement ("retrying writes no duplicate rows") and, wherever possible, a check: a shell command that exits 0 when the statement holds.
2. **Evidence, not claims.** An agent saying "tests pass" counts for nothing. interlock runs each check itself, on the exact files the agent produced, in a clean copy, and keeps the output. Before any agent starts it runs the checks on your commit too: a bug check that already passes, or a test command that runs no tests, proves nothing, so the task stops there.
3. **Two sessions, not one.** The worker makes the change. A separate verifier session, which cannot edit, judges it against your intent and the checks. If it finds a gap, interlock starts a new worker with the verifier's reason.
4. **Every step is a recorded move.** A task goes `pending → ready → running → awaiting verification → verified → done`, or to `blocked` or `failed` with the reason. Each move has a rule (the moves are named G1 to G7, and R1 to R3 for going back), and interlock refuses a move the evidence does not allow. `interlock task log <task>` lists every move and why.
5. **Your repository stays yours.** Agents work in separate worktrees under `.interlock/`. interlock's hooks refuse their edits outside the task's files, pushes, and commands that would change interlock's own records. The result waits as a commit you apply when you choose.

A real run of the example below, on this release: the first worker fixed the bug and the check passed, but the verifier found the fix broke inputs the old code handled (`export(d.values(), …)` now raised a TypeError). interlock sent it back with that reason; the second worker fixed both, the verifier passed it, and the task was done in four sessions.

## Install

**You need:**

- macOS (Apple silicon or Intel) or Linux on x86_64. Windows is not supported.
- git.
- A coding agent: **GitHub Copilot CLI** (`npm install -g @github/copilot`, which needs Node 22 or later), signed in, on a Copilot plan; or Claude Code. This release was tested with Copilot CLI 1.0.91 (driven by a scripted model) and run live with Claude Code 2.1.295.
- Whatever your checks run (the example needs `python3`).

**Then, from this package's folder:**

```bash
./install.sh
```

It puts one program, `interlock`, in `~/.local/bin` (`./install.sh --prefix DIR` for elsewhere) and says if that folder is not on your PATH yet.

- **On Linux** it installs the program in `bin/linux-x86_64/`, which runs on any distribution.
- **On a Mac** it fetches the program GitHub built and tested from this release's exact commit, using your GitHub CLI sign-in (`brew install gh`, then `gh auth login`). Without `gh`, it builds from the included source if you have Rust (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`; the build takes a few minutes). Or download `interlock-macos-universal` yourself from the repository's Actions page and run `./install.sh --from ~/Downloads/interlock-macos-universal.zip`. `RELEASE.txt` names the commit.

Check it:

```bash
interlock --version                              # interlock 0.1.0
interlock host inspect --host copilot --check-auth   # one small Copilot request: is it signed in?
```

The second command prints a report; look for `"auth": "ok"`. If it says `failed`, sign Copilot in (`copilot`, then `/login`). A `GH_TOKEN` or `GITHUB_TOKEN` in your environment takes precedence over that login, so it must be one Copilot accepts.

## Try it in ten minutes

The example is a small Python exporter with a real bug: after a transient failure it retries the whole batch, so rows already written are written twice.

```bash
cp -R examples/export-retry ~/interlock-tour
cd ~/interlock-tour
git init -q && git add -A && git commit -qm "Exporter with retry"

interlock init                                   # once per repository: creates .interlock/
interlock task create task.toml                  # read the task file first: it is short
interlock run export-retry --host copilot        # a few minutes
```

`interlock run` works through the task and ends with one line, for example:

```
interlock: export-retry is done after 2 sessions. Apply the verified change with: git cherry-pick refs/interlock/tasks/export-retry
```

Above it, on standard output, is the full report as JSON (`final_state`, each session, what it cost). Then:

```bash
interlock task log export-retry                  # every move and why
git diff HEAD refs/interlock/tasks/export-retry  # the verified change
git cherry-pick --no-commit refs/interlock/tasks/export-retry
git diff --cached                                # review it, then commit it as yourself
git commit -m "Fix duplicate rows when an export retries"
```

`--no-commit` stages the change so you write the commit message; plain `git cherry-pick` commits it as is, with the message `interlock: <task>, verified` and the task's intent.

## Use it on your own project

1. **Commit your work** in the repository. interlock and the agents start from your last commit; uncommitted changes are not part of it.
2. **`interlock init`** in the repository, once. It creates `.interlock/` (its records and the agents' worktrees), which git ignores on its own.
3. **Write a task file** (next section). Start from `templates/task.toml`. Commit any check script it runs: interlock refuses to start while a check file is uncommitted, because the agents would not have it.
4. **`interlock task create task.toml`**. It refuses a malformed task and says which field to fix.
5. **Run it**: hands-off with `interlock run <task> --host copilot`, or guided, with you in a Copilot session (see [two ways](#working-with-copilot-cli-two-ways)).
6. **Apply the result**: `git cherry-pick --no-commit refs/interlock/tasks/<task>`, review, commit.

Tasks live in the store until you no longer need them; a task id can be used once per repository.

## Writing a task

```toml
id = "export-retry"
repository = "."
workflow = "bug-fix"
intent = "Fix duplicate rows when an export retries: after a transient failure mid-batch, rows written before the failure are written again."
environment = "linux-python3"

[budget]
max_attempts = 3

[scope]
paths = ["exporter.py", "tests/**"]

[[criterion]]
id = "repro"
statement = "Retrying an export after a transient failure produces no duplicate rows"
check = "sh checks/export-retry.sh"
min_strength = "observed"
producer = "independent"
baseline = "fails"

[[criterion]]
id = "regression"
statement = "The existing unit tests pass"
check = "python3 -m unittest discover -s tests -q"
min_strength = "tested"
producer = "self"
baseline = "passes"
```

| Field | What it means |
| --- | --- |
| `id` | A short name, unique in the repository |
| `workflow` | `bug-fix`, `feature` or `refactor` change code; `investigation` answers a question and changes nothing |
| `intent` | What you want, as you would tell a colleague. Both agents get it, and the verifier judges the work against it, not only against the checks |
| `scope.paths` | Files and globs the work may change. A result that changes anything else is refused. `[]` allows no change (right for an investigation); `["**"]` allows any |
| `[budget]` | `max_attempts` (worker sessions, default 3); optionally `max_wall_secs`, `max_premium_requests` (Copilot), `max_cost_usd` (Claude Code). The task stops when one runs out |
| `[[criterion]]` | One per thing that must be true at the end |
| `check` | A shell command, run from the repository root, that exits 0 when the criterion holds. interlock runs it itself |
| `baseline` | `fails`: it must fail on your commit (a reproduction of the bug). `passes`: it must pass there (a guard, like your test suite). Omit it for neither |
| `producer` | `independent`: only the verifier's evidence counts. `self`: the worker's interlock-run check is enough |
| `min_strength` | `observed` (interlock saw the check pass), `tested` (a test ran), or `static` (read and reasoned about) |
| `integration_required` | `true` to have interlock deliver the change to GitHub when verified (see below). Default `false` |

**Good tasks:**

- **Make the bug a command.** A check that fails today for the right reason is the strongest thing you can give. A three-line script under `checks/`, committed, is enough.
- **Keep your normal test command as a guard** with `baseline = "passes"`. interlock refuses a test command that runs no tests.
- **Say the goal, not just the check.** Write in `intent` what a careful engineer would also make sure of: the verifier looks for that.
- **Keep the scope tight.** It is also what stops the agent "fixing" the check.
- **Criteria without a check** are allowed (a statement the verifier judges by reading), but only a verifier interlock started can pass them.

## Working with Copilot CLI: two ways

| | Hands-off: `interlock run` | Guided: you in a Copilot session |
| --- | --- | --- |
| Who drives | interlock starts Copilot sessions itself (`copilot -p`) and decides each step | You talk to Copilot as usual; interlock's skills steer it and its hooks enforce the rules |
| You write the task file | Yes | No: Copilot writes it with you, from your request |
| Questions mid-way | None: anything the task does not allow is refused | Copilot asks you before anything the task does not cover |
| Verifier | A separate Copilot session interlock starts | Also a separate session: Copilot runs `interlock verify`, which starts one |
| Best for | Well-defined fixes you can state with checks | Exploring, deciding what "done" means, larger changes |

### Hands-off

```bash
interlock run <task> --host copilot
interlock run <task> --host copilot --model <a model your plan offers> --effort high
```

It returns when the task is done (exit 0), or blocked, failed or out of budget (exit 5), with the reason in the last line. Ctrl-C stops the session cleanly (exit 6); the same command resumes. One run starts at most 6 sessions (`--max-sessions`) and gives each 20 minutes (`--timeout`); if it stops there, run it again to continue. Each Copilot session is about one premium request times the model's rate; a task takes at least two (a worker and a verifier), more when work is sent back. Cap it with `max_premium_requests` in `[budget]`.

### Guided

Once per repository:

```bash
interlock setup --host copilot
```

This writes six skills to `.github/skills/interlock-*` (commit them to share them with your team), the hooks to `.interlock/guided/copilot-plugin/` (local to you), and saves Copilot's details. It prints the command to start a session:

```bash
copilot --plugin-dir .interlock/guided/copilot-plugin
```

Keep `interlock` on the PATH of that terminal. Then ask for what you want in your own words, for example *"Fix the bug where export writes rows twice after a transient failure"*. The skills take it from there:

| Skill | What it does |
| --- | --- |
| `interlock-route` | Starts any request: picks the workflow, writes the criteria and checks with you, creates the task, records your commit |
| `interlock-implement` | The worker's playbook: reproduce first, fix the cause, run the checks through interlock, claim them |
| `interlock-investigate` | For "how does this work / why / which change caused it": an answer with file and line evidence, no changes |
| `interlock-verify` | Hands the work to an independent verifier (`interlock verify <task> --host copilot`) and lets interlock decide |
| `interlock-review` | An adversarial review of the submitted work before verification |
| `interlock-design` | Compares whole designs before a large change. You start it: type `/interlock-design` |

If Copilot answers without using them, type `/interlock-route` followed by your request. When Copilot asks to run something the task does not cover (a push, a network call), you decide. Edits outside the task's files and changes to interlock's records are refused outright.

When the verifier has passed the work, Copilot runs `interlock advance <task>`; its output gives the same `git cherry-pick` command under `apply_with`. `interlock status <task>` shows where things stand at any time.

### Claude Code

Everything above works with `--host claude-code` too. In a guided Claude Code session the verifier runs as interlock's verifier subagent: `interlock setup --host claude-code`, then `claude --plugin-dir .interlock/guided/claude-code-plugin`.

## Reading what happened

Commands print JSON (`brief` prints readable text); `jq` picks fields out of it (macOS 15 includes it).

```bash
interlock status <task>       # state, each criterion's verdict, what is missing, the next allowed moves
interlock task log <task>     # every move: G1 ... G7, R1 ..., with the reason
interlock brief <task>        # a readable summary: goal, criteria, evidence, previous attempts
interlock task events <task>  # results, each check run, each assessment with the verifier's note
interlock task list           # all tasks in this repository
```

What the moves mean: **G1** your commit recorded; **G2** a worker started; **G3** its result accepted (in scope, checks not touched); **G4** verified (every criterion has current passing evidence); **G5**/**G6** delivery to GitHub started and confirmed; **G7** done. **R1** sent back for rework with a reason; **R2** the code under evidence changed, so it goes back to verification; **R3** an attempt ended without a result (timed out, crashed, cancelled).

Check outputs, session transcripts and the store are under `.interlock/`.

## Delivering to GitHub (optional)

For a task with `integration_required = true`, interlock can push the verified commit to a branch `interlock/<task>`, open a pull request, wait for its checks, and merge it pinned to exactly the verified commit, so nothing else can slip in. It needs the GitHub CLI (`gh`) signed in, a `git` remote for the repository, and your explicit permission, given once per task:

```bash
interlock grant create --principal me --tasks <task> --classes landing --landing coordinator --origin "I approve landing this"
interlock integrate run <task>                   # or let `interlock run` continue into it
```

`--landing operator` instead makes interlock open the pull request and wait for you to merge it. Without a grant the task stops at `verified`, and you apply it yourself as above.

## Commands

| Command | Does |
| --- | --- |
| `interlock init` | Creates the store in this repository |
| `interlock task create task.toml` | Adds a task |
| `interlock run <task> --host copilot` | Works the task hands-off until done, blocked or failed |
| `interlock setup --host copilot` | Installs the skills and hooks for guided sessions |
| `interlock verify <task> --host copilot` | Starts an independent verifier session for a task awaiting verification |
| `interlock advance <task>` | Applies every move the evidence allows now |
| `interlock status <task>` / `task log` / `brief` / `task events` | What is happening and why |
| `interlock task unblock <task>` | Resumes a blocked task after you fixed the cause |
| `interlock task retry <task> --reason "..."` | Ends the current worker attempt so a fresh one starts |
| `interlock task cancel <task> --reason "..."` (or `task fail`) | Stops a task for good |
| `interlock task export <task>` / `task resume <task>` | Saves an interrupted worker's files on `interlock/wip/<task>` and continues from them |
| `interlock reconcile` | After a crash: settles sessions and GitHub operations nobody finished recording |
| `interlock grant create ...` / `grant list` | Gives or lists permissions (landing, external actions) |
| `interlock host inspect --host copilot` | What interlock found out about the agent: version, capabilities, sign-in |
| `interlock where` | Where this directory's store is |

`interlock <command> --help` lists every option. Exit codes: 0 success (or done), 5 the task is not done, 6 interrupted, 2 refused (the reason is on stderr), 3 no such task or record, 1 any other error.

## When something goes wrong

| You see | What it means, and what to do |
| --- | --- |
| `cannot start: checks/x.sh is run by a check but not committed as it is in your working directory` | interlock and the agents work on your last commit, where the check file is missing or different. Commit it, then run again |
| `blocked: fix the task's checks first. repro: the check already passes on the input snapshot` | Your bug check does not show the bug on your commit, so passing later would prove nothing. A task's checks are fixed when it starts: `interlock task cancel <task> --reason "..."`, correct the check, commit, and create the task again under a new id |
| `blocked: fix the task's checks first. ... the check ran nothing on the input snapshot` | Your test command ran no tests (often a wrong path or pattern), or `already fails`: your tests fail before any change. Same remedy: cancel, correct, commit, new task |
| `blocked: no independent verifier: ...` | The agent could not start a separate verifier. In guided Copilot sessions, make sure `interlock` is on the PATH, then `interlock task unblock <task>` |
| `failed: ... all 3 attempts are used` or `budget exhausted: ...` | The workers used up `max_attempts` or a `[budget]` limit. `interlock brief <task>` shows what each attempt tried; give the next task more room or a narrower scope |
| An auth failure in a session | Copilot is not signed in, or a `GH_TOKEN` it does not accept is set. `interlock host inspect --host copilot --check-auth` |
| `another supervisor holds the controller lock` | One `interlock run` per repository at a time. Wait for it; if it crashed, run again: it cleans up first |
| `network_filesystem` | The store must be on local disk (NFS, SMB and the like are refused). Use a local clone |
| `copilot X is installed, but .interlock/config.toml pins Y` | You pinned an agent version and a different one is installed. Install the pinned version or change the pin, then `interlock task unblock <task>` |

A run that dies (a closed laptop, a crash) loses nothing: run the same command again. It finds the session it left, follows or stops it, and continues.

## What interlock does not do

Be clear about these before relying on it:

- **It is not a sandbox.** Agents run as you, on your machine. interlock's hooks refuse what they can see in each command, and the agent's own tool settings narrow the rest, but a determined agent could get around a hook with a script. Use a separate machine or account for untrusted work.
- **On macOS,** a process an agent starts that deliberately detaches itself, clears its environment and loses its parent cannot be traced back to the session, so it is not stopped with it. Linux catches that case.
- **It has not been run with Copilot's real model yet.** Every part of the Copilot path (the CLI, its hooks, skills, agents, permissions) is tested on every change, on macOS and Linux, with a scripted model, and the whole flow has run live with Claude Code. Your first Copilot runs are the first with the real model; please report anything odd.
- **It does not yet show better results than an agent alone.** In a small evaluation on practice tasks, interlock with its skills and a plain agent both completed the tasks; interlock cost about 2.5 times as much. What it gives you is the evidence and the second look, which matter most on changes you cannot easily check yourself.
- **Linux on ARM** has no ready-made program; `./install.sh --build` builds one.

## What is in this package

| Path | What |
| --- | --- |
| `README.md` | This guide |
| `install.sh` | Installs `interlock` (see [Install](#install)) |
| `bin/linux-x86_64/interlock` | The program for Linux, static, any distribution |
| `examples/export-retry/` | The ten-minute example |
| `templates/task.toml` | A commented task to start from |
| `source/` | The complete source, with its own README and docs: `docs/runtime.md` (sessions, containment, budgets), `docs/skills.md` (guided sessions), `docs/forge.md` (GitHub delivery) |
| `RELEASE.txt` | Version, the exact commit, checksums |
| `LICENSE` | MIT |

To uninstall: delete `~/.local/bin/interlock`, and `.interlock/` (plus `.github/skills/interlock-*`, if you ran setup) in any repository you used it in.
