"""Does a plain Claude Code session invoke a generated skill on an ordinary
request? One live session, as the evaluation's skills condition runs it: the
plugin from `interlock skills generate --target claude-code`, the worker's
tools plus `Skill`, the harness's environment allowlist, no interlock on PATH,
no interlock store, and a bug report that never mentions interlock.

    python3 skills_invoked_probe.py INTERLOCK_BIN [MAX_TURNS [MAX_BUDGET_USD]]   # defaults 12 and 0.08
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

interlock_bin = os.path.abspath(sys.argv[1])
max_turns = int(sys.argv[2]) if len(sys.argv) > 2 else 12
budget = float(sys.argv[3]) if len(sys.argv) > 3 else 0.08
state = tempfile.mkdtemp()
args = SimpleNamespace(host="claude-code", model="claude-sonnet-5-5", effort="medium", max_turns=max_turns,
                       session_timeout_min=5, max_sessions=1, per_run_usd=budget, interlock_bin=interlock_bin,
                       claude_bin=None, copilot_bin=None, fake_model=False, pass_env=[], interlock_skills=False,
                       skills_dir=None, skills_generate=True,
                       skills_generate_cmd="{interlock} skills generate --target {target} --out {out}")
cfg = harness.HostConfig(args, state)
plugin = harness.prepare_skills(args, cfg)

repo = tempfile.mkdtemp()
with open(os.path.join(repo, "stats.py"), "w") as f:
    f.write('def mean(xs):\n    """The arithmetic mean of a non-empty list."""\n    return sum(xs) / (len(xs) - 1)\n')
with open(os.path.join(repo, "test_stats.py"), "w") as f:
    f.write("import unittest\nfrom stats import mean\n\n\nclass T(unittest.TestCase):\n"
            "    def test_one(self):\n        self.assertEqual(mean([5]), 5)\n")
for a in (["init", "-q"], ["add", "-A"], ["-c", "user.name=t", "-c", "user.email=t@example.org", "commit", "-qm", "init"]):
    subprocess.run(["git", *a], cwd=repo, check=True)

prompt = "`mean([2, 4])` in stats.py returns 6.0, not 3.0. Please fix this bug."
tools = {"allow": ["Read", "Grep", "Glob", "Edit", "Write", "Bash"], "deny": ["Bash(git push:*)", "Bash(git commit:*)"]}
cmd, stdin = cfg.plain_command(prompt, tools, plugin, budget)
env = cfg.env_for("skills")
on_path = [d for d in env.get("PATH", "").split(":") if d and os.path.exists(os.path.join(d, "interlock"))]
p = subprocess.run(cmd, input=stdin, capture_output=True, text=True, env=env, cwd=repo, timeout=300)
events = []
for line in p.stdout.splitlines():
    try:
        events.append(json.loads(line))
    except ValueError:
        pass
init = next((e for e in events if e.get("type") == "system" and e.get("subtype") == "init"), {})
result = next((e for e in events if e.get("type") == "result"), {})
calls = [(c["name"], c.get("input") or {}) for e in events if e.get("type") == "assistant"
         for c in e["message"].get("content", []) if c.get("type") == "tool_use"]
fixed = subprocess.run([sys.executable, "-c", "from stats import mean; print(mean([2, 4]))"], cwd=repo,
                       capture_output=True, text=True).stdout.strip()
print(json.dumps({
    "exit_code": p.returncode,
    "max_turns": max_turns,
    "max_budget_usd": budget,
    "interlock_on_path": on_path,
    "interlock_store": os.path.exists(os.path.join(repo, ".interlock", "state.db")),
    "prompt": prompt,
    "skills_listed": [s for s in init.get("skills", []) if s.startswith("interlock:")],
    "tool_calls_in_order": [n + (" " + json.dumps(i.get("skill")) if n == "Skill" else "") for n, i in calls],
    "skills_invoked": [i.get("skill") for n, i in calls if n == "Skill"],
    "shell_commands_calling_interlock": [i["command"] for n, i in calls
                                         if n == "Bash" and "interlock " in i.get("command", "")],
    "mean_2_4_after": fixed,
    "result": (result.get("result") or "")[-600:],
    "stop": result.get("subtype"),
    "cost_usd": result.get("total_cost_usd"),
}, indent=1))
