# interlock

A workflow runtime that only lets work advance on evidence. Agents propose moves; `interlock` checks each one against recorded evidence, the current attempt, and granted authority before a task advances. It is one Rust binary with a SQLite store.

This directory builds the design draft "A workflow runtime that only lets work advance on evidence" (draft 1, October 4, 2026). Phases P0 to P4 have code, tests and recorded runs, except spike S2 (the Copilot SDK), which was not run. An independent audit checked the build against the design item by item: [docs/conformance.md](docs/conformance.md). Gates not met here are listed under [What is not proven](#what-is-not-proven).

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
2. **G1**: records the repository's `HEAD` and a hash of its untracked inputs as the input snapshot, with the files the checks run marked as protected.
3. **Baseline**: interlock runs each check that promises a baseline on the input snapshot. A reproduction must fail there and a regression guard must pass there, and neither may run nothing. A task that breaks its promise is blocked before any session starts. `--max-sessions 0` stops here.
4. **Worker**: opens an attempt (G2) in a fresh git worktree under `.interlock/worktrees/`, with a brief built from records and the attempt's effective grant as the host's tool filters. When the session ends, interlock writes the worktree's tree through a temporary index and submits it as the result (G3), reading the changed files from the tree itself. The repository's branch and index are never touched.
5. **Verifier**: opens an independent attempt in a worktree checked out at that tree, with read and test tools only. It records one assessment per criterion, at the strength each criterion needs.
6. **Advance**: G4 when the evidence holds; R1 with a fix brief when an independent check fails; R3 when a session times out, crashes or is cancelled.
7. **Deliver**: G7 when the workflow needs no delivery. Otherwise G5, the pinned merge and G6 through the forge (see [Delivery](#delivery)), if the task's grant gives landing authority.

Every tool call in those sessions passes through `interlock hook pre-tool-use`. That hook:
- denies edits outside the attempt's worktree or the task's scope;
- denies anything that reaches interlock's own state: the store, attempt tokens, other processes' environments and, for headless sessions, anything under `.interlock/` outside the attempt's own worktree;
- denies commands the grant does not cover, including the operator's own commands (`grant`, `task unblock`, `integrate`, `reconcile`, `run`) and pushes anywhere but an interlock branch (headless sessions have nobody to ask). interlock also refuses those operator commands itself when its caller descends from a session it launched, however the command was spelled.

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

Runs of the [export-retry example](examples/export-retry/README.md) on Claude Code, described there (the raw records of those October 5 runs were not kept; later live runs keep theirs under `evidence/`): the corrected task went from create to done in two sessions, and the original task, whose regression command ran no tests, was blocked before any session started.

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

[docs/evaluation.md](docs/evaluation.md) is the harness for the design's §13 evaluation, run on a stand-in for the S3 frozen task set: eleven tasks with hidden executable checks, including two forced interruptions and three tasks whose prompts leave out a requirement a careful engineer should find. The "harder tasks" below are those three plus the second interruption task, `py-date-filter`. Conditions: the host's plain workflow, skills alone, and skills with interlock, with host, model and effort pinned, tools at parity, and each run isolated from the answers.

Two runs on the integrated build, with Claude Code, `claude-sonnet-5-5`, medium effort and ABBA order. v3 had 15 counted runs per condition. v4 came after the two fixes below; it reran `skills` and interlock on the harder tasks and added a second repeat of all three conditions on six of the seven original tasks, where the budget stopped it. `plain` was not rerun on the harder tasks: nothing interlock changed reaches it, and its tools are identical, but Claude Code updated itself between the runs, so that comparison crosses host versions.

| Accepted | plain | skills | interlock with skills |
| --- | --- | --- | --- |
| Harder tasks, v3 (two repeats each) | 7/8 | 8/8 | 5/8 |
| Harder tasks, v4 | not rerun | 8/8 | 8/8 |
| Original tasks, v3 (one run each) | 7/7 | 7/7 | 7/7 |
| Original tasks, v4 (a second run, six tasks) | 6/6 | 6/6 | 6/6 |
| False completion claims, v3 (15 runs each) | 1 | 0 | 3 |
| False completion claims, v4 | 0 (6 runs, original tasks only) | 0 (14) | 0 (14) |
| Scope violations, recovery after a forced interruption | 0, all | 0, all | 0, all |
| Cost and wall time, paired, v4 | | 1.1x, 1.1x plain | 2.4x, 2.4x skills; 2.8x, 2.8x plain |

What changed between v3 and v4, and what it shows:

- **The fix for interlock's v3 failures.** All three v3 failures were on requirements the goal implies but the criteria leave out, and in each one the verifier passed the work. The worker prompt said "make the smallest change that meets the criteria", and the verifier judged only the criteria. Now the worker is sent to the goal and the root cause wherever it occurs, and the verifier judges the work against the goal and fails a gap with its reason ([docs/evaluation.md](docs/evaluation.md) has the prompts before and after).
- **interlock then went from 5 of 8 to 8 of 8 on the harder tasks. That is not significant** (Fisher two-sided p = 0.20), and no difference in either run is: the smallest p, 0.20, is this one and v3's interlock against skills on the same tasks.
- **And it is not a held-out result.** The new prompt sentences were written after reading v3's failures and the tasks' hidden-requirement labels, one per category, so v4 tests them on the tasks they answer. The host update and the new skill text, which every interlock session loads, changed at the same time. Testing the prompts needs held-out tasks with new requirement categories and an ablation (v3's prompts on the new host).
- **No v4 run was sent back for rework.** The verifiers looked for other call sites and implied inputs and found no gap, so the verifier's new failure path is untested on a live gap.
- **Skills are still not used.** No session invoked a skill in v3, because the skills described themselves as steps of an interlock task and could not work without one. `implement` and `investigate` now trigger on ordinary requests and work without interlock. A scripted Copilot session shows they load and reach the model. But Claude Code's model invoked none in 28 v4 runs or 2 probes. So the skills condition still measures their descriptions in context, not skills in use.
- **The verifier is about three quarters of interlock's extra cost.**
- **Claude Code updated itself** from 2.1.289 to 2.1.291 between the runs.

The earlier run, before the harness review, accepted 14 of 14 in both conditions it ran. Spend for every run, the harness's isolation and the task set: [docs/evaluation.md](docs/evaluation.md) and [evidence/evaluation/README.md](evidence/evaluation/README.md).

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
| §13 evaluation and S3 baseline | Harness, stand-in task set and a three-condition run on Claude Code done; interlock showed no benefit at this size (see [Evaluation](#evaluation)) | [docs](docs/evaluation.md), [evidence](evidence/evaluation/README.md) |

Each workstream was built, then reviewed by an independent agent that tried to break it. Confirmed findings became regression tests that fail before their fix, with two exceptions recorded in the evidence: one skills finding was fixed in skill text only, and one forge mutation (m08b) still passes with its fix undone, because git 2.43 ignores the setting it guards. The review rounds are recorded in each evidence README.

## Invariant tests

Each invariant in the design has a fault test that passes today. The last rows are invariants this build adds.

| Invariant | Test |
| --- | --- |
| 1. No current evidence, no pass | A verifier pass on another tree is kept but does not count; the task stays awaiting. A tree recorded while integrating sends the task back (R2) and nothing merges |
| 2. Workers cannot override verifiers or grant themselves authority | Verifier fails, worker then claims pass: R1, never verified. Workers cannot record assessments; in a guided session, the session that did the work cannot open a verifier attempt for it, and a verifier attempt nobody bound counts only where interlock ran the check. Evidence tables reject UPDATE and DELETE. Irreversible grants are refused. On Copilot, a worker's `interlock grant create` is denied by the hook and no grant exists afterwards. A session that strips its own environment and calls interlock through a variable the hook cannot read is still refused every operator command, because an ancestor process carries the session's marker. A worker process that escapes its process group is stopped before any verifier starts, so it cannot report as one |
| 3. Changes invalidate evidence | A rebase after verification sends the task back through R2, and landing is refused. A merge onto a base nobody verified is not G6. The stop guard rejects a claim made before the worker's last edit |
| 4. Late workers cannot advance tasks | Retry, respawn, then the old result arrives: stored as superseded. A restarted supervisor re-attaches to a live session rather than starting a second |
| 5. Apply and acknowledge together | A duplicate event is a no-op; the process aborted mid-write, then the replay applies exactly once |
| 6. Permissions are bounded | On the real Copilot CLI (driven by a scripted model), a worker's `git push` is denied inside the session; edits outside the worktree or scope, commands touching interlock's state, and the operator's commands are denied. Sessions get an allowlisted environment |
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
- **Copilot through its CLI, not its SDK.** The draft chose between the Rust Copilot SDK and a Node sidecar after spike S2. S2 was not run. The adapter runs `copilot -p --output-format json` as a subprocess behind the same `Host` trait as Claude Code, so there is no `interlock-copilot` crate and no out-of-process adapter protocol. This works offline and needs no SDK, and cancelling is a process-group kill, but SDK parity (sessions over JSON-RPC, sub-agent lifecycle events) is unverified.
- **Threads, not tokio.** Sessions, watchers and signal handling use threads and `signal-hook`; nothing needed an async runtime.
- **Scope holds under every profile.** The draft lets the permissive profile edit freely; this build enforces the task's scope at the hook and at G3 under both profiles.
- **The host policy is configuration.** `[host_policy.<host>]` in `.interlock/config.toml` denies tools or limits action classes for one host; G2 refuses a task whose grant then lacks a tool its role needs.
- **Verification on Copilot** uses `interlock verify` (a session interlock launches) rather than a custom agent, because Copilot's hooks cannot name the calling subagent.

## Open questions from the draft (§14)

These are the defaults this build uses until they are decided:

- **Name**: interlock.
- **Default authorization**: the conservative profile.
- **S3 baseline repository**: none named yet; the evaluation uses a stand-in task set. The harness takes a real one with `source = {git, commit}`.
- **PR watcher**: written in Rust as the forge adapter's readiness polling, not a TypeScript sidecar.
- **The six v1 skills and four workflows**: built as drafted; whether they cover what you need first is yours to say.
- **Forge**: GitHub only.

Of the draft's risks: S1 settled skill loading on both hosts; S2 (the Rust SDK) was not run, see above; a second host, Claude Code, is now tested, so portability is shown for two hosts; the store stays one controller per checkout on local disk, and nothing detects a network filesystem.

## What is not proven

- **Live GitHub.** The container's GitHub token is invalid, so delivery ran against a fake `gh` and a local bare repository. GitHub's real messages, timing and branch protection are untested; [docs/forge.md](docs/forge.md) lists what a live run would add. Under auto-merge, a base that moves while the merge waits is detected after the merge, not prevented, unless the branch requires up-to-date branches.
- **Copilot CLI with a real model.** Copilot runs exercise the real CLI, hooks, permissions, skills and custom agents with a scripted model. Model behaviour under the skills is evidenced on Claude Code only.
- **That interlock improves outcomes.** On the stand-in tasks it first accepted fewer runs than plain or skills alone (v3), then, with prompts that send the worker and the verifier to the goal, as many (v4). Neither difference is significant, v4 is in-sample (its prompts were written against these tasks' failures) and confounded with a host update, and interlock costs about 2.5 times as much. Whether its verifier now catches a requirement nobody wrote down is untested: no v4 worker left one.
- **Skills in use.** The skills condition never saw a skill invoked by a live model, in v3 or v4.
- **The real S3 task set and its baseline.** The evaluation runs on stand-in tasks.
- **Spike S2 and the P0 adapter decision.** See "Copilot through its CLI" above. The P0 gate's choice between a Rust SDK adapter and a Node sidecar was not made; a third option was taken without S2.
- **Schemas are still v0.** P1 asked for v1 schemas; the records grew fields (check runs, bindings, handoffs, operations) but their `$id`s still say `v0`.
- **Lifecycle guards are tested by example, not by property.** The evidence policy, empty-run detection and pinned landing are property-tested; G1 to G7 have example fault tests.
- **Containment is not a security boundary.** Hooks read command text and do not parse shell grammar; a path computed at run time gets past them. Operator commands check their caller's process ancestry, which holds in sessions interlock launches; in a guided session a person drives, they rely on the hook asking the person, which a command hidden behind a shell variable gets past. A same-user process can read another's environment. Attempt tokens are bookkeeping. Real containment comes from the host's tool restrictions and the operating system ([docs/runtime.md](docs/runtime.md) says what holds).

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
