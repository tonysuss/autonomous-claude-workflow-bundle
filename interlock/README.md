# interlock

A workflow runtime that only lets work advance on evidence. Agents propose moves; `interlock` checks each one against recorded evidence, the current attempt, and granted authority before a task advances. It is one Rust binary with a SQLite store.

This directory is the first build slice of the design draft "A workflow runtime that only lets work advance on evidence" (draft 1, October 4, 2026).

## Hosts

The core is host-agnostic. Hosts sit behind one adapter contract (`interlock-adapter`): each reports versioned capabilities, and translates the core's host-neutral tool names into its own permission patterns.

- **GitHub Copilot CLI is a first-class host.** Its adapter is listed first and nothing in the core assumes another host.
- **Claude Code is the host we test with.** It runs headless in our development containers, where Copilot CLI cannot authenticate yet. Being the test host gives it no special status in the core.

`interlock host inspect` on October 5, 2026:

| Capability | Copilot CLI 1.0.91 | Claude Code 2.1.289 |
| --- | --- | --- |
| Headless session start / collect | `-p`, `--output-format json` | `--print`, `--output-format` |
| Event stream | JSONL | `stream-json` |
| Cancel | process signal | process signal |
| Tool restriction | `--available-tools`, `--allow-tool`, `--deny-tool` | `--allowedTools`, `--disallowedTools` |
| Per-call policy | repo hooks (`preToolUse`) | `PreToolUse` hooks via `--settings` |
| Stop guard | not confirmed | `Stop` / `SubagentStop` hooks |
| Model selection | `--model` | `--model` |
| Custom agents | `--agent` | `--agents` |

One policy, two hosts. `interlock host tools <task> --role verifier --host <host>` turns the bug-fix verifier's host-neutral policy into each host's patterns:

| | Allow | Deny |
| --- | --- | --- |
| Host-neutral | `read`, `shell` | `edit`, `shell:git push`, `shell:git commit` |
| Copilot CLI | `shell` (reading is never gated) | `write`, `shell(git push:*)`, `shell(git commit:*)` |
| Claude Code | `Read`, `Grep`, `Glob`, `Bash` | `Edit`, `Write`, `NotebookEdit`, `Bash(git push:*)`, `Bash(git commit:*)` |

## What is built

| Area | State |
| --- | --- |
| S4: JSON Schemas for all nine records (`schemas/`) | Done; Rust types are checked against them by a conformance test |
| Policy core: lifecycle, G1–G7, R1–R3, block, fail, cancel | Done; pure functions, no IO |
| Evidence policy v1 with currency keys | Done; property-tested |
| Grants: profiles, intersection, expiry, irreversible never grantable | Done |
| Shell command classifier for per-call policy | Done; conservative first cut |
| SQLite store: one transaction per move, append-only evidence, idempotent events | Done |
| CLI with JSON output | Done |
| Host inspection and tool translation for Copilot and Claude Code | Done |
| Headless session start, collect and cancel through the adapters | Next |
| Supervisor: leases, budgets, timeouts, restart reconcile | Next |
| Hook packages for each host (per-call policy, stop guard) | Next |
| Skill generator, forge adapter, evaluation | Later phases |

## Invariant tests

Each invariant in the design has a fault test that passes today, run against a real store.

| Invariant | Test |
| --- | --- |
| 1. No current evidence, no pass | A verifier pass on another tree is kept but does not count; the task stays awaiting |
| 2. Workers cannot override verifiers or grant themselves authority | Verifier fails, worker then claims pass: R1, never verified. Workers cannot record assessments. Evidence tables reject UPDATE and DELETE. Irreversible grants are refused. `interlock grant` classifies as operator-only |
| 3. Changes invalidate evidence | A rebase after verification sends the task back through R2, and landing is refused |
| 4. Late workers cannot advance tasks | Retry, respawn, then the old result arrives: stored as superseded |
| 5. Apply and acknowledge together | A duplicate event is a no-op; the process aborted mid-write, then the replay applies exactly once |
| 6. Permissions are bounded | Unit-tested in the core; enforcement inside a live session comes with the hook packages |
| 7. External effects are reconciled | Operation rows are written before G5 and G6 confirms the pinned head; restart reconcile comes with the supervisor |

## Changes from the design draft

- **R3, retry:** running back to ready when the current attempt is cancelled or times out. The draft's "cancel, respawn" fault test needs a path from running to a fresh attempt; R3 is that path, and it fails the task when the budget is spent.
- **One strength field on evidence.** The draft lists both strength and outcome on claims and assessments. Its own strength scale already includes `blocked` and `failed`, so a separate outcome would only allow contradictions.
- **Hand-written record types.** `typify` is still alpha (0.10.0-alpha.1). The schemas stay the source of truth: `interlock-schema` embeds them, the store validates every record it writes, and a conformance test catches drift. Generated types can replace these once the schemas settle.
- **A host-neutral tool vocabulary** (`read`, `edit`, `shell:<prefix>`, `web`, `mcp:<server>/<tool>`, `agent`) so grants are written once and translated per host.

## Use

```bash
cargo build --release
export PATH="$PWD/target/release:$PATH"

interlock init
interlock host inspect --save
interlock task create task.toml
interlock task ready export-retry --base "$(git rev-parse HEAD)"
interlock attempt start export-retry --role worker --host copilot
interlock result submit --attempt <id> --token <token> --epoch 1 --tree "$(git write-tree)" --summary "..."
interlock attempt start export-retry --role verifier --host copilot
interlock assess add --attempt <id> --token <token> --criterion repro --strength observed --tree <tree>
interlock advance export-retry
interlock status export-retry
interlock brief export-retry
```

A task file:

```toml
id = "export-retry"
repository = "."
workflow = "bug-fix"
intent = "Fix duplicate rows when an export retries"

[[criterion]]
id = "repro"
statement = "Retrying an export produces no duplicate rows"
check = "checks/export-retry.sh"
min_strength = "observed"
producer = "independent"
```

Every command prints JSON. Refusals exit 2 with the reason on stderr; unknown records exit 3; bad attempt tokens exit 4.

## Layout

| Crate | Job |
| --- | --- |
| `interlock-schema` | Record types; embeds and validates against `schemas/` |
| `interlock-core` | Lifecycle, guards, evidence policy, grants, classifier, briefs. Pure |
| `interlock-store` | SQLite: migrations, transactions, append-only triggers, events |
| `interlock-adapter` | Capability contract, Copilot CLI and Claude Code adapters |
| `interlock-cli` | The `interlock` binary |

Attempt tokens are bookkeeping, not security: an agent with filesystem access could still edit the store. Real containment comes from the host's tool restrictions and the operating system.

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all --check
```

Set `INTERLOCK_COPILOT_BIN` or `INTERLOCK_CLAUDE_BIN` to point inspection at a specific binary.
