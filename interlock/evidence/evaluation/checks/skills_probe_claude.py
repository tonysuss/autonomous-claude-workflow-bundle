"""Checks the skills condition's plumbing on Claude Code: the harness wraps a
skills directory as a plugin, and the session's init event lists the skill.
One short session with a trivial prompt; not an evaluation run."""
import json
import os
import subprocess
import sys
import tempfile
from types import SimpleNamespace

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "..", "eval"))
import harness  # noqa: E402

skills_dir = sys.argv[1]
args = SimpleNamespace(host="claude-code", model="claude-sonnet-5-5", effort="medium", max_turns=1,
                       session_timeout_min=3, max_sessions=1, per_run_usd=0.5, interlock_bin="/nonexistent",
                       claude_bin=None, copilot_bin=None, skills_dir=skills_dir, fake_model=False)
cfg = harness.HostConfig(args)
runner = harness.Runner(cfg, tempfile.mkdtemp(), tempfile.mkdtemp(), "probe")
root = tempfile.mkdtemp()
plugin = runner._skills_plugin(root)
cmd, stdin = cfg.plain_command("Reply with the single word ok.", plugin, 0.5)
p = subprocess.run(cmd, input=stdin, capture_output=True, text=True, env=cfg.env(), cwd=root, timeout=180)
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
    "event_types": sorted({e.get("type", "?") for e in events}),
    "plugin_dir": os.path.relpath(plugin, root),
    "plugins": [x.get("name") for x in init.get("plugins", [])],
    "eval_dummy_skill_listed": any("eval-dummy" in s for s in init.get("skills", [])),
    "skills_matching": [s for s in init.get("skills", []) if "dummy" in s],
    "result": result.get("result"),
    "cost_usd": result.get("total_cost_usd"),
}, indent=1))
