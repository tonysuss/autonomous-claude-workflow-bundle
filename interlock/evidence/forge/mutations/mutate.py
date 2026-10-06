"""Undo one review fix at a time, run the test that covers it, record whether
it fails, and restore the file. Exact-text replacements; each anchor must
occur once. Restores from memory, so nothing but the files themselves is
touched."""

import json
import os
import subprocess
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "../../.."))
OUT = os.path.join(ROOT, "evidence/forge/mutations")
ENV = dict(os.environ, CARGO_PROFILE_DEV_DEBUG="0", CARGO_INCREMENTAL="0")

DELIVER = "crates/interlock-forge/src/deliver.rs"
STORE_OPS = "crates/interlock-store/src/operations.rs"
CORE = "crates/interlock-core/src/delivery.rs"

MUTATIONS = [
    {
        "id": "m01-stale-evidence-merges",
        "finding": "1 (blocker): stale evidence does not stop a merge while integrating",
        "undo": [
            (STORE_OPS,
             'if !report.all_pass {\n                let why = "the evidence no longer covers the task\'s current tree; nothing was sent',
             'if false {\n                let why = "the evidence no longer covers the task\'s current tree; nothing was sent'),
            (STORE_OPS, "pin.tree.as_deref().filter(", "None::<&str>.filter("),
            (DELIVER,
             "        // Evidence first: a tree recorded while integrating, or anything else that made it stale, is R2.\n"
             "        let moves = self.store.advance(&task.id, Utc::now())?;\n        if !moves.is_empty() {",
             "        let moves = self.store.advance(&task.id, Utc::now())?;\n        if false && !moves.is_empty() {"),
            (DELIVER,
             "                let refused = Verdict::Land { report: MergeReport::Refused { reason }, merged_at: None };\n"
             "                self.settle(&op, &refused, json!({ \"called\": false }))?;\n"
             "                return Ok(Flow::Next);\n",
             "                let _ = reason;\n"
             "                self.settle(&op, &Verdict::Failed { reason: \"replaced\".into() }, json!({ \"called\": false }))?;\n"
             "                let intent = OperationIntent { expected_head_sha: Some(head.clone()), base: self.cfg.base.clone(), pull_request: None };\n"
             "                self.store.plan_operation(&task.id, self.landing_kind(authority), intent, Utc::now())?\n"),
            ("crates/interlock-supervisor/src/delivery.rs",
             "    if !store.advance(task_id, Utc::now())?.is_empty() {",
             "    if task.state == State::Verified && !store.advance(task_id, Utc::now())?.is_empty() {"),
        ],
        "tests": [
            ["-p", "interlock-forge", "--test", "regressions", "--", "a_tree_recorded_while_integrating_is_r2_never_a_merge"],
            ["-p", "interlock-cli", "--test", "forge_cli", "--", "r6_a_tree_recorded_while_integrating_is_r2_under_interlock_run"],
        ],
    },
    {
        "id": "m02-no-landing-checks",
        "finding": "2 (major) and 3: a merge onto a moved base, or one git cannot see, lands the task",
        "undo": [
            (CORE,
             "    let base_branch = pr.base_branch.as_deref().unwrap_or(\"its base branch\");\n    let Some(landed) = &pr.landed else {",
             "    return Verdict::Land { report: MergeReport::Merged { head_sha: pr.head.clone() }, merged_at: pr.merged_at };\n"
             "    #[allow(unreachable_code)]\n"
             "    let base_branch = pr.base_branch.as_deref().unwrap_or(\"its base branch\");\n    let Some(landed) = &pr.landed else {"),
        ],
        "tests": [
            ["-p", "interlock-forge", "--test", "regressions", "--", "an_armed_auto_merge_that_lands_on_a_moved_base_blocks_instead_of_g6"],
            ["-p", "interlock-forge", "--test", "regressions", "--", "a_reported_merge_git_cannot_see_on_the_base_branch_never_lands"],
            ["-p", "interlock-forge", "--test", "regressions", "--", "an_operator_merge_onto_a_moved_base_is_not_g6"],
        ],
    },
    {
        "id": "m03-forge-commands-inside-attempts",
        "finding": "3 (major): an agent can run forge commands and point interlock at a forged gh",
        "undo": [
            ("crates/interlock-cli/src/forge.rs", "    if inside_attempt() {\n        bail!", "    if false && inside_attempt() {\n        bail!"),
            ("crates/interlock-core/src/classify.rs", '    &["integrate", "run"],\n    &["reconcile"],\n', ""),
        ],
        "tests": [
            ["-p", "interlock-cli", "--test", "forge_cli", "--", "r3_forge_commands_are_the_operators_inside_an_attempt"],
            ["-p", "interlock-core", "--lib", "--", "classify::tests::forge_commands_belong_to_the_operator"],
        ],
    },
    {
        "id": "m04-retargeted-base",
        "finding": "4: readiness ignores the pull request's base branch",
        "undo": [("crates/interlock-forge/src/lib.rs", "    if pr.base_branch != base_branch {", "    if false {")],
        "tests": [["-p", "interlock-forge", "--test", "regressions", "--", "a_pull_request_retargeted_to_another_branch_is_not_merged"]],
    },
    {
        "id": "m05-unanswered-merge-failed",
        "finding": "5: an unanswered merge call is recorded failed",
        "undo": [(DELIVER, "(Verdict::Failed { .. }, Err(e)) => Verdict::Unknown {", "(Verdict::Failed { .. }, Err(e)) => Verdict::Block { took_effect: false,")],
        "tests": [["-p", "interlock-forge", "--test", "regressions", "--", "an_unanswered_merge_call_stays_open_until_reconcile_sees_the_merge"]],
    },
    {
        "id": "m06-push-lag-blocks",
        "finding": "6: a lagging head right after interlock's own push blocks the task",
        "undo": [(DELIVER, "            if !lagging {", "            if true || !lagging {")],
        "tests": [["-p", "interlock-forge", "--test", "regressions", "--", "a_lagging_head_after_interlocks_own_push_is_read_again_not_blocked"]],
    },
    {
        "id": "m07a-no-disarm",
        "finding": "7: revoked authority leaves auto-merge armed",
        "undo": [(DELIVER, "    if armed && !interlock_merges(", "    if false && armed && !interlock_merges(")],
        "tests": [["-p", "interlock-forge", "--test", "regressions", "--", "a_grant_revoked_while_auto_merge_is_armed_disarms_it"]],
    },
    {
        "id": "m07b-merge-after-grant-ended",
        "finding": "7: a merge made after the grant ended gets G6",
        "undo": [(STORE_OPS, "                } else if landing == LandingAuthority::None {", "                } else if false {")],
        "tests": [
            ["-p", "interlock-forge", "--test", "regressions", "--", "a_merge_made_after_the_grant_ended_is_not_g6"],
            ["-p", "interlock-store", "--test", "operations", "--", "a_merge_made_after_landing_authority_ended_is_recorded_but_not_g6"],
        ],
    },
    {
        "id": "m08a-no-fixed-dates",
        "finding": "8: the verified head depends on the clock",
        "undo": [("crates/interlock-forge/src/git.rs", '        ("GIT_AUTHOR_DATE", date.as_str()),\n        ("GIT_COMMITTER_DATE", date.as_str()),\n', "")],
        "tests": [["-p", "interlock-forge", "--test", "deliver", "--", "the_verified_head_is_the_same_commit_in_another_clone"]],
    },
    {
        "id": "m08b-signing-allowed",
        "finding": "8: the verified head is signed when commit.gpgsign is set",
        "undo": [("crates/interlock-forge/src/git.rs", '"commit-tree", "--no-gpg-sign", tree', '"commit-tree", tree')],
        "tests": [["-p", "interlock-forge", "--test", "deliver", "--", "the_verified_head_is_the_same_commit_in_another_clone"]],
    },
    {
        "id": "m08c-asked-to-sign",
        "finding": "8: the verified head is signed (commit-tree asked to sign, under a failing signer)",
        "undo": [("crates/interlock-forge/src/git.rs", '"commit-tree", "--no-gpg-sign", tree', '"commit-tree", "-S", tree')],
        "tests": [["-p", "interlock-forge", "--test", "deliver", "--", "the_verified_head_is_the_same_commit_in_another_clone"]],
    },
    {
        "id": "m09-base-not-read-before-merge",
        "finding": "9: the base is not read with git right before the merge",
        "undo": [(DELIVER, "            Ok(Some(tip)) if same_sha(&tip, &base_commit) => {}", "            Ok(Some(_)) if true => {}")],
        "tests": [["-p", "interlock-forge", "--test", "regressions", "--", "the_base_is_read_with_git_right_before_the_merge"]],
    },
    {
        "id": "m10-no-checks-settle",
        "finding": "10: no checks at all counts as ready at once",
        "undo": [(DELIVER, "                if quiet < self.cfg.checks_settle {", "                if false {")],
        "tests": [["-p", "interlock-forge", "--test", "regressions", "--", "no_checks_at_all_is_not_ready_until_ci_has_had_time"]],
    },
    {
        "id": "m11-verifier-after-moved-head",
        "finding": "11: a moved-head R2 starts a verifier session instead of advancing",
        "undo": [("crates/interlock-supervisor/src/run.rs",
                  "                State::AwaitingVerification if !self.store.advance(task_id, Utc::now())?.is_empty() => {}\n", "")],
        "tests": [["-p", "interlock-cli", "--test", "forge_cli", "--", "a_moved_head_r2_needs_no_verifier_session"]],
    },
    {
        "id": "m12-unbounded-output",
        "finding": "12: a child holding gh's output open hangs interlock",
        "undo": [("crates/interlock-forge/src/gh.rs", "    while !handle.is_finished() && Instant::now() < until {", "    while !handle.is_finished() {")],
        "tests": [["-p", "interlock-forge", "--test", "regressions", "--", "a_hung_gh_times_out_and_a_lingering_child_does_not_hold_interlock"]],
    },
    {
        "id": "m13-planned-landing-left-dangling",
        "finding": "1: a landing planned before an R2 stays planned after the next G5",
        "undo": [(DELIVER, "            for old in stale {", "            for old in stale.into_iter().take(0) {")],
        "tests": [["-p", "interlock-forge", "--test", "regressions", "--", "a_tree_recorded_while_integrating_is_r2_never_a_merge"]],
    },
]


def run_test(args):
    p = subprocess.run(["cargo", "test", *args], cwd=ROOT, env=ENV, capture_output=True, text=True)
    text = p.stdout + p.stderr
    lines = [l for l in text.splitlines() if not l.strip().startswith(("Compiling", "Finished", "Running", "Blocking"))]
    return p.returncode, "\n".join(lines[-25:])


def main():
    only = set(sys.argv[1:])
    os.makedirs(OUT, exist_ok=True)
    summary = []
    for m in MUTATIONS:
        if only and m["id"] not in only:
            continue
        originals = {}
        try:
            for path, old, new in m["undo"]:
                full = os.path.join(ROOT, path)
                text = originals.get(full) or open(full).read()
                originals.setdefault(full, text)
                current = open(full).read()
                assert current.count(old) == 1, f"{m['id']}: anchor not found once in {path}: {old[:60]!r}"
                open(full, "w").write(current.replace(old, new))
            results = []
            for t in m["tests"]:
                code, tail = run_test(t)
                results.append({"test": " ".join(t), "exit": code, "failed": code != 0, "tail": tail})
        finally:
            for full, text in originals.items():
                open(full, "w").write(text)
        entry = {"id": m["id"], "finding": m["finding"], "undo": [[p, o, n] for p, o, n in m["undo"]], "results": results}
        with open(os.path.join(OUT, m["id"] + ".json"), "w") as f:
            json.dump(entry, f, indent=2)
        summary.append({"id": m["id"], "finding": m["finding"], "all_failed": all(r["failed"] for r in results),
                        "tests": [(r["test"], "FAILED" if r["failed"] else "passed") for r in results]})
        print(m["id"], "->", [("FAILED" if r["failed"] else "passed") for r in results], flush=True)
    path = os.path.join(OUT, "summary.json")
    earlier = json.load(open(path)) if os.path.exists(path) else []
    ran = {s["id"] for s in summary}
    merged = [s for s in earlier if s["id"] not in ran] + summary
    order = [m["id"] for m in MUTATIONS]
    merged.sort(key=lambda s: order.index(s["id"]) if s["id"] in order else len(order))
    with open(path, "w") as f:
        json.dump(merged, f, indent=2)


main()
