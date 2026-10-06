# Verify

An independent verifier checks the result on exactly the files that were submitted, with read and test tools only, and records its own evidence. interlock then decides. The agent that did the work never verifies it: a second pass by the same agent is not independent, and interlock does not count a worker's evidence for independent criteria. Read `references/interlock-basics.md` once per session.

## 1. Check the task is ready for it

```bash
interlock status <id>
```

The state must be `awaiting_verification`, with `current_tree` set. If it is `ready` or `running`, the work is not submitted yet; go back to {{skill:implement}}.

## 2. Hand off to the verifier

To hand off, {{handoff:verifier}}. Give it this prompt, filled in:

> Verify interlock task `<id>` in the repository at `<absolute path of the repository root>`. Host: `{{host}}`. Open your own verifier attempt, have interlock run every check on the submitted files, record one assessment per criterion, end your attempt, and reply with your attempt id and one line per criterion. If a review of this task listed findings to act on, check each one: `<findings, or "none">`.

The verifier opens its own attempt, so its token never passes through you. Wait for its reply. Do not record assessments yourself, and do not edit files while it works: the hooks hold you to the verifier's read-only grant until its attempt ends.

If your host cannot start an isolated custom agent, stop here. The task stays `awaiting_verification` with its work kept; tell the person it needs an independent verifier.

## 3. Let interlock decide

```bash
interlock advance <id>
interlock status <id>
```

| State after `advance` | Meaning | Next |
| --- | --- | --- |
| `done` | Every criterion has current passing evidence from allowed producers (G4), and the workflow needs no delivery (G7) | Report to the person |
| `verified` | Verified; delivery needs landing authority | Tell the person; landing is theirs |
| `ready` | An independent check failed (R1); the brief now starts with the failure | Back to {{skill:implement}} with the task id |
| `failed` | It failed and the attempt budget is spent | Report what failed |
| `awaiting_verification` | Evidence is still missing; `next_moves[0].needs` says what | Hand off once more with that list. If it is still missing, leave the task waiting and tell the person why |

## 4. Report

```bash
interlock task log <id>
```

Tell the person the final state, each criterion's verdict and evidence strength from `status`, and the transition log. Quote failing output verbatim. Same surface before and after is the standard (`references/prove-it-works.md`): if a reproduction was only shown on a different surface, say so. Tests that only restate the code prove little (`references/test-behavior-not-implementation.md`).
