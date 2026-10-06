//! Property tests for the lifecycle guards. Each case creates a task and
//! drives a random sequence of moves through the pure lifecycle API, as any
//! caller could: G1 to G7, R1 to R3, results from current and late attempts,
//! evidence from either role, block and unblock, fail, cancel, and new trees.
//! After every step it checks the design's invariants:
//!
//! - only the transitions in the guard list happen;
//! - the lease epoch never decreases, and a result with a stale epoch is never applied;
//! - done is reached only through G7, or G5 then G6, after G4, with current passing evidence;
//! - a terminal state is never left;
//! - unblock returns exactly to the state the task left, with its work;
//! - attempts used never exceed the budget, and an exhausted budget gives failed, not ready.

mod common;

use common::*;
use interlock_core::EvidenceReport;
use interlock_core::capability::{Capability, CapabilitySet};
use interlock_core::lifecycle::{self, EvidenceKind, MergeReport, Outcome, RefusalCode, Signal, Start, Submission};
use interlock_core::workflow::Mode;
use interlock_schema::*;
use proptest::prelude::*;

const TREE_C: &str = "ccccccc3333333333333333333333333333333333";
const TREES: [&str; 3] = [TREE_A, TREE_B, TREE_C];
const UPSTREAM: &str = "schema-change";

#[derive(Debug, Clone, Copy)]
enum EpochPick {
    /// The epoch the attempt was opened at.
    Own,
    /// The task's lease epoch now.
    Current,
    Older,
    Newer,
}

#[derive(Debug, Clone, Copy)]
enum Merge {
    /// The forge merged the pinned head, reported with its first `cut` characters.
    Pinned {
        cut: usize,
    },
    /// The forge merged some other head.
    Other,
    Refused,
}

#[derive(Debug, Clone)]
enum Step {
    /// Whatever moves the task forward from where it is, as a cooperative
    /// worker, verifier and operator would; the other steps perturb it.
    Progress {
        tree: usize,
    },
    Ready {
        upstream_done: bool,
    },
    StartWorker {
        capable: bool,
    },
    StartVerifier {
        capable: bool,
    },
    /// A result from the current worker attempt, or from the `late`-th one before it.
    Submit {
        late: Option<usize>,
        epoch: EpochPick,
        tree: usize,
        in_scope: bool,
    },
    /// Any evidence from the latest attempt of either role, on the current tree or another.
    Evidence {
        from_verifier: bool,
        assessment: bool,
        repro: bool,
        strength: Strength,
        tree: Option<usize>,
    },
    /// The latest verifier assesses both criteria on the current tree: observed, or a failed reproduction.
    Verify {
        pass: bool,
    },
    /// The latest worker claims the regression suite passes on the current tree.
    Claim,
    Advance,
    Retry,
    Block,
    Unblock,
    Fail,
    Cancel,
    BeginIntegration {
        authority: LandingAuthority,
    },
    ConfirmIntegration {
        merge: Merge,
    },
    NewTree {
        tree: usize,
    },
}

fn strength() -> impl Strategy<Value = Strength> {
    prop_oneof![
        3 => Just(Strength::Observed),
        2 => Just(Strength::Tested),
        1 => Just(Strength::Static),
        1 => Just(Strength::Blocked),
        2 => Just(Strength::Failed),
    ]
}

fn step() -> impl Strategy<Value = Step> {
    let epoch = prop_oneof![
        4 => Just(EpochPick::Own),
        1 => Just(EpochPick::Current),
        1 => Just(EpochPick::Older),
        1 => Just(EpochPick::Newer),
    ];
    let authority = prop_oneof![
        Just(LandingAuthority::None),
        Just(LandingAuthority::Coordinator),
        Just(LandingAuthority::Owner),
        Just(LandingAuthority::Operator),
    ];
    let merge = prop_oneof![
        3 => (0usize..=41).prop_map(|cut| Merge::Pinned { cut }),
        1 => Just(Merge::Other),
        1 => Just(Merge::Refused),
    ];
    // Weighted so that most sequences get deep enough to meet every guard
    // before a failure or cancellation ends them.
    prop_oneof![
        60 => (0usize..3).prop_map(|tree| Step::Progress { tree }),
        10 => prop::bool::weighted(0.85).prop_map(|upstream_done| Step::Ready { upstream_done }),
        20 => prop::bool::weighted(0.95).prop_map(|capable| Step::StartWorker { capable }),
        15 => prop::bool::weighted(0.95).prop_map(|capable| Step::StartVerifier { capable }),
        25 => (prop::option::weighted(0.25, 0usize..3), epoch, 0usize..3, prop::bool::weighted(0.85))
            .prop_map(|(late, epoch, tree, in_scope)| Step::Submit { late, epoch, tree, in_scope }),
        6 => (any::<bool>(), any::<bool>(), any::<bool>(), strength(), prop::option::weighted(0.2, 0usize..3))
            .prop_map(|(from_verifier, assessment, repro, strength, tree)| {
                Step::Evidence { from_verifier, assessment, repro, strength, tree }
            }),
        20 => prop::bool::weighted(0.85).prop_map(|pass| Step::Verify { pass }),
        8 => Just(Step::Claim),
        30 => Just(Step::Advance),
        4 => Just(Step::Retry),
        2 => Just(Step::Block),
        10 => Just(Step::Unblock),
        1 => Just(Step::Fail),
        1 => Just(Step::Cancel),
        12 => authority.prop_map(|authority| Step::BeginIntegration { authority }),
        12 => merge.prop_map(|merge| Step::ConfirmIntegration { merge }),
        4 => (0usize..3).prop_map(|tree| Step::NewTree { tree }),
    ]
}

/// One case: the task's attempt budget, whether it integrates, whether it
/// waits on another task, and the moves made on it.
fn case() -> impl Strategy<Value = (u32, bool, bool, Vec<Step>)> {
    (1u32..=4, any::<bool>(), any::<bool>(), prop::collection::vec(step(), 1..80))
}

/// What the guard list allows: each signal's source and target states.
fn in_guard_list(signal: Signal, from: State, to: State, resume_point: Option<State>) -> bool {
    use State::*;
    let active = !from.is_terminal();
    match signal {
        Signal::G1 => from == Pending && to == Ready,
        Signal::G2 => from == Ready && to == Running,
        Signal::G3 => from == Running && to == AwaitingVerification,
        Signal::G4 => from == AwaitingVerification && to == Verified,
        Signal::G5 => from == Verified && to == Integrating,
        Signal::G6 => from == Integrating && to == Done,
        Signal::G7 => from == Verified && to == Done,
        Signal::R1 => from == AwaitingVerification && to == Ready,
        Signal::R2 => matches!(from, Verified | Integrating) && to == AwaitingVerification,
        Signal::R3 => from == Running && to == Ready,
        Signal::Block => active && from != Blocked && to == Blocked,
        Signal::Unblock => from == Blocked && Some(to) == resume_point,
        Signal::Fail => active && to == Failed,
        Signal::Cancel => active && to == Cancelled,
    }
}

/// The state a block left and the work the task held then.
#[derive(Debug, Clone, PartialEq)]
struct Held {
    state: State,
    tree: Option<String>,
    epoch: u32,
    attempts_used: u32,
}

struct World {
    task: Task,
    workers: Vec<Attempt>,
    verifiers: Vec<Attempt>,
    claims: Vec<Evidence>,
    assessments: Vec<Evidence>,
    operation: Option<Operation>,
    blocked: Option<Held>,
    /// G4 has cleared since the last result or send-back.
    verified: bool,
    clock: i64,
    /// Every signal applied, for checking that the sequences reach each guard.
    signals: Vec<Signal>,
}

fn capabilities(capable: bool) -> CapabilitySet {
    if capable { all_capabilities() } else { [Capability::ToolRestriction].into_iter().collect() }
}

impl World {
    fn new(max_attempts: u32, integration_required: bool, upstream: bool) -> World {
        let mut s = spec(vec![
            criterion("repro", MinStrength::Observed, Producer::Independent),
            criterion("regression", MinStrength::Tested, Producer::SelfReport),
        ]);
        s.budget = Budget::attempts(max_attempts);
        s.integration_required = integration_required;
        if upstream {
            s.dependencies = vec![UPSTREAM.into()];
        }
        let task = lifecycle::create(s, &bug_fix(), t(0)).unwrap();
        World {
            task,
            workers: vec![],
            verifiers: vec![],
            claims: vec![],
            assessments: vec![],
            operation: None,
            blocked: None,
            verified: false,
            clock: 0,
            signals: vec![],
        }
    }

    fn now(&mut self) -> Timestamp {
        self.clock += 1;
        t(self.clock)
    }

    fn report(&self) -> EvidenceReport {
        eval(&self.task, &self.claims, &self.assessments)
    }

    fn current_tree(&self) -> String {
        self.task.current_tree.clone().unwrap_or_else(|| TREE_A.to_string())
    }

    /// Appends a claim or an assessment the source was allowed to record.
    fn record(&mut self, kind: EvidenceKind, source: &Attempt, criterion: &str, strength: Strength, tree: &str) {
        let id = format!("e{}", self.claims.len() + self.assessments.len());
        let e = evidence(&self.task, &id, criterion, &source.id, strength, tree);
        match kind {
            EvidenceKind::Claim => self.claims.push(e),
            EvidenceKind::Assessment => self.assessments.push(e),
        }
    }

    /// Checks one move against the invariants, then makes it the world's state.
    fn apply(&mut self, out: Outcome, report: Option<&EvidenceReport>) -> Result<(), TestCaseError> {
        let (before, after, mv) = (&self.task, &out.task, &out.mv);
        prop_assert!(!before.state.is_terminal(), "{:?} left the terminal state {}", mv.signal, before.state);
        prop_assert_eq!(mv.from, before.state);
        prop_assert_eq!(mv.to, after.state);
        prop_assert!(
            in_guard_list(mv.signal, mv.from, mv.to, before.resume_point),
            "{:?} from {} to {} is not in the guard list",
            mv.signal,
            mv.from,
            mv.to
        );

        // Epochs and the attempt budget.
        if mv.signal == Signal::G2 {
            prop_assert_eq!(after.lease_epoch, before.lease_epoch + 1);
            prop_assert_eq!(after.attempts_used, before.attempts_used + 1);
        } else {
            prop_assert_eq!(after.lease_epoch, before.lease_epoch);
            prop_assert_eq!(after.attempts_used, before.attempts_used);
        }
        prop_assert!(after.attempts_used <= after.budget.max_attempts);
        if after.state == State::Ready {
            prop_assert!(after.attempts_used < after.budget.max_attempts, "ready with the budget spent");
        }

        // Done only through G7, or G5 then G6, after G4, on current passing evidence.
        let passing = report.is_some_and(|r| r.all_pass);
        match mv.signal {
            Signal::G4 => prop_assert!(passing, "G4 without current passing evidence"),
            Signal::G5 | Signal::G7 => {
                prop_assert!(self.verified && passing, "{:?} without G4 and evidence", mv.signal)
            }
            Signal::G6 => prop_assert!(self.verified && passing, "G6 without current passing evidence"),
            _ => {}
        }
        if after.state == State::Done {
            prop_assert!(matches!(mv.signal, Signal::G6 | Signal::G7), "done through {:?}", mv.signal);
        }

        // Unblock returns exactly to where the task stopped, with its work.
        match mv.signal {
            Signal::Block => {
                prop_assert_eq!(after.resume_point, Some(before.state));
                self.blocked = Some(Held {
                    state: before.state,
                    tree: before.current_tree.clone(),
                    epoch: before.lease_epoch,
                    attempts_used: before.attempts_used,
                });
            }
            Signal::Unblock => {
                let held = self.blocked.take().expect("a blocked task was blocked by a move");
                let back = Held {
                    state: after.state,
                    tree: after.current_tree.clone(),
                    epoch: after.lease_epoch,
                    attempts_used: after.attempts_used,
                };
                prop_assert_eq!(back, held);
                prop_assert_eq!(after.resume_point, None);
            }
            _ => {}
        }

        match mv.signal {
            Signal::G4 => self.verified = true,
            Signal::G2 | Signal::G3 | Signal::R1 | Signal::R2 | Signal::R3 => self.verified = false,
            _ => {}
        }
        self.signals.push(mv.signal);
        self.task = out.task;
        Ok(())
    }

    /// A send-back (R1 or R3) or, with the budget spent, a failure.
    fn check_send_back(&self, out: &Outcome, back: Signal) -> Result<(), TestCaseError> {
        if self.task.attempts_used < self.task.budget.max_attempts {
            prop_assert_eq!((out.mv.signal, out.task.state), (back, State::Ready));
        } else {
            prop_assert_eq!((out.mv.signal, out.task.state), (Signal::Fail, State::Failed));
        }
        Ok(())
    }

    /// The forward move for the task's state.
    fn progress(&self, tree: usize) -> Step {
        let report = self.report();
        match self.task.state {
            State::Pending => Step::Ready { upstream_done: true },
            State::Ready => Step::StartWorker { capable: true },
            State::Running => Step::Submit { late: None, epoch: EpochPick::Own, tree, in_scope: true },
            State::AwaitingVerification if report.all_pass || report.any_fail => Step::Advance,
            State::AwaitingVerification if self.verifiers.iter().any(|v| v.epoch == self.task.lease_epoch) => {
                Step::Verify { pass: true }
            }
            State::AwaitingVerification => Step::StartVerifier { capable: true },
            State::Verified if self.task.integration_required && report.all_pass => {
                Step::BeginIntegration { authority: LandingAuthority::Coordinator }
            }
            State::Verified => Step::Advance,
            State::Integrating if report.all_pass => Step::ConfirmIntegration { merge: Merge::Pinned { cut: 41 } },
            State::Integrating => Step::Advance,
            State::Blocked => Step::Unblock,
            State::Done | State::Failed | State::Cancelled => Step::Advance,
        }
    }

    fn step(&mut self, step: &Step) -> Result<(), TestCaseError> {
        let now = self.now();
        let terminal = self.task.state.is_terminal();
        match step {
            Step::Progress { tree } => {
                let next = self.progress(*tree);
                return self.step(&next);
            }
            Step::Ready { upstream_done } => {
                let upstream = if *upstream_done { State::Done } else { State::Running };
                let snapshot = Snapshot {
                    repository: "/work/repo".into(),
                    base_commit: "e43c7ee".into(),
                    untracked_hash: None,
                    protected_paths: vec![],
                };
                if let Ok(out) = lifecycle::ready(&self.task, &[(UPSTREAM.into(), upstream)], snapshot, now) {
                    prop_assert!(self.task.dependencies.is_empty() || *upstream_done, "G1 with a dependency not done");
                    self.apply(out, None)?;
                }
            }
            Step::StartWorker { capable } => {
                let grant = grant_for(&self.task, Role::Worker);
                let id = format!("w{}", self.workers.len() + 1);
                let caps = capabilities(*capable);
                match lifecycle::start_worker(&self.task, &bug_fix(), Mode::Headless, &caps, &grant, &id, now) {
                    Ok(Start::Allowed { epoch, outcome: Some(out), .. }) => {
                        prop_assert_eq!(epoch, self.task.lease_epoch + 1);
                        prop_assert_eq!(out.task.current_attempt.as_deref(), Some(id.as_str()));
                        self.apply(out, None)?;
                        self.workers.push(attempt(&self.task, &id, Role::Worker, epoch));
                    }
                    Ok(Start::Blocked(out)) => {
                        prop_assert!(!capable);
                        self.apply(out, None)?;
                    }
                    Ok(Start::Allowed { outcome: None, .. }) => prop_assert!(false, "a worker start moves the task"),
                    Err(r) => {
                        if r.code == RefusalCode::BudgetExhausted {
                            prop_assert_eq!(self.task.attempts_used, self.task.budget.max_attempts);
                        }
                    }
                }
            }
            Step::StartVerifier { capable } => {
                let grant = grant_for(&self.task, Role::Verifier);
                let caps = capabilities(*capable);
                match lifecycle::start_verifier(&self.task, &bug_fix(), Mode::Headless, &caps, &grant, now) {
                    Ok(Start::Allowed { epoch, outcome, .. }) => {
                        prop_assert!(outcome.is_none(), "a verifier opens without a move");
                        let id = format!("v{}", self.verifiers.len() + 1);
                        self.verifiers.push(attempt(&self.task, &id, Role::Verifier, epoch));
                    }
                    Ok(Start::Blocked(out)) => {
                        prop_assert!(!capable);
                        prop_assert_eq!(&out.task.current_tree, &self.task.current_tree, "the work is kept");
                        self.apply(out, None)?;
                    }
                    Err(_) => {}
                }
            }
            Step::Submit { late, epoch, tree, in_scope } => {
                let Some(current) = self.workers.len().checked_sub(1) else { return Ok(()) };
                let index = late.map_or(current, |back| current.saturating_sub(back + 1));
                let attempt = self.workers[index].clone();
                let epoch = match epoch {
                    EpochPick::Own => attempt.epoch,
                    EpochPick::Current => self.task.lease_epoch,
                    EpochPick::Older => self.task.lease_epoch.saturating_sub(1),
                    EpochPick::Newer => self.task.lease_epoch + 1,
                };
                let paths = [if *in_scope { "src/export/retry.rs" } else { "README.md" }.to_string()];
                let applies = self.task.current_attempt.as_deref() == Some(attempt.id.as_str())
                    && epoch == self.task.lease_epoch
                    && attempt.epoch == self.task.lease_epoch
                    && self.task.state == State::Running;
                match lifecycle::submit_result(&self.task, &attempt, epoch, TREES[*tree], &paths, now).unwrap() {
                    Submission::Accepted(out) => {
                        prop_assert!(applies && *in_scope, "a result from {} at epoch {epoch} applied", attempt.id);
                        prop_assert_eq!(out.task.current_tree.as_deref(), Some(TREES[*tree]));
                        self.apply(out, None)?;
                        self.workers[index].status = AttemptStatus::Submitted;
                    }
                    Submission::Superseded { .. } => prop_assert!(!applies, "a current result was superseded"),
                    Submission::Rejected { .. } => prop_assert!(applies && !in_scope),
                }
            }
            Step::Evidence { from_verifier, assessment, repro, strength, tree } => {
                let source = if *from_verifier { self.verifiers.last() } else { self.workers.last() };
                let Some(source) = source.cloned() else { return Ok(()) };
                let kind = if *assessment { EvidenceKind::Assessment } else { EvidenceKind::Claim };
                let criterion = if *repro { "repro" } else { "regression" };
                let allowed = lifecycle::check_evidence_source(&self.task, &source, kind, criterion);
                // The role comes from the attempt: workers never assess, verifiers never claim.
                if *from_verifier != *assessment {
                    let expected = if terminal { RefusalCode::Terminal } else { RefusalCode::WrongRole };
                    prop_assert_eq!(allowed.as_ref().map_err(|r| r.code), Err(expected));
                }
                if allowed.is_ok() {
                    let tree = tree.map_or(self.current_tree(), |i| TREES[i].to_string());
                    self.record(kind, &source, criterion, *strength, &tree);
                }
            }
            Step::Verify { pass } => {
                let Some(verifier) = self.verifiers.last().cloned() else { return Ok(()) };
                let tree = self.current_tree();
                for (criterion, strength) in [
                    ("repro", if *pass { Strength::Observed } else { Strength::Failed }),
                    ("regression", Strength::Tested),
                ] {
                    if lifecycle::check_evidence_source(&self.task, &verifier, EvidenceKind::Assessment, criterion)
                        .is_ok()
                    {
                        self.record(EvidenceKind::Assessment, &verifier, criterion, strength, &tree);
                    }
                }
            }
            Step::Claim => {
                let Some(worker) = self.workers.last().cloned() else { return Ok(()) };
                if lifecycle::check_evidence_source(&self.task, &worker, EvidenceKind::Claim, "regression").is_ok() {
                    let tree = self.current_tree();
                    self.record(EvidenceKind::Claim, &worker, "regression", Strength::Tested, &tree);
                }
            }
            Step::Advance => {
                let report = self.report();
                if let Some(out) = lifecycle::advance(&self.task, &report, now) {
                    if self.task.state == State::AwaitingVerification && report.any_fail {
                        self.check_send_back(&out, Signal::R1)?;
                    }
                    self.apply(out, Some(&report))?;
                }
            }
            Step::Retry => {
                if let Ok(out) = lifecycle::retry(&self.task, "the session timed out", now) {
                    self.check_send_back(&out, Signal::R3)?;
                    self.apply(out, None)?;
                }
            }
            Step::Block => {
                if let Ok(out) = lifecycle::block(&self.task, "an operator gate", now) {
                    self.apply(out, None)?;
                }
            }
            Step::Unblock => {
                if let Ok(out) = lifecycle::unblock(&self.task, now) {
                    self.apply(out, None)?;
                }
            }
            Step::Fail => {
                if let Ok(out) = lifecycle::fail(&self.task, "unrecoverable", now) {
                    self.apply(out, None)?;
                }
            }
            Step::Cancel => {
                if let Ok(out) = lifecycle::cancel(&self.task, "the operator cancelled", now) {
                    self.apply(out, None)?;
                }
            }
            Step::BeginIntegration { authority } => {
                let report = self.report();
                if let Ok(out) = lifecycle::begin_integration(&self.task, &report, *authority, now) {
                    let pinned = self.task.current_tree.clone();
                    let landing = out.mv.signal == Signal::G5;
                    prop_assert!(landing == (*authority != LandingAuthority::None));
                    self.apply(out, Some(&report))?;
                    if landing {
                        // The operation pins the head built from the verified tree.
                        self.operation = Some(Operation {
                            id: format!("op{}", self.clock),
                            task_id: self.task.id.clone(),
                            kind: OperationKind::Merge,
                            intent: OperationIntent {
                                expected_head_sha: pinned,
                                base: None,
                                pull_request: Some(7),
                                tree: None,
                            },
                            state: OperationState::Started,
                            outcome: None,
                            created_at: now,
                            updated_at: now,
                        });
                    }
                }
            }
            Step::ConfirmIntegration { merge } => {
                let Some(op) = self.operation.clone() else { return Ok(()) };
                let pinned = op.intent.expected_head_sha.clone().unwrap_or_default();
                let merged = match merge {
                    Merge::Pinned { cut } => {
                        MergeReport::Merged { head_sha: pinned[..(*cut).min(pinned.len())].into() }
                    }
                    Merge::Other => {
                        MergeReport::Merged { head_sha: "ddddddd4444444444444444444444444444444444".into() }
                    }
                    Merge::Refused => MergeReport::Refused { reason: "the head moved".into() },
                };
                let report = self.report();
                if let Ok(out) = lifecycle::confirm_integration(&self.task, &op, &merged, &report, now) {
                    if let (Signal::G6, MergeReport::Merged { head_sha }) = (out.mv.signal, &merged) {
                        // Only the pinned head lands, named by at least seven characters.
                        prop_assert!(head_sha.len() >= 7 && pinned.starts_with(head_sha.as_str()), "{head_sha} landed");
                    }
                    self.apply(out, Some(&report))?;
                }
            }
            Step::NewTree { tree } => {
                if let Ok(next) = lifecycle::record_new_tree(&self.task, TREES[*tree], now) {
                    prop_assert_eq!(next.state, self.task.state, "a new tree is not a move");
                    prop_assert_eq!(next.lease_epoch, self.task.lease_epoch);
                    self.task = next;
                }
            }
        }
        if terminal {
            prop_assert!(self.task.state.is_terminal());
            prop_assert!(lifecycle::next_moves(&self.task, &self.report()).is_empty());
        }
        Ok(())
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Every guard holds over every sequence of moves.
    #[test]
    fn the_guards_hold_over_any_sequence_of_moves((max_attempts, integration_required, upstream, steps) in case()) {
        let mut world = World::new(max_attempts, integration_required, upstream);
        for step in &steps {
            world.step(step)?;
        }
    }
}

/// The sequences above reach every signal: a property that never meets a
/// guard proves nothing about it.
#[test]
fn the_sequences_reach_every_signal() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let mut seen: std::collections::HashMap<Signal, usize> = std::collections::HashMap::new();
    for _ in 0..512 {
        let (max_attempts, integration_required, upstream, steps) = case().new_tree(&mut runner).unwrap().current();
        let mut world = World::new(max_attempts, integration_required, upstream);
        for step in &steps {
            world.step(step).unwrap();
        }
        for signal in world.signals {
            *seen.entry(signal).or_default() += 1;
        }
    }
    use Signal::*;
    for signal in [G1, G2, G3, G4, G5, G6, G7, R1, R2, R3, Block, Unblock, Fail, Cancel] {
        assert!(seen.get(&signal).is_some_and(|n| *n >= 5), "{signal:?} was met too rarely: {seen:?}");
    }
}
