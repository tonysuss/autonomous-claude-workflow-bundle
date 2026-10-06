"""v4 next to v3: acceptance and claims per condition and task group, Fisher
exact tests (two-sided) between the cells that matter, paired cost and time
ratios within v4, and skill use. Reads run.json files only.

    python3 compare_v3_v4.py V3_DIR V4_DIR

V3_DIR and V4_DIR are results or exported evidence directories. The harder
tasks are the three whose prompts leave a requirement out, plus py-date-filter.
"""
import glob
import json
import os
import statistics
import sys
from math import comb

HARDER = {"go-dotted-section", "go-duration-days", "py-thousands", "py-date-filter"}


def load(d):
    out = []
    for p in sorted(glob.glob(os.path.join(d, "runs", "*", "run.json"))):
        r = json.load(open(p))
        i = r.get("interruption")
        if i and not i.get("valid"):
            continue  # flagged and re-run; never counted
        out.append(r)
    return out


def fisher(a, n1, c, n2):
    """Two-sided Fisher exact p for a/n1 against c/n2 successes."""
    b, d = n1 - a, n2 - c
    n, r1, k = n1 + n2, n1, a + c
    if n == 0:
        return None

    def p(x):
        return comb(r1, x) * comb(n - r1, k - x) / comb(n, k)

    p0 = p(a)
    lo, hi = max(0, k - (n - r1)), min(r1, k)
    return round(sum(p(x) for x in range(lo, hi + 1) if p(x) <= p0 * (1 + 1e-9)), 3)


def cell(runs):
    m = [r["metrics"] for r in runs]
    return {
        "runs": len(runs),
        "accepted": sum(x["accepted"] for x in m),
        "false_claims": sum(bool(x.get("false_claim")) for x in m),
        "claims_without_evidence": sum(bool(x.get("claim_without_evidence")) for x in m),
        "missed_defects": sum(x["missed_defects"] for x in m),
        "scope_violations": sum(bool(x["scope_violation"]) for x in m),
        "not_claimed": sum(not x["claimed_done"] for x in m),
        "recovered": sum(bool((x.get("recovery") or {}).get("recovered")) for x in m),
        "interrupted_as_designed": sum(bool((x.get("recovery") or {}).get("valid")) for x in m),
        "runs_invoking_skills": sum(bool((r.get("tool_usage") or {}).get("skills_invoked")) for r in runs),
        "skills_invoked": sorted({s for r in runs for s in (r.get("tool_usage") or {}).get("skills_invoked") or []}),
        "failures": [{"run": r["run_id"], "failed": (r.get("judge") or {}).get("failed"),
                      "claimed": r["metrics"]["claimed_done"],
                      "interlock": (r.get("interlock") or {}).get("final_state"),
                      "attempts": (r.get("interlock") or {}).get("attempts_used")}
                     for r in runs if not r["metrics"]["accepted"]],
    }


v3, v4 = load(sys.argv[1]), load(sys.argv[2])
out = {"cells": {}, "fisher_two_sided": {}}


def sel(runs, cond, harder):
    return [r for r in runs if r["condition"] == cond and (r["task"] in HARDER) == harder]


for version, runs in (("v3", v3), ("v4", v4)):
    for cond in ("plain", "skills", "interlock"):
        for group, harder in (("harder", True), ("original", False)):
            rs = sel(runs, cond, harder)
            if rs:
                out["cells"][f"{version} {cond} {group}"] = cell(rs)
        rs = [r for r in runs if r["condition"] == cond]
        if rs:
            out["cells"][f"{version} {cond} all"] = cell(rs)

c = out["cells"]


def test(name, x, y):
    if x in c and y in c:
        out["fisher_two_sided"][name] = {
            x: f"{c[x]['accepted']}/{c[x]['runs']}", y: f"{c[y]['accepted']}/{c[y]['runs']}",
            "p": fisher(c[x]["accepted"], c[x]["runs"], c[y]["accepted"], c[y]["runs"])}


test("interlock harder, v4 against v3 (did the change help?)", "v4 interlock harder", "v3 interlock harder")
test("skills harder, v4 against v3", "v4 skills harder", "v3 skills harder")
test("interlock v4 against plain v3, harder (plain unchanged)", "v4 interlock harder", "v3 plain harder")
test("interlock v4 against skills v4, harder", "v4 interlock harder", "v4 skills harder")
test("skills v4 against plain v3, harder", "v4 skills harder", "v3 plain harder")
test("interlock against plain, original tasks, v4 repeat", "v4 interlock original", "v4 plain original")
test("interlock against skills, original tasks, v4 repeat", "v4 interlock original", "v4 skills original")

# Paired ratios inside v4, by task and repeat; cost only where both runs' cost is complete.
def complete(r):
    return all(s.get("complete") for s in r["sessions"])


by = {(r["task"], r["repeat"], r["condition"]): r for r in v4}
ratios = {}
for a, b in (("interlock", "skills"), ("interlock", "plain"), ("skills", "plain")):
    cost, wall = [], []
    for (t, rep, cnd), r in by.items():
        if cnd != a or (t, rep, b) not in by:
            continue
        o = by[(t, rep, b)]
        wall.append(r["metrics"]["wall_s"] / o["metrics"]["wall_s"])
        if complete(r) and complete(o):
            cost.append(r["metrics"]["cost_usd"] / o["metrics"]["cost_usd"])

    def s(x):
        return {"n": len(x), "median": round(statistics.median(x), 2), "min": round(min(x), 2),
                "max": round(max(x), 2)} if x else None

    ratios[f"{a}/{b}"] = {"cost": s(cost), "wall": s(wall)}
out["v4_paired_ratios"] = ratios
roles = {}
for r in v4:
    if r["condition"] == "interlock" and not r.get("interruption") and complete(r):
        for x in r["sessions"]:
            roles.setdefault(x.get("role"), []).append(x["cost_usd"])
out["v4_interlock_cost_by_role_uninterrupted"] = {k: {"sessions": len(v), "total": round(sum(v), 4)}
                                                 for k, v in roles.items()}
il = [r for r in v4 if r["condition"] == "interlock"]
out["v4_interlock_attempts_used"] = sorted(((r.get("interlock") or {}).get("attempts_used") or 0) for r in il)
# R1: a verifier recorded a failure and the task went back for rework. R3: a
# dead attempt (the forced interruption) was reconciled and a new one opened.
out["v4_interlock_runs_sent_back_by_the_verifier_R1"] = [
    r["run_id"] for r in il if "R1" in ((r.get("interlock") or {}).get("signals") or [])]
out["v4_interlock_runs_restarted_after_interruption_R3"] = [
    r["run_id"] for r in il if "R3" in ((r.get("interlock") or {}).get("signals") or [])]
out["v4_mean_cost_complete"] = {
    cond: round(statistics.mean([r["metrics"]["cost_usd"] for r in v4 if r["condition"] == cond and complete(r)]), 4)
    for cond in ("plain", "skills", "interlock") if any(r["condition"] == cond and complete(r) for r in v4)}
out["v4_mean_wall_s"] = {cond: round(statistics.mean([r["metrics"]["wall_s"] for r in v4 if r["condition"] == cond]), 1)
                         for cond in ("plain", "skills", "interlock") if any(r["condition"] == cond for r in v4)}
print(json.dumps(out, indent=1))
