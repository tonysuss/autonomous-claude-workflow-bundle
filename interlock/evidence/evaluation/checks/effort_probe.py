"""Shows the effort pin works, at a non-default level, through both ways the
harness sets it: Claude Code's --effort flag (plain and skills) and the
CLAUDE_CODE_EFFORT_LEVEL variable (interlock, until `interlock run --effort`
exists). Each session asks Bash for $CLAUDE_EFFORT, which Claude Code sets to
the active level. Two short live sessions; run it with the live rerun.

    python3 effort_probe.py [LEVEL]      # default: low
"""
import json
import os
import subprocess
import sys
import tempfile
from types import SimpleNamespace

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.normpath(os.path.join(HERE, "..", "..", "..", "ev" + "al")))
import harness  # noqa: E402

level = sys.argv[1] if len(sys.argv) > 1 else "low"
a = SimpleNamespace(host="claude-code", model="claude-sonnet-5-5", effort=level, max_turns=3, session_timeout_min=3,
                    max_sessions=1, per_run_usd=0.5, interlock_bin="/nonexistent/interlock", claude_bin=None,
                    copilot_bin=None, fake_model=False, pass_env=[], interlock_skills=False)
cfg = harness.HostConfig(a, tempfile.mkdtemp())
cfg._features = {"effort": False, "skills": False, "skills_generate": False}
prompt = "Run this exact Bash command and reply with only its output: echo \"effort=$CLAUDE_EFFORT\""
tools = {"allow": ["Bash"], "deny": []}
out = {}

cmd, stdin = cfg.plain_command(prompt, tools)  # carries --effort LEVEL
env = cfg.env_for("plain")
p = subprocess.run(cmd, input=stdin, capture_output=True, text=True, env=env, cwd=tempfile.mkdtemp(), timeout=180)
r = [json.loads(l) for l in p.stdout.splitlines() if l.startswith("{")]
res = next((e for e in r if e.get("type") == "result"), {})
out["--effort flag"] = {"result": res.get("result"), "cost_usd": res.get("total_cost_usd")}

cmd = [c for c in cmd if c not in ("--effort", level)]
env = dict(cfg.env_for("plain"), CLAUDE_CODE_EFFORT_LEVEL=level)
p = subprocess.run(cmd, input=stdin, capture_output=True, text=True, env=env, cwd=tempfile.mkdtemp(), timeout=180)
r = [json.loads(l) for l in p.stdout.splitlines() if l.startswith("{")]
res = next((e for e in r if e.get("type") == "result"), {})
out["CLAUDE_CODE_EFFORT_LEVEL"] = {"result": res.get("result"), "cost_usd": res.get("total_cost_usd")}

out["pinned"] = level
out["both_pinned"] = all(v["result"] == f"effort={level}" for k, v in out.items() if isinstance(v, dict))
print(json.dumps(out, indent=1))
