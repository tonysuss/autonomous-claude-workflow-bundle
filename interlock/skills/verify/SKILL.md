# Verify

An independent verifier checks the result on exactly the files that were submitted, with read and test tools only, and records its own evidence. interlock then decides. The agent that did the work never verifies it, and interlock enforces that: the hooks refuse a verifier attempt from the session that holds the work, a verifier's credentials work only for the verifier they are bound to, and where a criterion has no check, an assessment counts only from a verifier interlock could bind. Read `references/interlock-basics.md` once per session.

## 1. Check the task is ready for it

```bash
interlock status <id>
```

The state must be `awaiting_verification`, with `current_tree` set. If it is `ready` or `running`, the work is not submitted yet; go back to {{skill:implement}}.

## 2. Hand off to the verifier

{{handoff:verifier}}

Wait for it to finish. Do not record assessments yourself, and do not try to open a verifier attempt: the hooks refuse it from the session that did the work.

## 3. Let interlock decide

```bash
interlock advance <id>
interlock status <id>
```

| State after `advance` | Meaning | Next |
| --- | --- | --- |
| `done` | Every criterion has current passing evidence from allowed producers (G4), and the workflow needs no delivery (G7). interlock closed the task's attempts, removed their worktrees, and kept the verified change at `settled.output_ref` | Report to the person |
| `verified` | Verified; delivery needs landing authority | Tell the person; landing is theirs |
| `ready` | An independent check failed (R1); the brief now starts with the failure | Back to {{skill:implement}} with the task id |
| `blocked` | interlock stopped the task: `blocked_reason` says why, for example no independent verifier is available on this host | Tell the person the reason; the work is kept, and `interlock task unblock` (the person's call) resumes it |
| `failed` | It failed and the attempt budget is spent | Report what failed |
| `awaiting_verification` | Evidence is still missing; `next_moves[0].needs` says what. Each attempt in `attempts` shows how it was bound: an `unbound` verifier's assessments count only where interlock ran the check | Hand off once more with that list. If it is still missing, leave the task waiting and tell the person why |

## 4. Report

```bash
interlock task log <id>
```

Tell the person the final state, each criterion's verdict and evidence strength from `status`, and the transition log. For a change, tell them where it is: `advance` printed `settled.output_ref`, a commit on the input snapshot, and `settled.apply_with` (`git cherry-pick <ref>`) applies it to their branch. Applying it is their call; do not run it yourself. Quote failing output verbatim. Same surface before and after is the standard (`references/prove-it-works.md`): if a reproduction was only shown on a different surface, say so. Tests that only restate the code prove little (`references/test-behavior-not-implementation.md`).
