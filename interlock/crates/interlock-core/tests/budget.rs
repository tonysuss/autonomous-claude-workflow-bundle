mod common;

use common::*;
use interlock_core::budget::{cost_left_usd, exhausted, total, wall_left_ms};
use interlock_core::lifecycle::{self, RefusalCode};
use interlock_schema::*;

fn spent(attempt: &mut Attempt, wall_ms: u64, cost_usd: Option<f64>, premium: Option<f64>) {
    attempt.spent = Some(Spent { wall_ms, cost_usd, premium_requests: premium, turns: Some(2), model: None });
}

#[test]
fn spending_sums_over_attempts_in_each_hosts_unit() {
    let task = bug_fix_task();
    let mut a = attempt(&task, "w1", Role::Worker, 1);
    let mut b = attempt(&task, "v1", Role::Verifier, 1);
    let c = attempt(&task, "w2", Role::Worker, 2);
    spent(&mut a, 30_000, Some(0.25), None);
    spent(&mut b, 12_500, Some(0.5), None);
    let sum = total(&[a.clone(), b.clone(), c]);
    assert_eq!(sum.wall_ms, 42_500);
    assert_eq!(sum.cost_usd, Some(0.75));
    assert_eq!(sum.premium_requests, None, "no attempt reported premium requests");
    assert_eq!(sum.turns, Some(4));
    spent(&mut a, 1_000, None, Some(1.0));
    assert_eq!(total(&[a]).premium_requests, Some(1.0));
}

#[test]
fn a_budget_is_exhausted_by_any_limit_it_sets() {
    let mut budget = Budget::attempts(3);
    let used = Spent { wall_ms: 61_000, cost_usd: Some(1.2), premium_requests: Some(4.0), turns: None, model: None };
    assert_eq!(exhausted(&budget, &used), None, "attempts only: time and cost are free");
    assert_eq!(wall_left_ms(&budget, &used), None);
    assert_eq!(cost_left_usd(&budget, &used), None);

    budget.max_wall_secs = Some(120);
    assert_eq!(exhausted(&budget, &used), None);
    assert_eq!(wall_left_ms(&budget, &used), Some(59_000));
    budget.max_wall_secs = Some(60);
    let why = exhausted(&budget, &used).unwrap();
    assert!(why.contains("wall-clock budget of 60s is spent (61s used)"), "{why}");
    assert_eq!(wall_left_ms(&budget, &used), Some(0));

    let mut budget = Budget::attempts(3);
    budget.max_cost_usd = Some(1.0);
    assert!(exhausted(&budget, &used).unwrap().contains("cost budget of $1.00 is spent"));
    assert_eq!(cost_left_usd(&budget, &Spent { cost_usd: Some(0.4), ..Spent::default() }), Some(0.6));

    let mut budget = Budget::attempts(3);
    budget.max_premium_requests = Some(5.0);
    assert_eq!(exhausted(&budget, &used), None);
    budget.max_premium_requests = Some(4.0);
    assert!(exhausted(&budget, &used).unwrap().contains("4 premium requests"));
    // A host that reports no cost cannot spend a cost budget.
    let mut budget = Budget::attempts(3);
    budget.max_cost_usd = Some(0.01);
    assert_eq!(exhausted(&budget, &Spent { wall_ms: 5, ..Spent::default() }), None);
}

#[test]
fn resuming_from_an_export_needs_a_ready_task() {
    let task = bug_fix_task();
    let err = lifecycle::resume_from(&task, TREE_B, t(2)).unwrap_err();
    assert_eq!(err.code, RefusalCode::WrongState, "a pending task has no snapshot to resume on");
    let snapshot = Snapshot {
        repository: "/work/repo".into(),
        base_commit: "e43c7ee".into(),
        untracked_hash: None,
        protected_paths: vec![],
    };
    let ready = lifecycle::ready(&task, &[], snapshot, t(1)).unwrap().task;
    let resumed = lifecycle::resume_from(&ready, TREE_B, t(2)).unwrap();
    assert_eq!(resumed.state, State::Ready, "not a move");
    assert_eq!(resumed.current_tree.as_deref(), Some(TREE_B));
    let cancelled = lifecycle::cancel(&ready, "stop", t(3)).unwrap().task;
    assert_eq!(lifecycle::resume_from(&cancelled, TREE_B, t(4)).unwrap_err().code, RefusalCode::Terminal);
}
