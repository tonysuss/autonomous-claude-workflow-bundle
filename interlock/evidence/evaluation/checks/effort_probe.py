"""Asks Claude Code which effort level is active, with and without the pin,
using the same environment the harness gives every condition."""
import json
import os
import subprocess
import sys

sys.path.insert(0, "/home/user/autonomous-claude-workflow-bundle/.claude/worktrees/agent-a3de08fe5c01e8fbb/interlock/eval")
import harness  # noqa: E402

prompt = "Run this exact Bash command and reply with only its output: echo \"effort=$CLAUDE_EFFORT\""
out = {}
for effort in (None, "medium"):
    env = {k: v for k, v in os.environ.items() if k not in harness.SESSION_VARS}
    if effort:
        env["CLAUDE_CODE_EFFORT_LEVEL"] = effort
    cmd = ["claude", "-p", "--output-format", "json", "--permission-mode", "dontAsk", "--setting-sources", "",
           "--no-session-persistence", "--model", "claude-sonnet-5-5", "--max-turns", "3", "--allowedTools", "Bash"]
    p = subprocess.run(cmd, input=prompt, capture_output=True, text=True, env=env, cwd="/tmp", timeout=180)
    r = json.loads(p.stdout)
    out[str(effort)] = {"result": r.get("result"), "cost": r.get("total_cost_usd")}
print(json.dumps(out, indent=1))
