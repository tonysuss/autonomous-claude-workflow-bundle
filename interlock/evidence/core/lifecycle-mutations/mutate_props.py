"""Breaks one lifecycle guard at a time, in a copy of the workspace, and runs
the lifecycle property tests against it with five seeds. Before every run the
saved-failures file is deleted and persistence is off, so no case found for
one mutation is replayed against another.

Usage: python3 -I evidence/core/lifecycle-mutations/mutate_props.py <interlock dir> [mutation ...]
Prints one line per mutation: how many of the five seeds the property
caught it with, and whether the deterministic coverage test fails. The copy
and its build go in a new temporary directory, removed at the end.
"""
import os
import shutil
import subprocess
import sys
import tempfile

SRC = sys.argv[1]
WORK = tempfile.mkdtemp(prefix="interlock-mutations-")
ROOT = f"{WORK}/interlock"
# Fresh modification times: a build of an earlier mutation must never look newer than these sources.
shutil.copytree(SRC, ROOT, ignore=shutil.ignore_patterns("target", ".git", "evidence", "eval"), copy_function=shutil.copy)
F = f"{ROOT}/crates/interlock-core/src/lifecycle.rs"
REG = f"{ROOT}/crates/interlock-core/tests/lifecycle_props.proptest-regressions"
orig = open(F).read()

M = {
    # The eighteen a review ran first, under its names.
    "G6-short-prefix": (
        "if !crate::evidence::same_tree(&head_sha.to_ascii_lowercase(), &expected.to_ascii_lowercase()) {",
        "if !(expected.len() >= 7 && (head_sha.starts_with(expected) || expected.starts_with(head_sha.as_str()))) {",
    ),
    "G6-no-evidence": ("            if !evidence.all_pass {\n                let reason = format!(\n                    \"merged at",
                       "            if false && !evidence.all_pass {\n                let reason = format!(\n                    \"merged at"),
    "G4-not-all-pass": ("State::AwaitingVerification if report.all_pass => Some(transition(",
                        "State::AwaitingVerification if !report.any_fail => Some(transition("),
    "G3-epoch": ("if epoch != task.lease_epoch || attempt.epoch != task.lease_epoch {", "if false {"),
    "G3-scope": ("    if !violations.is_empty() {\n        return Ok(Submission::Rejected",
                 "    if false {\n        return Ok(Submission::Rejected"),
    "R1-budget": ("        State::AwaitingVerification if report.any_fail => {\n            if task.attempts_used < task.budget.max_attempts {",
                  "        State::AwaitingVerification if report.any_fail => {\n            if true {"),
    "R3-budget": ("    require_state(task, Signal::R3, &[State::Running])?;\n    if task.attempts_used >= task.budget.max_attempts {",
                  "    require_state(task, Signal::R3, &[State::Running])?;\n    if false {"),
    "G5-authority": ("    if landing == LandingAuthority::None {\n        let reason = \"no landing authority",
                     "    if false {\n        let reason = \"no landing authority"),
    "G5-evidence": ("    if !report.all_pass {\n        return Err(Refusal {\n            signal: Some(Signal::G5),",
                    "    if false {\n        return Err(Refusal {\n            signal: Some(Signal::G5),"),
    "cancel-terminal": ("    if task.state.is_terminal() {\n        return Err(refuse(Some(Signal::Cancel)",
                        "    if false {\n        return Err(refuse(Some(Signal::Cancel)"),
    "G1-deps": ("    if !not_done.is_empty() {", "    if false {"),
    "G7-integration": ("State::Verified if !task.integration_required => {", "State::Verified => {"),
    "R2-integrating-dropped": ("State::Verified | State::Integrating if !report.all_pass => Some(transition(",
                               "State::Verified if !report.all_pass => Some(transition("),
    "G2-no-epoch-bump": ("let epoch = task.lease_epoch + 1;", "let epoch = task.lease_epoch;"),
    "G2-budget": ("    require_state(task, Signal::G2, &[State::Ready])?;\n    if task.attempts_used >= task.budget.max_attempts {",
                  "    require_state(task, Signal::G2, &[State::Ready])?;\n    if false {"),
    "block-from-blocked": ("    if task.state.is_terminal() || task.state == State::Blocked {\n        return Err(refuse(Some(Signal::Block)",
                           "    if task.state.is_terminal() {\n        return Err(refuse(Some(Signal::Block)"),
    "unblock-wrong-state": ("let to = task.resume_point.unwrap_or(State::Pending);",
                            "let to = if task.resume_point == Some(State::Integrating) { State::Verified } else { task.resume_point.unwrap_or(State::Pending) };"),
    "G6-wrong-state-ok": ("    require_state(task, Signal::G6, &[State::Integrating])?;",
                          "    require_state(task, Signal::G6, &[State::Integrating, State::Verified])?;"),
    # Further guards.
    "G6-tree-unchecked": ("            if let Some(pinned) = operation.intent.tree.as_deref()\n                && !crate::evidence::same_tree(pinned, current)",
                          "            if let Some(pinned) = operation.intent.tree.as_deref()\n                && false && !crate::evidence::same_tree(pinned, current)"),
    "G6-case-sensitive": ("if !crate::evidence::same_tree(&head_sha.to_ascii_lowercase(), &expected.to_ascii_lowercase()) {",
                          "if !crate::evidence::same_tree(head_sha, expected) {"),
    "G6-refused-not-R2": ("            Ok(transition(task, Signal::R2, State::AwaitingVerification, format!(\"merge refused: {reason}\"), now))",
                          "            Err(refuse(Some(Signal::G6), RefusalCode::HeadMismatch, reason.clone()))"),
    "G3-current-attempt": ("    if task.current_attempt.as_deref() != Some(attempt.id.as_str()) {", "    if false {"),
    "G3-not-running": ("    if task.state != State::Running {\n        return superseded(", "    if false {\n        return superseded("),
    "R2-verified-dropped": ("State::Verified | State::Integrating if !report.all_pass => Some(transition(",
                            "State::Integrating if !report.all_pass => Some(transition("),
    "G7-before-R2": ("State::Verified | State::Integrating if !report.all_pass => Some(transition(",
                     "State::Verified | State::Integrating if false => Some(transition("),
    "R1-fix-brief-keeps-attempt": ("                out.task.current_attempt = None;\n                Some(out)",
                                   "                Some(out)"),
    "R3-keeps-attempt": ("    let mut out = transition(task, Signal::R3, State::Ready, reason, now);\n    out.task.current_attempt = None;",
                         "    let out = transition(task, Signal::R3, State::Ready, reason, now);"),
    "R3-from-ready": ("    require_state(task, Signal::R3, &[State::Running])?;",
                      "    require_state(task, Signal::R3, &[State::Running, State::Ready])?;"),
    "fail-from-terminal": ("    if task.state.is_terminal() {\n        return Err(refuse(Some(Signal::Fail)",
                           "    if false {\n        return Err(refuse(Some(Signal::Fail)"),
    "block-forgets-where": ("    out.task.resume_point = Some(task.state);\n    out.task.blocked_reason = Some(reason.to_string());\n    Ok(out)\n}\n\n/// Returns",
                            "    out.task.resume_point = Some(State::Ready);\n    out.task.blocked_reason = Some(reason.to_string());\n    Ok(out)\n}\n\n/// Returns"),
    "verifier-from-verified": ("    require_state(task, Signal::G4, &[State::AwaitingVerification])?;",
                               "    require_state(task, Signal::G4, &[State::AwaitingVerification, State::Verified])?;"),
    "new-tree-while-running": ("    require_state(task, Signal::R2, &[State::AwaitingVerification, State::Verified, State::Integrating])?;",
                               "    require_state(task, Signal::R2, &[State::Running, State::AwaitingVerification, State::Verified, State::Integrating])?;"),
    "G5-without-integration": ("    if !task.integration_required {\n        return Err(refuse(Some(Signal::G5)",
                               "    if false {\n        return Err(refuse(Some(Signal::G5)"),
}

env = dict(os.environ, CARGO_TARGET_DIR=f"{WORK}/target", CARGO_PROFILE_DEV_DEBUG="0", CARGO_INCREMENTAL="0",
           PROPTEST_DISABLE_FAILURE_PERSISTENCE="1")
SEEDS = ["11", "22", "33", "44", "55"]
only = sys.argv[2:] or list(M)


def cargo(args, extra_env=None):
    return subprocess.run(["cargo", "test", "-q", "-p", "interlock-core", "--test", "lifecycle_props", *args],
                          cwd=ROOT, env=dict(env, **(extra_env or {})), capture_output=True, text=True)


try:
    base = cargo(["--", "--test-threads=2"])
    print("unmutated:", "passes" if base.returncode == 0 else "FAILS\n" + base.stdout[-2000:], flush=True)
    for name in only:
        a, b = M[name]
        assert orig.count(a) == 1, (name, orig.count(a))
        open(F, "w").write(orig.replace(a, b))
        caught, first = 0, ""
        for seed in SEEDS:
            if os.path.exists(REG):
                os.remove(REG)
            r = cargo(["the_guards_hold"], {"PROPTEST_RNG_SEED": seed})
            out = r.stdout + r.stderr
            if "error[" in out:
                first = "did not compile"
                break
            if r.returncode != 0:
                caught += 1
                first = first or next((l.strip()[:150] for l in out.splitlines() if "Test failed:" in l), "failed")
        cov = cargo(["the_sequences"])
        coverage = "fails" if cov.returncode != 0 else "passes"
        print(f"{name}: caught {caught}/{len(SEEDS)}; coverage test {coverage} :: {first}", flush=True)
finally:
    shutil.rmtree(WORK)
