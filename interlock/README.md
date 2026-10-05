# interlock

A workflow runtime that only lets work advance on evidence. Agents propose moves; `interlock` checks each one against recorded evidence, the current attempt, and granted authority before a task advances. It is one Rust binary with a SQLite store.

This directory builds the design draft "A workflow runtime that only lets work advance on evidence" (draft 1, October 4, 2026).

## Hosts

The core is host-agnostic. Hosts sit behind one adapter contract (`interlock-adapter`): each reports versioned capabilities, plans headless sessions, reads their output, and translates the core's host-neutral tool names into its own permission patterns.

- **GitHub Copilot CLI is a first-class host.** Its adapter is listed first and nothing in the core assumes another host. Its whole `interlock run` path is tested on the real CLI, offline against a scripted model.
- **Claude Code is the host we test with live.** It runs headless in our development containers, where Copilot CLI cannot sign in to GitHub yet. Being the test host gives it no special status in the core.

Both hosts load the same Claude-format hooks plugin with `--plugin-dir`, send it the same payloads, and accept the same responses. The details, and how each was verified, are in [docs/host-spike-2026-10-05.md](docs/host-spike-2026-10-05.md).

| Capability | Copilot CLI 1.0.91 | Claude Code 2.1.289 |
| --- | --- | --- |
| Headless session | `-p`, `--output-format json` (JSONL) | `--print`, prompt on stdin, `stream-json` |
| Cancel and timeout | kill the process group | kill the process group |
| Tool restriction | `--allow-tool`, `--deny-tool` | `--allowedTools`, `--disallowedTools`, `--permission-mode dontAsk` |
| Per-call policy | plugin `PreToolUse` hook | plugin `PreToolUse` hook |
| Stop guard | plugin `Stop` hook | plugin `Stop` hook |
| Model selection | `--model` | `--model` |
| Custom agents | `--agent` | `--agents` |

One policy, two hosts. `interlock host tools <task> --role verifier --host <host>` turns the bug-fix verifier's host-neutral policy into each host's patterns:

| | Allow | Deny |
| --- | --- | --- |
| Host-neutral | `read`, `shell` | `edit`, `shell:git push`, `shell:git commit` |
| Copilot CLI | `shell` (reading is never gated) | `write`, `shell(git push:*)`, `shell(git commit:*)` |
| Claude Code | `Read`, `Grep`, `Glob`, `Bash` | `Edit`, `Write`, `NotebookEdit`, `Bash(git push:*)`, `Bash(git commit:*)` |

## Running a task

`interlock run <task> --host <copilot|claude-code>` drives a task through headless sessions until it is done, blocked, failed, or waiting on the operator:

1. **G1**: records the repository's `HEAD` as the input snapshot, with the files the checks run marked as protected.
2. **Baseline**: interlock runs each check that promises a baseline on the input snapshot. A reproduction must fail there and a regression guard must pass there, and neither may run nothing. A task that breaks its promise is blocked before any session starts, with the reason.
3. **Worker**: opens an attempt (G2) in a fresh git worktree under `.interlock/worktrees/`, with a brief built from records and the attempt's effective grant as the host's tool filters. When the session ends, interlock writes the worktree's tree through a temporary index and submits it as the result (G3). The repository's branch and index are never touched.
4. **Verifier**: opens an independent attempt in a worktree checked out at that tree, with read and test tools only. It records one assessment per criterion.
5. **Advance**: G4 then G7 when the evidence holds; R1 with a fix brief when an independent check fails; R3 when a session times out or crashes.

Every tool call in those sessions passes through `interlock hook pre-tool-use`. That hook:
- denies edits outside the attempt's worktree or the task's scope;
- denies commands that touch the store;
- denies anything the effective grant does not cover (headless sessions have nobody to ask).

`interlock hook stop` holds a worker until interlock has run each self-checked criterion's check on its final files and the worker has claimed it, and holds a verifier until it has done the same for every criterion.

### Evidence interlock saw

An agent's word is not enough for a criterion that names a check. `interlock check run --criterion <id>` runs the check itself, on the exact tree in the agent's worktree, and keeps the output as a content-addressed artifact. Evidence policy v2 then requires, for a checked criterion:

| Rule | Why |
| --- | --- |
| A passing run on the current tree, asked for by an allowed producer (a verifier or the operator, for independent criteria) | The pass is something interlock saw |
| A failing run by a verifier fails the criterion outright | Same as a failed independent assessment |
| A run whose output shows nothing was tested never passes and never fails | `Ran 0 tests`, `collected 0 items`, `running 0 tests`, `[no test files]`, `No tests found`, `0 passing` |
| `baseline = "fails"`: the check failed on the input snapshot | A reproduction that already passes proves nothing |
| `baseline = "passes"`: the check passed there | A regression guard that already fails, or tests nothing, guards nothing |

A result whose changed files leave the task's scope, or touch a file a check runs, is rejected at G3: the change set is read from the output tree, so a shell write (`sed -i`, `>`) cannot get around it. The attempt can try again; under `interlock run`, the next attempt's brief says why the last result was rejected.

Recorded runs of the [export-retry example](examples/export-retry/README.md) on Claude Code: the corrected task went from create to done in two sessions, and the original task, whose regression command ran no tests, was blocked before any session started.

## What is built

| Area | State |
| --- | --- |
| S4: JSON Schemas for all records (`schemas/`), now ten with `check_run` | Done; Rust types are checked against them by a conformance test |
| Policy core: lifecycle, G1–G7, R1–R3, block, fail, cancel | Done; pure functions, no IO |
| Evidence policy v1: currency keys, independent failures win | Done; property-tested |
| Grants: profiles, intersection, expiry, irreversible never grantable | Done |
| Shell command classifier and task scope globs | Done; conservative first cut |
| Evidence policy v2: check runs by interlock, baselines, empty-run detection, output scope at G3 | Done |
| SQLite store: one transaction per move, append-only evidence, idempotent events | Done |
| CLI with JSON output | Done |
| Host adapters: inspection, tool translation, session planning, output parsing | Done for Copilot CLI and Claude Code |
| Session runner: transcripts, timeouts, cancellation | Done |
| Hooks plugin: per-call policy and stop guard, one plugin for both hosts | Done |
| Supervisor (`interlock run`): worktrees, worker and verifier sessions, rework, restart reconcile, single-controller lock | Done |
| Skill generator, forge adapter, evaluation | Later phases |

## Invariant tests

Each invariant in the design has a fault test that passes today. The last row is an invariant this build adds.

| Invariant | Test |
| --- | --- |
| 1. No current evidence, no pass | A verifier pass on another tree is kept but does not count; the task stays awaiting |
| 2. Workers cannot override verifiers or grant themselves authority | Verifier fails, worker then claims pass: R1, never verified. Workers cannot record assessments. Evidence tables reject UPDATE and DELETE. Irreversible grants are refused. On Copilot, a worker's `interlock grant create` is denied by the hook and no grant exists afterwards |
| 3. Changes invalidate evidence | A rebase after verification sends the task back through R2, and landing is refused. The stop guard rejects a claim made before the worker's last edit |
| 4. Late workers cannot advance tasks | Retry, respawn, then the old result arrives: stored as superseded |
| 5. Apply and acknowledge together | A duplicate event is a no-op; the process aborted mid-write, then the replay applies exactly once |
| 6. Permissions are bounded | On Copilot, a worker's `git push` is denied inside the live session; edits outside the worktree or scope, and commands touching the store, are denied |
| 7. External effects are reconciled | Operation rows are written before G5, and G6 confirms the pinned head. On restart, attempts left running get a synthetic failure report and the task retries |
| Added: checks cannot be faked or emptied | On Copilot, end to end: a regression check that runs no tests, and a reproduction that already passes, each block the task before any session; a worker that rewrites the check to `exit 0` has its result rejected, and the next worker must really fix the bug. Runs that tested nothing never pass or fail (property-tested) |

## Changes from the design draft

- **R3, retry:** running back to ready when the current attempt is cancelled or times out. The draft's "cancel, respawn" fault test needs a path from running to a fresh attempt; R3 is that path, and it fails the task when the budget is spent.
- **One strength field on evidence.** The draft lists both strength and outcome on claims and assessments. Its own strength scale already includes `blocked` and `failed`, so a separate outcome would only allow contradictions.
- **Hand-written record types.** `typify` is still alpha (0.10.0-alpha.1). The schemas stay the source of truth: `interlock-schema` embeds them, the store validates every record it writes, and a conformance test catches drift. Generated types can replace these once the schemas settle.
- **A host-neutral tool vocabulary** (`read`, `edit`, `shell:<prefix>`, `web`, `mcp:<server>/<tool>`, `agent`) so grants are written once and translated per host.
- **One hooks plugin instead of per-host hook packages.** Both hosts load the same Claude-format plugin, so per-call policy and the stop guard are one implementation.
- **Check runs are a tenth record.** The draft's evidence was claims and assessments. A `check_run` records interlock's own execution of a criterion's check, and criteria gained a `baseline`. The draft's "same-surface reproduction before and after the change" becomes a rule rather than an instruction.
- **Scope is enforced on the output tree.** The draft limits edits to the task's scope; the hook enforces that per call for edit tools, and G3 enforces it for everything, including the checks' own files.

## Next

- **Interactive path.** Generated skills that call `interlock` from a session a person drives (phase 1), using the same hooks in `interactive` mode, where they ask instead of deny.
- **Forge adapter** for G5 and G6 with pinned merges, and the evaluation against a frozen task set (phase 4).
- **Copilot CLI with a real model**, in an environment where it can sign in. Today's Copilot runs exercise the real CLI, hooks and permissions, with a scripted model.
- **Wider empty-run detection.** The detectors know unittest, pytest, cargo, go, Jest, Vitest and Mocha. Other runners need adding, or a criterion could state the minimum number of tests it expects.

## Use

```bash
cargo build --release
export PATH="$PWD/target/release:$PATH"

interlock init
interlock host inspect --save
interlock task create task.toml
interlock run export-retry --host copilot
interlock status export-retry
interlock task log export-retry
```

The same moves can be made by hand, as an interactive session would:

```bash
interlock task ready export-retry --base "$(git rev-parse HEAD)"
interlock attempt start export-retry --role worker --host copilot
interlock result submit --attempt <id> --token <token> --epoch 1 --tree "$(git write-tree)" --summary "..."
interlock attempt start export-retry --role verifier --host copilot
interlock check run --criterion repro            # interlock runs the check; credentials come from the attempt
interlock assess add --attempt <id> --token <token> --criterion repro --strength observed --tree <tree>
interlock advance export-retry
interlock brief export-retry
```

`--tree auto` records the current worktree's tree. Every command prints JSON. Refusals exit 2 with the reason on stderr; unknown records exit 3; bad attempt tokens exit 4. `interlock run` exits 0 when the task is done and 5 otherwise.

## Layout

| Crate | Job |
| --- | --- |
| `interlock-schema` | Record types; embeds and validates against `schemas/` |
| `interlock-core` | Lifecycle, guards, evidence policy, grants, classifier, scope, briefs. Pure |
| `interlock-store` | SQLite: migrations, transactions, append-only triggers, events, restart reconcile |
| `interlock-adapter` | Capability contract; Copilot CLI and Claude Code adapters; session runner; hook protocol and plugin |
| `interlock-supervisor` | `interlock run`: git worktrees, sessions, hooks, prompts |
| `interlock-cli` | The `interlock` binary |

Attempt tokens are bookkeeping, not security: an agent with filesystem access could still edit the store. The hooks narrow what agents can do, and the shell classifier does not parse shell grammar. Real containment comes from the host's tool restrictions and the operating system.

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all --check
```

The Copilot end-to-end tests (`crates/interlock-cli/tests/run_copilot.rs`) run whole `interlock run` passes on the real Copilot CLI in offline mode against a scripted model, so they need no account and make no model calls. They skip unless a Copilot binary is available:

```bash
npm install @github/copilot
INTERLOCK_COPILOT_BIN="$PWD/node_modules/.bin/copilot" cargo test -p interlock-cli --test run_copilot
```

Set `INTERLOCK_COPILOT_BIN` or `INTERLOCK_CLAUDE_BIN` to point interlock at a specific binary.
