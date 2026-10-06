# Implement

You are the worker on an interlock task. Make the smallest change the evidence justifies, inside the task's scope, in the worktree interlock opens for you. You cannot mark anything done: interlock moves the task when the evidence holds. Read `references/interlock-basics.md` once per session.

## 1. Read the brief

```bash
interlock brief <id>
interlock status <id>
```

The brief gives the goal, the scope, the files you may not change, each criterion with its check, and, on rework, the failures to fix first. The state must be `ready`. If it is `awaiting_verification`, invoke {{skill:verify}} instead.

## 2. Open the attempt

```bash
interlock attempt start <id> --role worker --host {{host}} --worktree auto
```

Keep `attempt.id`, `attempt.epoch`, `attempt.worktree` and `token`. Every edit goes under `attempt.worktree`, by its absolute path. The hooks deny edits anywhere else, and edits outside the task's scope. Do not commit, push or branch.

## 3. Follow the workflow's playbook

| Workflow | Playbook |
| --- | --- |
| `bug-fix` | `references/bug-fix.md` |
| `feature` | `references/feature.md` |
| `refactor` | `references/refactor.md` |

Each one starts by having interlock run the task's checks on the input snapshot, so the before is on record:

```bash
cd <worktree> && interlock check run --criterion <c> --target base --attempt <attempt.id> --token <token>
```

Read the output, not just `passed`. A reproduction must fail there for the reported reason; a regression guard must pass and must have run tests (`checked_nothing` is null). If either promise breaks, stop and tell the person: the task's checks prove nothing, and the task needs new criteria.

When a change crosses a function boundary and more than one shape is plausible, the person can run {{skill:design}} before you write code. Ground any change you do not yet understand with {{skill:investigate}} (its steps 3 to 5, in this attempt).

Principles that apply here: `references/fix-root-causes.md`, `references/subtract-before-you-add.md`, `references/test-behavior-not-implementation.md`, `references/prove-it-works.md`.

## 4. Record the after

After your last edit, have interlock run every criterion's check on your files, then record one claim per criterion:

```bash
cd <worktree> && interlock check run --criterion <c> --attempt <attempt.id> --token <token>
cd <worktree> && interlock claim add --criterion <c> --strength tested --tree auto \
  --ref "<the command>" --note "<what you saw>" --attempt <attempt.id> --token <token>
```

Claim what you saw: `observed` for behavior you watched on the real surface, `tested` for a passing targeted test, `failed` if it still does not hold. Any edit after this makes these records stale; run and claim again.

## 5. Submit the result

```bash
cd <worktree> && interlock result submit --epoch <attempt.epoch> --tree auto \
  --summary "<what changed, why, and how you checked it>" --attempt <attempt.id> --token <token>
```

Read `outcome.result.status`. `accepted` moves the task to `awaiting_verification`. `rejected` names the files outside the scope or the protected checks you changed: undo those and submit again. `superseded` means another attempt took over; stop.

## 6. Hand off

Invoke {{skill:verify}} with the task id. Do not verify your own work: criteria marked independent count only a separate verifier's evidence.

If verification sends the task back (`ready` again, rule R1), `interlock brief <id>` starts with what failed. Open a new attempt and fix that first; the new worktree continues from your last accepted output.
