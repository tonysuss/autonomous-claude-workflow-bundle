"""Are the worker tools that the plain and skills conditions get, read from
`interlock host tools`, the same as in an earlier run's records? No model call.

    python3 tools_parity.py INTERLOCK_BIN RUNS_DIR    # e.g. ../claude-code-v3/runs
"""
import glob
import json
import os
import sys
import tempfile
from types import SimpleNamespace

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.normpath(os.path.join(HERE, "..", "..", "..", "ev" + "al")))
import harness  # noqa: E402

interlock_bin, runs = os.path.abspath(sys.argv[1]), sys.argv[2]
args = SimpleNamespace(host="claude-code", model="claude-sonnet-5-5", effort="medium", max_turns=80,
                       session_timeout_min=15, max_sessions=6, per_run_usd=3, interlock_bin=interlock_bin,
                       claude_bin=None, copilot_bin=None, fake_model=False, pass_env=[], interlock_skills=False)
cfg = harness.HostConfig(args, tempfile.mkdtemp())
out = {"same": [], "different": []}
for t in harness.load_tasks(None):
    now = cfg.worker_tools(t)
    old = [json.load(open(p))["tools"] for p in glob.glob(os.path.join(runs, f"{t.id}.plain.*", "run.json"))][0]
    key = "same" if (now["allow"], now["deny"]) == (old["allow"], old["deny"]) else "different"
    out[key].append({"task": t.id, "now": {"allow": now["allow"], "deny": now["deny"]},
                     "before": {"allow": old["allow"], "deny": old["deny"]}} if key == "different" else t.id)
print(json.dumps(out, indent=1))
