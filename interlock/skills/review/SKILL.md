# Review

Challenge a submitted result before it is verified. The deliverable is a judged list of findings, each with its evidence. Review informs; it does not decide. Findings that break a criterion go to the verifier, whose evidence interlock counts. Read `references/interlock-basics.md` once per session.

## 1. Open a reviewer attempt

```bash
interlock status <id>          # must be awaiting_verification
interlock attempt start <id> --role reviewer --host {{host}} --worktree auto
```

Keep `attempt.id`, `attempt.worktree` and `token`. The worktree holds exactly the submitted output, committed on top of the input snapshot, so the change is:

```bash
cd <worktree> && git show --stat HEAD && git show HEAD
```

The reviewer grant is read-only. Do not edit anything here.

## 2. State the intent

Write one paragraph: what the change is for, from `interlock brief <id>`, the result summary, and the code. If you cannot tell, ask the person before reviewing.

## 3. Find problems

Read the whole change against the intent and the criteria. Look for:

- correctness: wrong results, missed cases, broken invariants, races;
- the criteria themselves: a check that would pass without the fix, a test that restates the code;
- scope: changes the task did not ask for;
- reader load: layers or state the change adds without need.

For each finding, gather evidence: the `path:line`, and wherever possible a command that shows the problem. A finding you cannot support is a hunch; label it so. Grade claims about intent or history with `references/confidence-tiers.md`.

If your host can run other agents or models, a second reviewer given the same prompt adds signal: findings two reviewers raise on their own are the strongest.

## 4. Judge each finding

Use `references/lead-judgment.md`: you are the lead reviewer, not a neutral aggregator. Every finding gets one bucket (act on, consider, noted, dismissed) and a one-line reason. Keep the dismissed ones in the list with their reasons.

## 5. Close the attempt and hand over

```bash
interlock attempt end <attempt.id> --token <token> --note "review: <n> act on, <n> consider, <n> noted, <n> dismissed"
```

Then write the full judged list to `.interlock/reviews/<id>.md` in the repository root, and show it to the person. Pass the act-on findings to {{skill:verify}}, which hands them to the verifier to check; a finding it reproduces fails its criterion, and interlock sends the task back for rework.
