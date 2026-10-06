//! Reading `gh pr view --json` output, and the readiness observer's verdicts.

use interlock_core::delivery::PrState;
use interlock_forge::gh::parse_pr;
use interlock_forge::{CheckStatus, Readiness, readiness};
use serde_json::{Value, json};

const HEAD: &str = "a22eefd9ad2a1d0c38501b951e6eebbdb5d655d0";
const BASE: &str = "0c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d";

/// Shaped like gh 2.89's `gh pr view --json` output for the fields interlock asks for.
fn view(overrides: Value) -> Value {
    let mut v = json!({
        "number": 12,
        "url": "https://github.com/acme/app/pull/12",
        "state": "OPEN",
        "isDraft": false,
        "headRefName": "interlock/fix-add",
        "headRefOid": HEAD,
        "baseRefName": "main",
        "baseRefOid": BASE,
        "mergeable": "MERGEABLE",
        "mergeStateStatus": "CLEAN",
        "statusCheckRollup": [
            {"__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "SUCCESS",
             "workflowName": "CI", "detailsUrl": "https://github.com/acme/app/actions/runs/1"},
            {"__typename": "StatusContext", "context": "ci/legacy", "state": "SUCCESS",
             "targetUrl": "https://ci.example/1"},
            {"__typename": "CheckRun", "name": "lint", "status": "COMPLETED", "conclusion": "SKIPPED"}
        ],
        "mergeCommit": null,
        "autoMergeRequest": null
    });
    for (k, val) in overrides.as_object().unwrap() {
        v[k] = val.clone();
    }
    v
}

fn ready(overrides: Value) -> Readiness {
    readiness(&parse_pr(&view(overrides)).unwrap(), HEAD, BASE)
}

#[test]
fn reads_checks_runs_and_commit_statuses() {
    let pr = parse_pr(&view(json!({}))).unwrap();
    assert_eq!((pr.number, pr.state, pr.head.as_str()), (12, PrState::Open, HEAD));
    assert_eq!(pr.base.as_deref(), Some(BASE));
    let statuses: Vec<_> = pr.checks.iter().map(|c| (c.name.as_str(), c.status)).collect();
    assert_eq!(
        statuses,
        [("test", CheckStatus::Passed), ("ci/legacy", CheckStatus::Passed), ("lint", CheckStatus::Skipped)]
    );
    assert!(!pr.auto_merge);
    let merged = parse_pr(&view(json!({"state": "MERGED", "mergeCommit": {"oid": BASE}}))).unwrap();
    assert_eq!(merged.merge_commit.as_deref(), Some(BASE));
    assert!(parse_pr(&json!({"number": 1})).is_err(), "a pull request without a state is unreadable");
}

#[test]
fn ready_only_at_the_verified_head_on_the_verified_base_with_checks_green() {
    assert_eq!(ready(json!({})), Readiness::Ready);
    assert_eq!(ready(json!({"headRefOid": BASE})), Readiness::HeadMoved { found: BASE.into() });
    assert_eq!(ready(json!({"baseRefOid": HEAD})), Readiness::BaseMoved { found: HEAD.into() });
    assert_eq!(ready(json!({"state": "MERGED"})), Readiness::Merged { head: HEAD.into() });
    assert!(matches!(ready(json!({"state": "CLOSED"})), Readiness::Refused(_)));
}

#[test]
fn checks_and_mergeability_decide_wait_or_refuse() {
    let pending = json!({"statusCheckRollup": [{"__typename": "CheckRun", "name": "test", "status": "IN_PROGRESS",
                                                "conclusion": ""}], "mergeStateStatus": "BLOCKED"});
    assert_eq!(ready(pending), Readiness::Waiting("checks pending on pull request #12: test".into()));
    let expected =
        json!({"statusCheckRollup": [{"__typename": "StatusContext", "context": "deploy", "state": "EXPECTED"}]});
    assert!(matches!(ready(expected), Readiness::Waiting(_)));
    let failed = json!({"statusCheckRollup": [{"__typename": "CheckRun", "name": "test", "status": "COMPLETED",
                                               "conclusion": "FAILURE"}]});
    assert_eq!(ready(failed), Readiness::Refused("checks failed on pull request #12: test".into()));
    let errored = json!({"statusCheckRollup": [{"__typename": "StatusContext", "context": "ci", "state": "ERROR"}]});
    assert!(matches!(ready(errored), Readiness::Refused(_)));
    assert!(matches!(ready(json!({"mergeable": "CONFLICTING", "mergeStateStatus": "DIRTY"})), Readiness::Refused(_)));
    assert!(matches!(ready(json!({"mergeable": "UNKNOWN"})), Readiness::Waiting(_)));
    assert!(matches!(ready(json!({"mergeStateStatus": "BLOCKED"})), Readiness::Waiting(_)));
    assert!(matches!(ready(json!({"isDraft": true})), Readiness::Waiting(_)));
    assert_eq!(ready(json!({"mergeStateStatus": "UNSTABLE"})), Readiness::Ready, "only non-required checks fail");
}
