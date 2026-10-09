"""Paired cost and wall-time ratios, interlock's cost by session role, and a
per-run summary, from a results or exported evidence directory. Reads the
run.json files only. Pairs are the same task and repeat; a cost ratio needs
both runs' cost complete (an interrupted run's is not).

    python3 paired_ratios.py interlock/evidence/evaluation/claude-code-v3
"""
import glob
import json
import os
import statistics
import sys

R = sys.argv[1]
runs = []
for p in sorted(glob.glob(os.path.join(R, "runs", "*", "run.json"))):
    r = json.load(open(p))
    runs.append(r)


def counts(r):
    i = r.get("interruption")
    return not (i and not i.get("valid"))


counted = [r for r in runs if counts(r)]
by = {}
for r in counted:
    by[(r["task"], r["repeat"], r["condition"])] = r

out = {"runs_total": len(runs), "runs_counted": len(counted),
       "not_counted": [r["run_id"] for r in runs if not counts(r)]}


def complete(r):
    return all(s.get("complete") for s in r["sessions"])


pairs = {}
for a, b in (("skills", "plain"), ("interlock", "plain"), ("interlock", "skills")):
    cost, wall, cost_unint, wall_unint = [], [], [], []
    for (t, rep, c), r in by.items():
        if c != a or (t, rep, b) not in by:
            continue
        o = by[(t, rep, b)]
        wall.append(r["metrics"]["wall_s"] / o["metrics"]["wall_s"])
        inter = bool(r.get("interruption"))
        if not inter:
            wall_unint.append(wall[-1])
        if complete(r) and complete(o):
            cost.append(r["metrics"]["cost_usd"] / o["metrics"]["cost_usd"])
            if not inter:
                cost_unint.append(cost[-1])

    def s(x):
        return {"n": len(x), "median": round(statistics.median(x), 2), "min": round(min(x), 2),
                "max": round(max(x), 2)} if x else None

    pairs[f"{a}/{b}"] = {"cost": s(cost), "wall": s(wall), "cost_uninterrupted": s(cost_unint),
                         "wall_uninterrupted": s(wall_unint)}
out["paired_ratios"] = pairs

roles = {}
for r in counted:
    if r["condition"] != "interlock" or r.get("interruption") or not complete(r):
        continue
    for s in r["sessions"]:
        roles.setdefault(s.get("role"), []).append(s["cost_usd"])
out["interlock_role_cost_uninterrupted"] = {k: {"sessions": len(v), "total": round(sum(v), 4)} for k, v in roles.items()}
out["interlock_runs_uninterrupted_complete"] = sum(
    1 for r in counted if r["condition"] == "interlock" and not r.get("interruption") and complete(r))
mean_plain = [r["metrics"]["cost_usd"] for r in counted if r["condition"] == "plain" and not r.get("interruption") and complete(r)]
out["plain_mean_cost_uninterrupted"] = round(statistics.mean(mean_plain), 4) if mean_plain else None

rows = []
for r in runs:
    m = r["metrics"]
    il = r.get("interlock") or {}
    rows.append({
        "run": r["run_id"], "label": r.get("label"), "counted": counts(r), "accepted": m["accepted"],
        "claimed": m["claimed_done"], "failed": (r.get("judge") or {}).get("failed"),
        "cwe": m.get("claim_without_evidence"), "scope": m["scope_violation"], "recovery": m.get("recovery"),
        "il": il.get("final_state"), "signals": il.get("signals"), "skills": (r.get("tool_usage") or {}).get("skills_invoked"),
        "il_cmds": (r.get("tool_usage") or {}).get("interlock_commands"), "leak": m.get("hidden_material_seen"),
        "cost": m["cost_usd"], "cost_complete": complete(r), "charge": r.get("budget_charge_usd"), "wall": m["wall_s"],
        "sessions": m["sessions"],
    })
out["runs"] = rows
out["spend"] = {"observed": round(sum(r["metrics"]["cost_usd"] or 0 for r in runs), 4),
                "charged": round(sum(r.get("budget_charge_usd", 0) for r in runs), 4)}
print(json.dumps(out, indent=1))
