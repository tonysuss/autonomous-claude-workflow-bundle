//! Regression tests for the forge review of October 6, 2026. Each names the
//! reviewer's repro it covers; each failed before its fix.

mod common;

use std::time::{Duration, Instant};

use chrono::Utc;
use common::*;
use interlock_core::delivery::UNKNOWN_OUTCOME;
use interlock_forge::{Forge, ForgeError};
use interlock_schema::*;
use serde_json::json;

/// r6: a tree recorded while integrating (as `interlock task tree` does after
/// a rebase) has no evidence. The next pass must apply R2, not push and merge
/// the unverified tree.
#[test]
fn a_tree_recorded_while_integrating_is_r2_never_a_merge() {
    let Some(mut f) = Fixture::new() else { return };
    let tree = f.verified();
    f.grant_landing();
    f.gh.checks("pending");
    assert_eq!(f.integrate().final_state, State::Integrating);
    let unverified = f.tree_with(&tree, "evil.py", "print('unverified')\n");
    f.store.record_new_tree(TASK, &unverified, Utc::now()).unwrap();
    f.gh.checks("pass");

    let d = f.integrate();
    assert_eq!(d.final_state, State::AwaitingVerification, "{d:#?}");
    assert_eq!(f.signals().last().map(String::as_str), Some("R2"));
    assert!(f.gh.calls_to(&["pr", "merge"]).is_empty(), "nothing was merged");
    assert_eq!(f.remote.tip("main").as_deref(), Some(f.base.as_str()));
    assert!(!f.store.evaluate(TASK).unwrap().1.all_pass);

    // Once the new tree is verified, it lands under a fresh G5; the old landing is not left planned.
    f.reverify(&unverified);
    assert_eq!(f.integrate().final_state, State::Done);
    assert!(f.ops().iter().all(|(_, s)| *s != OperationState::Planned), "{:?}", f.ops());
    let ops = f.store.operations(TASK).unwrap();
    let superseded = ops
        .iter()
        .filter(|o| o.state == OperationState::Failed)
        .find(|o| o.outcome.as_ref().is_some_and(|v| v.to_string().contains("superseded before any call")));
    assert!(superseded.is_some(), "the landing planned before R2 is failed as superseded: {ops:#?}");
}

/// r1: auto-merge is armed, then the base moves; the forge merges anyway.
/// The landing is recorded, but it is not the verified landing: no G6.
#[test]
fn an_armed_auto_merge_that_lands_on_a_moved_base_blocks_instead_of_g6() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant_landing();
    f.cfg.auto_merge = true;
    f.gh.checks("pending");
    assert_eq!(f.integrate().final_state, State::Integrating);
    f.remote.commit_file("main", "OTHER.md", "other work\n");
    f.gh.checks("pass");

    let r = f.reconcile();
    assert_eq!(r[0].after, OperationState::Confirmed, "the forge did merge: {r:#?}");
    assert_eq!(f.state(), State::Blocked);
    assert!(f.blocked_reason().starts_with("landed on a base nobody verified"), "{}", f.blocked_reason());
    assert!(!f.signals().contains(&"G6".to_string()));
}

/// r3: a forged `gh` (or a lying API) reports a merge that never happened.
/// git does not see it on the base branch, so it is unknown, never G6.
#[test]
fn a_reported_merge_git_cannot_see_on_the_base_branch_never_lands() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant_landing();
    f.gh.checks("pending");
    assert_eq!(f.integrate().final_state, State::Integrating);
    f.gh.update(|v| v["prs"][0]["lie_merged"] = json!(true));

    let d = f.integrate();
    assert_eq!(d.final_state, State::Blocked, "{d:#?}");
    assert!(f.blocked_reason().contains("as git sees it does not contain the merge"), "{}", f.blocked_reason());
    assert!(!f.signals().contains(&"G6".to_string()));
    assert_eq!(f.remote.tip("main").as_deref(), Some(f.base.as_str()));
}

/// r2: someone retargets the pull request to another branch that points at
/// the same commit. interlock must not merge into it.
#[test]
fn a_pull_request_retargeted_to_another_branch_is_not_merged() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant_landing();
    f.gh.checks("pending");
    assert_eq!(f.integrate().final_state, State::Integrating);
    f.remote.commit_file("main", "unused.txt", "x\n");
    // release-1 at the original base: same commit the evidence was built on.
    let dir = f.remote.path.display().to_string();
    git(&f.repo, &["--git-dir", &dir, "update-ref", "refs/heads/release-1", &f.base], &[]);
    f.gh.update(|v| v["prs"][0]["base"] = json!("release-1"));
    f.gh.checks("pass");

    let d = f.integrate();
    assert_eq!(d.final_state, State::Blocked, "{d:#?}");
    assert!(f.blocked_reason().contains("targets release-1"), "{}", f.blocked_reason());
    assert!(f.gh.calls_to(&["pr", "merge"]).is_empty());
    assert_eq!(f.remote.tip("release-1").as_deref(), Some(f.base.as_str()));
}

/// r8: the merge request reaches the forge (queued) but the call dies with a
/// network error. The operation must stay open for reconcile, which finds
/// the merge once the forge completes it.
#[test]
fn an_unanswered_merge_call_stays_open_until_reconcile_sees_the_merge() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant_landing();
    f.gh.fault("merge", "queue_then_drop", 1);
    let d = f.integrate();
    assert_eq!(d.final_state, State::Blocked, "{d:#?}");
    assert_eq!(f.op(OperationKind::Merge).state, OperationState::Unknown, "not failed: the merge may still land");
    assert!(f.blocked_reason().starts_with(UNKNOWN_OUTCOME));

    // Time passes on the forge: the queued merge completes.
    f.gh.gh(&["pr", "view", "1", "--json", "state"]);
    let r = f.reconcile();
    assert_eq!((r[0].before, r[0].after), (OperationState::Unknown, OperationState::Confirmed), "{r:#?}");
    assert_eq!(f.state(), State::Done);
    assert_eq!(f.gh.calls_to(&["pr", "merge"]).len(), 1);
}

/// r4: right after interlock pushes a new head to an open pull request, the
/// forge still reports the previous head for a few reads.
#[test]
fn a_lagging_head_after_interlocks_own_push_is_read_again_not_blocked() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant_landing();
    f.gh.checks("pending");
    assert_eq!(f.integrate().final_state, State::Integrating);
    let old_head = f.head();
    // The base moves; the rebased tree is verified again.
    f.remote.commit_file("main", "OTHER.md", "other work\n");
    f.gh.checks("pass");
    assert_eq!(f.integrate().final_state, State::AwaitingVerification);
    let rebased = f.store.task(TASK).unwrap().current_tree.unwrap();
    f.reverify(&rebased);
    // The next reads of the pull request show the previous head.
    f.gh.set("lag", json!({ "head": old_head, "reads": 3 }));

    let d = f.integrate();
    assert_eq!(d.final_state, State::Done, "{d:#?}");
    assert_eq!(f.gh.calls_to(&["pr", "create"]).len(), 1, "the same pull request carried the new head");
}

/// Item 9: the API's baseRefOid lags behind a base that moved. interlock reads
/// the base with git right before the merge, and nothing is merged.
#[test]
fn the_base_is_read_with_git_right_before_the_merge() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant_landing();
    f.remote.commit_file("main", "OTHER.md", "other work\n");
    f.gh.set("base_oid", json!(f.base.clone()));

    let d = f.integrate();
    assert_eq!(d.final_state, State::AwaitingVerification, "{d:#?}");
    assert!(d.stopped_because.contains("moved from"), "{}", d.stopped_because);
    assert!(f.gh.calls_to(&["pr", "merge"]).is_empty(), "no merge onto a base nobody verified");
}

/// Item 10: a pull request that reports no checks at all is not ready until
/// CI has had time to report after the push.
#[test]
fn no_checks_at_all_is_not_ready_until_ci_has_had_time() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant_landing();
    f.gh.checks("none");
    f.cfg.checks_settle = Duration::from_secs(3600);
    let d = f.integrate();
    assert_eq!(d.final_state, State::Integrating, "{d:#?}");
    assert!(d.stopped_because.contains("reports no checks yet"), "{}", d.stopped_because);
    assert!(f.gh.calls_to(&["pr", "merge"]).is_empty());

    f.cfg.checks_settle = Duration::ZERO;
    assert_eq!(f.integrate().final_state, State::Done);
}

/// Item 12: a `gh` call that never answers times out as unknown, and output
/// a child keeps open after `gh` exits does not hang interlock.
#[test]
fn a_hung_gh_times_out_and_a_lingering_child_does_not_hold_interlock() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant_landing();
    f.gh.checks("pending");
    assert_eq!(f.integrate().final_state, State::Integrating);
    f.gh.set("hang_seconds", json!(20));

    f.forge.timeout = Duration::from_secs(1);
    f.gh.fault("view", "hang", 1);
    let started = Instant::now();
    let err = f.forge.view_pr(1).unwrap_err();
    assert!(matches!(err, ForgeError::Unavailable(_)), "{err}");
    assert!(started.elapsed() < Duration::from_secs(10), "timed out after {:?}", started.elapsed());

    f.forge.timeout = Duration::from_secs(30);
    f.gh.fault("view", "orphan", 1);
    let started = Instant::now();
    let pr = f.forge.view_pr(1).unwrap();
    assert_eq!(pr.number, 1);
    assert!(started.elapsed() < Duration::from_secs(10), "returned after {:?}", started.elapsed());

    // A merge call that hangs leaves its operation unknown, not failed.
    f.forge.timeout = Duration::from_secs(1);
    f.gh.checks("pass");
    f.gh.fault("merge", "hang", 1);
    let d = f.integrate();
    assert_eq!(d.final_state, State::Blocked, "{d:#?}");
    assert_eq!(f.op(OperationKind::Merge).state, OperationState::Unknown);
}

/// r5: the landing grant is revoked while auto-merge is armed. The next
/// reconcile disarms it under its own operation and blocks the task.
#[test]
fn a_grant_revoked_while_auto_merge_is_armed_disarms_it() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    let grant = f.grant_landing();
    f.cfg.auto_merge = true;
    f.gh.checks("pending");
    assert_eq!(f.integrate().final_state, State::Integrating);
    f.store.revoke_grant(&grant.id, Utc::now()).unwrap();

    let r = f.reconcile();
    assert_eq!(f.gh.calls_to(&["pr", "merge", "1", "--disable-auto"]).len(), 1, "{r:#?}");
    assert_eq!(f.op(OperationKind::DisarmAutoMerge).state, OperationState::Confirmed);
    assert_eq!(f.op(OperationKind::ArmAutoMerge).state, OperationState::Failed);
    assert_eq!(f.state(), State::Blocked);
    assert!(f.blocked_reason().contains("interlock disarmed it"), "{}", f.blocked_reason());
    // Checks pass later: nothing merges.
    f.gh.checks("pass");
    f.gh.gh(&["pr", "view", "1", "--json", "state"]);
    assert_eq!(f.remote.tip("main").as_deref(), Some(f.base.as_str()));
}

/// r5: the grant ends, then the armed auto-merge fires before interlock could
/// disarm it. The merge is recorded; it is not G6.
#[test]
fn a_merge_made_after_the_grant_ended_is_not_g6() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    let grant = f.grant_landing();
    f.cfg.auto_merge = true;
    f.gh.checks("pending");
    assert_eq!(f.integrate().final_state, State::Integrating);
    f.store.revoke_grant(&grant.id, Utc::now()).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    f.gh.checks("pass");

    let d = f.integrate();
    assert_ne!(d.final_state, State::Done, "{d:#?}");
    assert_eq!(f.state(), State::Blocked);
    assert!(f.blocked_reason().contains("when no landing authority was granted"), "{}", f.blocked_reason());
    assert_eq!(f.op(OperationKind::ArmAutoMerge).state, OperationState::Confirmed, "the forge did merge");
}

/// Decision on operator authority: interlock opens the pull request pinned at
/// the verified head and waits; the operator merges; reconcile lands it.
#[test]
fn with_operator_authority_interlock_opens_the_pull_request_and_the_operator_merges() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant(LandingAuthority::Operator);
    f.cfg.auto_merge = true;
    let d = f.integrate();
    let head = d.head.clone().unwrap();
    assert_eq!(d.final_state, State::Integrating, "{d:#?}");
    assert_eq!(d.stopped_because, format!("waiting for the operator to merge pull request #1 at {head}"));
    assert!(f.gh.calls_to(&["pr", "merge"]).is_empty(), "interlock neither merges nor arms auto-merge");
    assert_eq!(f.op(OperationKind::Merge).state, OperationState::Planned);

    // The operator merges on the forge.
    assert!(f.gh.gh(&["pr", "merge", "1", "--merge", "--match-head-commit", &head]).status.success());
    let r = f.reconcile();
    assert_eq!(r[0].after, OperationState::Confirmed, "{r:#?}");
    assert_eq!(f.state(), State::Done);
    assert_eq!(f.signals().last().map(String::as_str), Some("G6"));
}

/// Operator authority: a merge the operator made onto a base nobody verified
/// never gives G6.
#[test]
fn an_operator_merge_onto_a_moved_base_is_not_g6() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant(LandingAuthority::Operator);
    f.gh.checks("pending");
    assert_eq!(f.integrate().final_state, State::Integrating);
    f.remote.commit_file("main", "OTHER.md", "other work\n");
    f.gh.checks("pass");
    assert!(f.gh.gh(&["pr", "merge", "1", "--merge"]).status.success());

    f.reconcile();
    assert_eq!(f.state(), State::Blocked);
    assert!(f.blocked_reason().starts_with("landed on a base nobody verified"), "{}", f.blocked_reason());
}

/// Operator authority: a merge the operator made at another head never gives G6.
#[test]
fn an_operator_merge_at_another_head_is_not_g6() {
    let Some(mut f) = Fixture::new() else { return };
    f.verified();
    f.grant(LandingAuthority::Operator);
    assert_eq!(f.integrate().final_state, State::Integrating);
    f.remote.commit_file("interlock/fix-add", "late.txt", "a push nobody verified\n");
    assert!(f.gh.gh(&["pr", "merge", "1", "--merge"]).status.success());

    f.reconcile();
    assert_ne!(f.state(), State::Done);
    assert!(!f.signals().contains(&"G6".to_string()));
    assert!(f.blocked_reason().contains("interlock pinned"), "{}", f.blocked_reason());
}
