# Investigate

Build a working model of the code from evidence you can point at: files and lines, commands you ran and what they printed, and the commits that shaped it. Investigations are read-only.

Use this skill three ways:

- **As the worker of an interlock investigation task**: you were given a task id, `INTERLOCK_ATTEMPT` is set, or the repository has an interlock store (`.interlock/state.db`; with no task for this question yet, invoke {{skill:route}} first). Read `references/interlock-basics.md` once per session and follow every step below. The answer is your result.
- **To ground a change** inside {{skill:implement}}. Run steps 3 to 5 where you already work, then go back.
- **On its own**, for any other question about code: follow "Without interlock" at the end.

## 1. Read the task

```bash
interlock brief <id>
```

The goal is the question. The criteria say what the answer must show.

## 2. Open the attempt

```bash
interlock attempt start <id> --role worker --host {{host}} --worktree auto
```

Keep `attempt.id`, `attempt.epoch`, `attempt.worktree` and `token`. The worktree holds the repository at the input snapshot. Read there. An investigation changes nothing: the hooks deny this attempt the edit tools, and interlock rejects its result if the worktree's files differ from the snapshot in any way, however they were changed. Put scratch files under /tmp.

If `INTERLOCK_ATTEMPT` is set, interlock opened the attempt for you and you are in its worktree: skip this step, leave out `--attempt` and `--token` (interlock reads them from the environment), and report as your instructions say.

## 3. How it works

Decide whether the question is narrow (one function, one module) or spans a subsystem. For a narrow one, trace it yourself in one pass. For a wide one, split it into two to four angles (entry points, data flow, state, error paths) and trace each.

- Start from the entry point the question names and follow the calls. Read the code; do not infer behavior from names.
- When the code's behavior is not obvious, run it: a test, a one-line script, the CLI. Something you ran and saw beats something you read.
- Record every claim with its evidence as you go: `path:line` for code, the exact command and its output for behavior.

## 4. Why it is this way

For motivation, regressions or "are we sure" questions, read the history of the lines that matter:

```bash
git log --oneline -15 -- <file>
git blame -L <start>,<end> <file>
git log -1 --format=%B <commit>
```

Recency bias is the common failure: the last commit is rarely the whole story. Trace back until the reason is stated or the trail ends, and say which. Grade every "why" claim with `references/confidence-tiers.md`; never present an inference as a fact.

## 5. Write the answer

Use `references/answer-format.md`. If the question was a decision between alternatives, give a recommendation with a table of tradeoffs, and your real judgment. Push back if the premise is wrong. If the root cause of a defect is the question, `references/fix-root-causes.md` says how far to dig.

## 6. Report through interlock

Run interlock's checks if any criterion has one, then record one claim per criterion and submit:

```bash
cd <worktree> && interlock check run --criterion <c> --attempt <attempt.id> --token <token>    # criteria with a check
cd <worktree> && interlock claim add --criterion <c> --strength observed --tree auto \
  --ref "<path:line or command>" --note "<the claim and what showed it>" --attempt <attempt.id> --token <token>
cd <worktree> && interlock result submit --epoch <attempt.epoch> --tree auto \
  --summary "<the answer, with its citations>" --attempt <attempt.id> --token <token>
```

Use `observed` only for what you ran and saw; `static` for what you only read. The summary is the record of your answer: keep the citations in it. Keep the full answer on the task as well, so it outlives this session:

```bash
interlock note add --task <id> --kind answer --file - <<'EOF'
<the answer, in the answer format>
EOF
```

The task now awaits verification. Invoke {{skill:verify}} with the task id: an independent verifier rechecks your citations and reruns your commands. Give the person the answer when the task is verified.

## Without interlock

There is no task, attempt or store, so nothing is recorded. Change no file; put scratch files under /tmp.

1. **Take the question** from the request: what the answer must show, and any form it must take.
2. **Follow steps 3 to 5** in the working copy you were given.
3. **Give the answer** in your reply, in `references/answer-format.md`, with its citations and the commands you ran. Say what you could not confirm.
