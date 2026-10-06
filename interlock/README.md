# interlock

A workflow runtime that only lets work advance on evidence. Agents propose moves; `interlock` checks each one against recorded evidence, the current attempt, and granted authority before a task advances. It is one Rust binary with a SQLite store.

This directory builds the design draft "A workflow runtime that only lets work advance on evidence" (draft 1, October 4, 2026). Every phase of its plan, P0 to P4, has code, tests and recorded runs. Where a gate could not be met here, the gap is stated under [What is not proven](#what-is-not-proven).

## Hosts

The core is host-agnostic. Hosts sit behind one adapter contract (`interlock-adapter`): each reports versioned capabilities, plans headless sessions, reads their output, and translates the core's host-neutral tool names into its own permission patterns.

- **GitHub Copilot CLI is a first-class host.** Its adapter is listed first and nothing in the core assumes another host. Its whole `interlock run` path, the guided path and delivery are tested on the real CLI, offline against a scripted model.
- **Claude Code is the host we test with live.** It runs headless in our development containers, where Copilot CLI cannot sign in to GitHub yet. Being the test host gives it no special status in the core.

Both hosts load the same Claude-format hooks plugin with `--plugin-dir`, send it the same payloads, and accept the same responses. The details, and how each was verified, are in [docs/host-spike-2026-10-05.md](docs/host-spike-2026-10-05.md).

| Capability | Copilot CLI 1.0.91 | Claude Code 2.1.289 |
| --- | --- | --- |
| Headless session | `-p`, `--output-format json` (JSONL) | `--print`, prompt on stdin, `stream-json` |
| Cancel and timeout | kill the process group | kill the process group |
| Tool restriction | `--allow-tool`, `--deny-tool` | `--allowedTools`, `--disallowedTools`, `--permission-mode dontAsk` |
| Per-call policy | plugin `PreToolUse` hook | plugin `PreToolUse` hook |
| Stop guard | plugin `Stop` hook | plugin `Stop` hook |
| Model and effort | `--model`, `--reasoning-effort` | `--model`, `--effort` |
| Session id chosen by interlock | `--session-id` | `--session-id` |
| Skills | `.github/skills/interlock-*` | plugin, namespaced `interlock:` |
| Custom agents | `--agent` | `--agents` |

One policy, two hosts. `interlock host tools <task> --role verifier --host <host>` turns the bug-fix verifier's host-neutral policy into each host's patterns:

| | Allow | Deny |
| --- | --- | --- |
| Host-neutral | `read`, `shell` | `edit`, `shell:git push`, `shell:git commit` |
| Copilot CLI | `shell` (reading is never gated) | `write`, `shell(git push:*)`, `shell(git commit:*)` |
| Claude Code | `Read`, `Grep`, `Glob`, `Bash` | `Edit`, `Write`, `NotebookEdit`, `Bash(git push:*)`, `Bash(git commit:*)` |

## Running a task

`interlock run <task> --host <copilot|claude-code>` drives a task through headless sessions until it is done, blocked, failed, or waiting on the operator:

1. **Reconcile**: settles whatever a previous controller left open: forge operations nobody confirmed, and sessions without a recorded end in any task (live ones are re-attached or stopped, dead ones get a synthetic crash report).
2. **G1**: records the repository's `HEAD` as the input snapshot, with the files the checks run marked as protected.
3. **Baseline**: interlock runs each check that promises a baseline on the input snapshot. A reproduction must fail there and a regression guard must pass there, and neither may run nothing. A task that breaks its promise is blocked before any session starts. `--max-sessions 0` stops here.
4. **Worker**: opens an attempt (G2) in a fresh git worktree under `.interlock/worktrees/`, with a brief built from records and the attempt's effective grant as the host's tool filters. When the session ends, interlock writes the worktree's tree through a temporary index and submits it as the result (G3), reading the changed files from the tree itself. The repository's branch and index are never touched.
5. **Verifier**: opens an independent attempt in a worktree checked out at that tree, with read and test tools only. It records one assessment per criterion, at the strength each criterion needs.
6. **Advance**: G4 when the evidence holds; R1 with a fix brief when an independent check fails; R3 when a session times out, crashes or is cancelled.
7. **Deliver**: G7 when the workflow needs no delivery. Otherwise G5, the pinned merge and G6 through the forge (see [Delivery](#delivery)), if the task's grant gives landing authority.

Every tool call in those sessions passes through `interlock hook pre-tool-use`. That hook:
- denies edits outside the attempt's worktree or the task's scope;
- denies anything that reaches interlock's own state: the store, attempt tokens, other processes' environments and, for headless sessions, anything under `.interlock/` outside the attempt's own worktree;
- denies commands the grant does not cover, including the operator's own commands (`grant`, `task unblock`, `integrate`, `reconcile`, `run`) and pushes anywhere but an interlock branch (headless sessions have nobody to ask).

`interlock hook stop` holds a worker until interlock has run each self-checked criterion's check on its final files and the worker has claimed it, and holds a verifier until it has done the same for every criterion at the strength the criterion needs.

The session's environment is built from an allowlist, so a session started from inside another agent never sees that agent's session id, message socket, account details or GitHub token. Processes a session starts are stopped with it, including ones that leave its process group. A run stops on its wall-clock, cost or premium-request budget, and a host whose version differs from a pin in `.interlock/config.toml` blocks the task before any session. Ctrl-C, SIGTERM and SIGHUP stop the session, cancel its attempt and leave the task to resume (exit 6). `interlock task export` saves an interrupted worker's files as a `wip:` commit on `interlock/wip/<task>` and `task resume` starts the next worker from it. Details: [docs/runtime.md](docs/runtime.md).

### Evidence interlock saw

An agent's word is not enough for a criterion that names a check. `interlock check run --criterion <id>` runs the check itself, on the exact tree in the agent's worktree, and keeps the output as a content-addressed artifact. Evidence policy v2 requires, for a checked criterion:

| Rule | Why |
| --- | --- |
| A passing run on the current tree, asked for by an allowed producer (a verifier or the operator, for independent criteria) | The pass is something interlock saw |
| A failing run by a verifier fails the criterion outright | Same as a failed independent assessment |
| A run whose output shows nothing was tested never passes and never fails | `Ran 0 tests`, `collected 0 items`, `running 0 tests`, `[no test files]`, `No tests found`, `0 passing` |
| `baseline = "fails"`: the check failed on the input snapshot | A reproduction that already passes proves nothing |
| `baseline = "passes"`: the check passed there | A regression guard that already fails, or tests nothing, guards nothing |

A criterion with no check is decided by assessments alone, so for it a verifier must be one whose independence interlock established: a session interlock launched, or a subagent the host named to the hooks. A verifier attempt nobody bound counts only where interlock ran the criterion's check.

A result whose changed files leave the task's scope, or touch a file a check runs, is rejected at G3. An empty scope allows no change, and an investigation may change nothing at all. The change set is read from the output tree in the repository that owns the store, so neither a shell write (`sed -i`, `>`) nor a misleading `GIT_DIR` gets around it.

Recorded runs of the [export-retry example](examples/export-retry/README.md) on Claude Code: the corrected task went from create to done in two sessions, and the original task, whose regression command ran no tests, was blocked before any session started.

## Guided sessions

`interlock setup --host <copilot|claude-code>` installs six skills (route, investigate, design, implement, verify, review), the verifier agent, and the hooks plugin in interactive mode, where hooks ask instead of deny. A person drives the session; the skills call `interlock` at every step. Setup keeps a manifest and never overwrites a file it did not write, unless `--force`.

A guided attempt governs only the host session that opened it. The verifier must be independent: on Claude Code it runs as the `interlock:verifier` subagent, and its attempt is bound to that subagent; where the host cannot name a subagent to the hooks, `interlock verify <task> --host <host>` launches the verifier as a headless session. The worker's own session cannot open a verifier attempt for its task. Designs, reviews and answers are kept in the store with `interlock note`. Details, including the S1 spike on user-invoked skills: [docs/skills.md](docs/skills.md).

`interlock run --skills <plugin>` loads the same generated skills (`interlock skills generate --target claude-code`) into headless sessions.

## Delivery

A verified task that needs integration lands through `interlock-forge`, which drives `gh` and `git`. Details: [docs/forge.md](docs/forge.md).

- **One commit lands**: the verified head, a deterministic unsigned commit of the verified tree on the snapshot base. It is pushed to `interlock/<task>` and merged only with `gh pr merge --match-head-commit <head>`.
- **Operations first**: every call that changes the forge runs under an operation row committed as planned, then started, before the call. Its outcome is read from the forge afterwards, never from the call's exit code. A call that dies unanswered leaves the operation unknown and the task blocked until `interlock reconcile` can tell.
- **What counts as G6**: the merge must contain exactly the verified tree, on the verified base, made while landing authority was granted. A moved head, a moved base, a stale tree or a retargeted pull request sends the task back for verification (R2) or blocks it with the reason.
- **Landing authority**: `none` blocks the task at verified. `coordinator` and `owner` let interlock merge. `operator` lets interlock open the pinned pull request and wait: G6 comes only from reconcile seeing the operator's merge of that exact head.

## Evaluation

[docs/evaluation.md](docs/evaluation.md) is the harness for the design's §13 evaluation, run on a stand-in for the S3 frozen task set: eleven tasks with hidden executable checks, including two forced interruptions and three tasks whose prompts leave out a requirement a careful engineer should find. Conditions: the host's plain workflow, skills alone, and skills with interlock, with host, model and effort pinned, tools at parity, and each run isolated from the answers.

The first live run (Claude Code, seven tasks, two repeats, before the harness review) accepted 14 of 14 in both conditions it ran. interlock cost a median 2.4 times as much and took 2.1 times as long, most of it the verifier session. That task set was too easy to show a reliability difference; both failure rates are bounded at about 19 to 23 percent. Evidence: [evidence/evaluation/README.md](evidence/evaluation/README.md).

## What is built

| Area | State | Docs and evidence |
| --- | --- | --- |
| S4: JSON Schemas for all records (`schemas/`), ten with `check_run` | Done; Rust types are checked against them by a conformance test | |
| Policy core: lifecycle, G1–G7, R1–R3, block, fail, cancel | Done; pure functions, no IO | |
| Evidence policy v2: check runs by interlock, baselines, empty-run detection, output scope at G3, verifier binding | Done; property-tested | |
| Grants: profiles, intersection, expiry, landing authority, irreversible never grantable | Done | |
| SQLite store: one transaction per move, append-only evidence, idempotent events, operation rows | Done | |
| Host adapters, session runner, hooks plugin | Done for Copilot CLI and Claude Code | [host spike](docs/host-spike-2026-10-05.md) |
| P2 supervisor (`interlock run`) | Done | [export-retry runs](examples/export-retry/README.md) |
| P1 skills, generator, guided sessions, S1 | Done; live on Claude Code, scripted on Copilot | [docs](docs/skills.md), [evidence](evidence/skills/README.md) |
| P3 robustness: re-attach, reconcile, containment, signals, budgets, pins, export | Done | [docs](docs/runtime.md), [evidence](evidence/runtime/README.md) |
| P4 delivery: forge adapter, pinned merges, operations, reconcile | Done against a fake `gh`; live GitHub not yet | [docs](docs/forge.md), [evidence](evidence/forge/README.md) |
| §13 evaluation and S3 baseline | Harness and stand-in task set done; full three-condition run in progress | [docs](docs/evaluation.md), [evidence](evidence/evaluation/README.md) |

Each workstream was built, then reviewed by an independent agent that tried to break it; every confirmed finding became a regression test that fails before its fix. The review rounds are recorded in each evidence README.

## Invariant tests

Each invariant in the design has a fault test that passes today. The last rows are invariants this build adds.

| Invariant | Test |
| --- | --- |
| 1. No current evidence, no pass | A verifier pass on another tree is kept but does not count; the task stays awaiting. A tree recorded while integrating sends the task back (R2) and nothing merges |
| 2. Workers cannot override verifiers or grant themselves authority | Verifier fails, worker then claims pass: R1, never verified. Workers cannot record assessments; in a guided session, the session that did the work cannot open a verifier attempt for it, and a verifier attempt nobody bound counts only where interlock ran the check. Evidence tables reject UPDATE and DELETE. Irreversible grants are refused. On Copilot, a worker's `interlock grant create` is denied by the hook and no grant exists afterwards. A worker process that escapes its process group is stopped before any verifier starts, so it cannot report as one |
| 3. Changes invalidate evidence | A rebase after verification sends the task back through R2, and landing is refused. A merge onto a base nobody verified is not G6. The stop guard rejects a claim made before the worker's last edit |
| 4. Late workers cannot advance tasks | Retry, respawn, then the old result arrives: stored as superseded. A restarted supervisor re-attaches to a live session rather than starting a second |
| 5. Apply and acknowledge together | A duplicate event is a no-op; the process aborted mid-write, then the replay applies exactly once |
| 6. Permissions are bounded | On Copilot, a worker's `git push` is denied inside the live session; edits outside the worktree or scope, commands touching interlock's state, and the operator's commands are denied. Sessions get an allowlisted environment |
| 7. External effects are reconciled | Every forge call runs under an operation row already marked started. A fake `gh` kills `interlock` with SIGKILL right after merging; `interlock reconcile` and a restarted `interlock run` each finish at G6 with exactly one merge. A merge call that dies unanswered stays unknown until the forge shows the merge. On the attempt side, every session left without a recorded end, in any task, is re-attached, or stopped and reconciled with a synthetic report |
| Added: checks cannot be faked or emptied | On Copilot, end to end: a regression check that runs no tests, and a reproduction that already passes, each block the task before any session; a worker that rewrites the check to `exit 0` has its result rejected, however it builds and submits the tree, and the next worker must really fix the bug. Runs that tested nothing never pass or fail (property-tested) |
| Added: only the verified head lands | Property-tested over abbreviated and prefix SHAs; end to end, a moved head is refused by `--match-head-commit` |

## Changes from the design draft

- **R3, retry:** running back to ready when the current attempt is cancelled or times out. The draft's "cancel, respawn" fault test needs a path from running to a fresh attempt; R3 is that path, and it fails the task when the budget is spent.
- **One strength field on evidence.** The draft lists both strength and outcome on claims and assessments. Its own strength scale already includes `blocked` and `failed`, so a separate outcome would only allow contradictions.
- **Hand-written record types.** `typify` is still alpha (0.10.0-alpha.1). The schemas stay the source of truth: `interlock-schema` embeds them, the store validates every record it writes, and a conformance test catches drift.
- **A host-neutral tool vocabulary** (`read`, `edit`, `shell:<prefix>`, `web`, `mcp:<server>/<tool>`, `agent`) so grants are written once and translated per host.
- **One hooks plugin instead of per-host hook packages.** Both hosts load the same Claude-format plugin, so per-call policy and the stop guard are one implementation.
- **Check runs are a tenth record.** A `check_run` records interlock's own execution of a criterion's check, and criteria gained a `baseline`. The draft's "same-surface reproduction before and after the change" becomes a rule rather than an instruction.
- **Scope is enforced on the output tree**, and an empty scope allows no change.
- **Verifier binding.** Attempts record how interlock knows who holds them (interlock launched the session, the host named the subagent, the session that opened it, or nobody), and an unbound verifier's assessment does not decide a criterion without a check.
- **Landing authority `operator`** means interlock opens the pinned pull request and the operator merges it; interlock never merges or arms auto-merge under it.
- **The push is part of opening the pull request** rather than its own operation kind, and reads of the forge get no operation row.
- **Skills by host:** Claude Code gets a plugin namespaced `interlock:`, because it ships its own `verify` and `design` skills; Copilot gets skills named `interlock-<name>`. User-invoked skills keep `disable-model-invocation`, which S1 showed works as intended on both hosts.

## Open questions from the draft (§14)

These are the defaults this build uses until they are decided:

- **Name**: interlock.
- **Default authorization**: the conservative profile.
- **Forge**: GitHub only.
- **S3 task set**: a stand-in task set until a real repository is named. The harness takes one with `source = {git, commit}`.

## What is not proven

- **Live GitHub.** The container's GitHub token is invalid, so delivery ran against a fake `gh` and a local bare repository. GitHub's real messages, timing and branch protection are untested; [docs/forge.md](docs/forge.md) lists what a live run would add. Under auto-merge, a base that moves while the merge waits is detected after the merge, not prevented, unless the branch requires up-to-date branches.
- **Copilot CLI with a real model.** Copilot runs exercise the real CLI, hooks, permissions, skills and custom agents with a scripted model. Model behaviour under the skills is evidenced on Claude Code only.
- **The real S3 task set and its baseline.** The evaluation runs on stand-in tasks.
- **Containment is not a security boundary.** Hooks read command text and do not parse shell grammar; a path computed at run time gets past them. A same-user process can read another's environment. Attempt tokens are bookkeeping. Real containment comes from the host's tool restrictions and the operating system ([docs/runtime.md](docs/runtime.md) says what holds).

## Use

```bash
cargo build --release
export PATH="$PWD/target/release:$PATH"

interlock init
interlock host inspect --save
interlock task create task.toml
interlock run export-retry --host copilot          # or --host claude-code
interlock status export-retry
interlock task log export-retry
```

Guided, from a session a person drives:

```bash
interlock setup --host claude-code                 # skills, verifier agent, interactive hooks
interlock verify export-retry --host copilot       # an independent verifier where the host cannot bind a subagent
```

Delivery and recovery:

```bash
interlock grant create --principal operator --tasks export-retry --classes landing --landing coordinator --origin "land it"
interlock integrate run export-retry               # G5, the pinned merge, G6
interlock reconcile                                # settle operations nobody confirmed
interlock task export export-retry                 # save an interrupted worker's files on interlock/wip/<task>
interlock task resume export-retry
```

The same moves can be made by hand, as an interactive session would:

```bash
interlock task ready export-retry --base "$(git rev-parse HEAD)"
interlock attempt start export-retry --role worker --host copilot --worktree auto
interlock result submit --attempt <id> --token <token> --epoch 1 --tree auto --summary "..."
interlock attempt start export-retry --role verifier --host copilot --worktree auto
interlock check run --criterion repro            # interlock runs the check; credentials come from the attempt
interlock assess add --attempt <id> --token <token> --criterion repro --strength observed --tree auto
interlock advance export-retry
interlock brief export-retry
```

Every command prints JSON. Refusals exit 2 with the reason on stderr; unknown records exit 3; bad attempt tokens exit 4. `interlock run` and `interlock integrate run` exit 0 when the task is done and 5 otherwise; an interrupted run exits 6.

## Layout

| Crate | Job |
| --- | --- |
| `interlock-schema` | Record types; embeds and validates against `schemas/` |
| `interlock-core` | Lifecycle, guards, evidence policy, grants, classifier, scope, delivery verdicts, briefs. Pure |
| `interlock-store` | SQLite: migrations, transactions, append-only triggers, events, sessions, operations |
| `interlock-adapter` | Capability contract; Copilot CLI and Claude Code adapters; session runner, environment allowlist, containment; hook protocol and plugin |
| `interlock-supervisor` | `interlock run` and `interlock verify`: git worktrees, sessions, hooks, prompts, guided sessions, export |
| `interlock-forge` | GitHub delivery through `gh` and `git`: pinned merges, readiness, reconcile |
| `interlock-skillgen` | Canonical skills to Copilot, Claude Code and Agent Skills output; validation; setup |
| `interlock-cli` | The `interlock` binary |

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all --check
```

The end-to-end tests run whole `interlock run`, guided and delivery passes on the real Copilot CLI in offline mode against a scripted model, so they need no account and make no model calls. They skip unless the host binaries are available; set `INTERLOCK_REQUIRE_HOSTS=1` to make a missing host fail the test instead:

```bash
npm install @github/copilot
INTERLOCK_REQUIRE_HOSTS=1 INTERLOCK_COPILOT_BIN="$PWD/node_modules/.bin/copilot" cargo test --workspace
```

Set `INTERLOCK_COPILOT_BIN` or `INTERLOCK_CLAUDE_BIN` to point interlock at a specific binary. The evaluation harness has its own tests: `python3 eval/test_harness.py`.
