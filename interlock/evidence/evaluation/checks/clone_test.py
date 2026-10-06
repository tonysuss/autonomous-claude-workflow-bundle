"""Checks that a task built from an upstream repository cannot reach the fix."""
import os
import subprocess
import sys
import tempfile

sys.path.insert(0, "/home/user/autonomous-claude-workflow-bundle/.claude/worktrees/agent-a3de08fe5c01e8fbb/interlock/eval")
import harness  # noqa: E402

tmp = tempfile.mkdtemp()
up = os.path.join(tmp, "upstream")
os.makedirs(up)


def g(*a, cwd=up):
    return subprocess.run(["git", *a], cwd=cwd, capture_output=True, text=True, check=True,
                          env=harness._commit_env(0)).stdout.strip()


g("init", "-q", "-b", "main")
open(os.path.join(up, "f.txt"), "w").write("bug\n")
g("add", "-A"); g("commit", "-q", "-m", "before the fix")
start = g("rev-parse", "HEAD")
open(os.path.join(up, "f.txt"), "w").write("fixed\n")
g("add", "-A"); g("commit", "-q", "-m", "THE FIX")
fix = g("rev-parse", "HEAD")
g("tag", "v1")

dest = os.path.join(tmp, "frozen")
commits = harness._clone_at({"git": up, "commit": start}, dest)
print("frozen HEAD is the start commit:", commits[0]["sha"] == start)
print("working file:", open(os.path.join(dest, "f.txt")).read().strip())
print("refs:", g("for-each-ref", "--format=%(refname)", cwd=dest).split())
print("remotes:", g("remote", cwd=dest).split())
p = subprocess.run(["git", "cat-file", "-e", fix], cwd=dest, capture_output=True)
print("fix object present:", p.returncode == 0)
print("log --all mentions the fix:", "THE FIX" in g("log", "--all", "--format=%s", cwd=dest))
