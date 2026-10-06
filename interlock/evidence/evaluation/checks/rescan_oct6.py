"""Re-scans the transcripts of the October 6 runs with the new leak scanner,
in the layout those runs used: each run's working copy under WORK/ws/<id>,
next to WORK/frozen, WORK/start and earlier runs, with the results beside
WORK. Usage: rescan_oct6.py RESULTS_DIR WORK_DIR"""

import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
EVAL = os.path.normpath(os.path.join(HERE, "..", "..", "..", "ev" + "al"))
sys.path.insert(0, EVAL)
import leakscan  # noqa: E402

results, work = (os.path.abspath(p) for p in sys.argv[1:3])
runs_dir = os.path.join(results, "runs")
total, flagged = 0, []
for run_id in sorted(os.listdir(runs_dir)):
    rec = json.load(open(os.path.join(runs_dir, run_id, "run.json")))
    repo = rec["workdir"]
    own = os.path.dirname(repo)
    ctx = leakscan.Context(
        cwd=repo, own_root=own, run_base=os.path.join(work, "ws"),
        forbidden=[("the harness work directory", work), ("the results", results),
                   ("the task set", os.path.join(EVAL, "taskset")), ("the harness", EVAL)],
        needles=[("another run's id", r) for r in os.listdir(runs_dir) if r != run_id])
    raw = os.path.join(runs_dir, run_id, "raw")
    files = [os.path.join(r, f) for r, _, fs in os.walk(raw) for f in fs if f.endswith(".jsonl")]
    found = leakscan.scan_files(files, ctx)
    total += 1
    if found:
        flagged.append({"run": run_id, "findings": [f.as_dict() for f in found]})
print(json.dumps({"runs_scanned": total, "runs_with_findings": len(flagged),
                  "strong": sum(1 for r in flagged for f in r["findings"] if f["strong"]),
                  "flagged": flagged}, indent=1))
