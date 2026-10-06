# Implement

Make a change: fix a bug, add or change a feature, or restructure code. Fix the cause wherever it occurs, change nothing the request does not need, and check the result on the real surface before you say it is done.

**With interlock or without.** Work through interlock when this work is an interlock task: you were given a task id, `INTERLOCK_ATTEMPT` is set, or `interlock where` exits 0. That command prints the store every interlock command would use from here, found the same way they find it (`INTERLOCK_DB` when set, then an attempt's worktree, then the repository root), and creates nothing; it exits 3 when there is no store, and the shell says the command is not found where interlock is not installed. Do not look for the store's file yourself: it need not be at `.interlock/state.db`. Where there is a store but no task for this work, invoke {{skill:route}} first. Otherwise there is no task to report to: follow "Without interlock" at the end.

With interlock, you are the worker on the task, in the worktree interlock opens for you, inside the task's scope. You cannot mark anything done: interlock moves the task when the evidence holds. Read `references/interlock-basics.md` once per session.

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

If `INTERLOCK_ATTEMPT` is set, interlock opened the attempt for you, you are in its worktree, and it runs the rest of the loop itself: skip this step and steps 5 and 6, leave out `--attempt` and `--token` (interlock reads them from the environment), and report as your instructions say.

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

## Without interlock

There is no task, attempt or store, so nothing is recorded; the playbooks still apply, in the working copy you were given.

1. **Say what done means** before you change anything: what must work afterwards, and what must keep working. Take it from the request, and stay inside any limits it sets on what may change.
2. **Follow the workflow's playbook** from step 3: `references/bug-fix.md`, `references/feature.md` or `references/refactor.md`. Run each check yourself and read its output: the reproduction before the change and after it, and the project's tests after your last edit. Skip the steps that claim, submit or hand off.
3. **Check it on the real surface** after your last edit (`references/prove-it-works.md`). A test you did not run is not evidence.
4. **Report** what changed, the root cause, and the commands you ran with what they printed. Say what you did not check, and any part of the request you left undone and why.
