"""Dumps what a guided run left in a repository's interlock store, for the evidence folder.

usage: dump_run.py INTERLOCK_BIN REPO TASK OUT

Writes task-log.json, status.json, task.json, attempts.json, notes.json, and the store's
results, claims, assessments and check runs as JSON; output.diff (the change, from the input
snapshot to the output tree) when there is one. Attempt tokens are never in the store (only their hashes).
"""
import json
import os
import sqlite3
import subprocess
import sys

interlock, repo, task, out = sys.argv[1:5]
os.makedirs(out, exist_ok=True)
env = {k: v for k, v in os.environ.items() if not k.startswith("INTERLOCK_")}


def cli(*args):
    r = subprocess.run([interlock, *args], cwd=repo, env=env, capture_output=True, text=True, check=True)
    return json.loads(r.stdout)


def save(name, value):
    with open(os.path.join(out, name), "w") as f:
        json.dump(value, f, indent=2)
        f.write("\n")


save("task-log.json", cli("task", "log", task))
save("status.json", cli("status", task))
save("task.json", cli("task", "show", task))
save("attempts.json", cli("attempt", "list", task))
save("notes.json", cli("note", "list", "--task", task))
db = sqlite3.connect(os.path.join(repo, ".interlock", "state.db"))
for table, name in (("results", "results.json"), ("claims", "claims.json"), ("assessments", "assessments.json"),
                    ("check_runs", "check_runs.json")):
    try:
        rows = db.execute(f"SELECT record FROM {table} WHERE task_id = ?", (task,)).fetchall()
    except sqlite3.OperationalError:
        continue
    save(name, [json.loads(r[0]) for r in rows])
show = cli("task", "show", task)
base = (show.get("input_snapshot") or {}).get("base_commit")
tree = show.get("current_tree")
diff = subprocess.run(["git", "-C", repo, "diff", base or "HEAD", tree or "HEAD"], capture_output=True, text=True)
if diff.returncode == 0 and diff.stdout:
    with open(os.path.join(out, "output.diff"), "w") as f:
        f.write(diff.stdout)
print("dumped", task, "to", out)
