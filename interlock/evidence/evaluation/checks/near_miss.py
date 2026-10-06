"""Judges plausible partial fixes, to show the hidden checks discriminate."""
import os
import shutil
import sys
import tempfile

sys.path.insert(0, "/home/user/autonomous-claude-workflow-bundle/.claude/worktrees/agent-a3de08fe5c01e8fbb/interlock/eval")
import harness  # noqa: E402

WORK = "/tmp/claude-0/-home-user-autonomous-claude-workflow-bundle/533a7202-b2b1-5f9c-8fb9-d9bcdfeb7c84/scratchpad/work"

NEAR_MISSES = {
    "py-split-remainder": ("tally/money.py", "    return [total // n] * n\n",
                           "    q, r = divmod(total, n)\n    return [q + 1 if i < r else q for i in range(n)]\n"),
    "py-parse-amount": ("tally/money.py", "        return int(whole) * 100 + int(frac)\n",
                        "        return int(whole) * 100 + int(frac.ljust(2, '0'))\n"),
    "go-quoted-hash": ("parser.go", '\tif i := strings.Index(raw, "#"); i >= 0 {\n\t\traw = strings.TrimSpace(raw[:i])\n\t}\n',
                       '\tif !strings.HasPrefix(raw, `"`) {\n\t\tif i := strings.Index(raw, "#"); i >= 0 {\n\t\t\traw = strings.TrimSpace(raw[:i])\n\t\t}\n\t} else if j := strings.LastIndex(raw, `"`); j > 0 {\n\t\traw = raw[:j+1]\n\t}\n'),
}

for task in harness.load_tasks(list(NEAR_MISSES)):
    frozen = harness.Frozen(WORK, task)
    path, old, new = NEAR_MISSES[task.id]
    tmp = tempfile.mkdtemp()
    repo = os.path.join(tmp, "r")
    shutil.copytree(frozen.repo, repo)
    with open(os.path.join(repo, path)) as f:
        text = f.read()
    assert text.count(old) == 1, (task.id, "pattern")
    with open(os.path.join(repo, path), "w") as f:
        f.write(text.replace(old, new))
    cases = harness.judge_dir(task, frozen, repo, [])
    visible = {c["id"]: harness._check_runs_as_promised(repo, c)[0] for c in task.spec["criterion"] if c.get("check")}
    print(task.id, "visible check exit codes:", visible)
    print("   hidden failing:", [c["id"] for c in cases if not c["pass"]])
    shutil.rmtree(tmp)
