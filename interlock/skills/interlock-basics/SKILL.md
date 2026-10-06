# Working through interlock

interlock decides when work advances. You propose; it checks each move against recorded evidence, the current attempt, and the authority granted. Only interlock writes task state. Every command prints JSON. A refusal exits 2 and says why on stderr; a missing record exits 3; a bad token exits 4.

## The loop

| Step | Command | Who |
| --- | --- | --- |
| Create the task | `interlock task create .interlock/tasks/<id>.toml` | route |
| Record the input snapshot (G1) | `interlock task ready <id> --base "$(git rev-parse HEAD)"` | route |
| Open a worker attempt (G2) | `interlock attempt start <id> --role worker --host {{host}} --worktree auto` | implement, investigate |
| Have interlock run a check | `interlock check run --criterion <c> [--target base]` | worker, verifier |
| Record a worker claim | `interlock claim add --criterion <c> --strength <s> --tree auto --ref "<command>" --note "<what you saw>"` | worker |
| Submit the result (G3) | `interlock result submit --epoch <n> --tree auto --summary "<what changed and how you checked>"` | worker |
| Verify independently | the verifier agent opens its own attempt and records assessments | verify |
| Apply what the evidence allows (G4, G7, R1, R2) | `interlock advance <id>` | verify |
| Where things stand | `interlock status <id>` and `interlock task log <id>` | anyone |
| Resume from records | `interlock brief <id>` | anyone |

`--host` names the host you run in (`copilot` or `claude-code`); interlock reads that host's capabilities from it.

## Attempts and tokens

`interlock attempt start` prints `attempt.id`, `attempt.epoch`, `attempt.worktree` and a `token` that is shown once. Keep all four. Pass the id and token to every command that reports evidence:

```
interlock claim add --attempt <attempt.id> --token <token> ...
```

The flags `--attempt` and `--token` work on `check run`, `claim add`, `assess add` and `result submit`. `interlock attempt end <attempt.id> --token <token>` closes an attempt.

With `--worktree auto`, interlock makes a fresh git worktree for the attempt under `.interlock/worktrees/` and prints its path. Work there. Run interlock's evidence commands from inside it: `check run` and `--tree auto` read the files in the directory you run them from. Prefix commands with `cd <worktree> &&` so a shell that forgets its directory cannot point them elsewhere.

## Evidence strength

| Strength | Means |
| --- | --- |
| `observed` | You saw the behavior on the real surface: the CLI, the API, the UI |
| `tested` | A targeted test exercises the changed path and passes |
| `static` | Only type checks or the build pass |
| `failed` | The check ran and the criterion does not hold |
| `blocked` | The check could not run. Never a pass |

A criterion that names a check needs a passing run that interlock made itself (`interlock check run`); your word is not enough. A run whose output shows nothing was tested never counts. A claim or run made before your last edit no longer counts, so record evidence last.

## Rules

- Report only through `interlock`. Never read or write its store, never pass `--operator`, never run `interlock grant`.
- Do not commit, push, or create branches in an attempt's worktree. interlock records the worktree's files itself.
- If a hook denies or asks about a tool call, the reason says why. Do not work around it; tell the person.
- Read `interlock status <id>` after each move. Its `next_moves` list what is still needed.
