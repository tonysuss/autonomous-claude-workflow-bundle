"""Checks the skills condition's plumbing on Claude Code: the harness wraps a
skills directory as a plugin, and the session's init event lists the skill.
One short session with a trivial prompt; not an evaluation run.

    python3 skills_probe_claude.py SKILLS_DIR
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

skills_dir = sys.argv[1]
args = SimpleNamespace(host="claude-code", model="claude-sonnet-5-5", effort="medium", max_turns=1,
                       session_timeout_min=3, max_sessions=1, per_run_usd=0.5, interlock_bin="/nonexistent/interlock",
                       claude_bin=None, copilot_bin=None, fake_model=False, pass_env=[], interlock_skills=False,
                       skills_dir=skills_dir, skills_generate=False)
cfg = harness.HostConfig(args, tempfile.mkdtemp())
plugin = harness.prepare_skills(args, cfg)
root = tempfile.mkdtemp()
cmd, stdin = cfg.plain_command("Reply with the single word ok.", {"allow": [], "deny": []}, plugin, 0.5)
p = subprocess.run(cmd, input=stdin, capture_output=True, text=True, env=cfg.env_for("skills"), cwd=root, timeout=180)
events = []
for line in p.stdout.splitlines():
    try:
        events.append(json.loads(line))
    except ValueError:
        pass
init = next((e for e in events if e.get("type") == "system" and e.get("subtype") == "init"), {})
result = next((e for e in events if e.get("type") == "result"), {})
print(json.dumps({
    "exit_code": p.returncode,
    "stderr_tail": p.stderr[-300:],
    "plugins": [x.get("name") for x in init.get("plugins", [])],
    "skill_listed": [s for s in init.get("skills", []) if "eval-dummy" in s],
    "result": result.get("result"),
    "cost_usd": result.get("total_cost_usd"),
}, indent=1))
