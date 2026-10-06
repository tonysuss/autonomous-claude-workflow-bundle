//! Property tests for the lifecycle guards. Each case creates a task and
//! drives a random sequence of moves through the pure lifecycle API, as any
//! caller could: G1 to G7, R1 to R3, results from current and late attempts,
//! evidence from either role, block and unblock, fail, cancel, forge reports,
//! and new trees. Every call is checked against an oracle written from the
//! design's guard list: what the call must do in the task's state, given its
//! evidence, budget and integration, including the moves it must make (a
//! guard that stops firing fails as surely as one that fires wrongly). After
//! every move it also checks the design's invariants:
//!
//! - only the transitions in the guard list happen;
//! - the lease epoch never decreases, and a result with a stale epoch is never applied;
//! - done is reached only through G7, or G5 then G6, after G4, with current passing
//!   evidence, and G6 lands only the pinned head of the task's current tree;
//! - G7 never fires on a task that must integrate;
//! - a terminal state is never left;
//! - unblock returns exactly to the state the task left, with its work;
//! - attempts used never exceed the budget, an exhausted budget gives failed,
//!   not ready, and a ready task holds no attempt.
//!
//! Two checks cannot be reached this way, because no sequence of moves sets
//! up the state they guard against; each is tested alone in `lifecycle.rs`:
//!
//! - G2's refusal once the budget is spent. No move leaves a task ready with
//!   its budget spent (R1 and R3 fail the task instead, which the property
//!   checks): `g2_refuses_once_the_attempt_budget_is_spent`.
//! - G3's check that a result comes from the current attempt. Every G2 opens
//!   one attempt at a new epoch, so only an attempt made by hand shares the
//!   current epoch without being current; the epoch check refuses every
//!   other: `g3_supersedes_another_attempt_at_the_current_epoch`.

mod common;

use std::collections::HashMap;

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
    /// The forge merged the pinned head, reported by its first `cut`
    /// characters, in upper case if `upper`.
    Pinned {
        cut: usize,
        upper: bool,
    },
    /// The forge merged some other head.
    Other,
    Refused,
}

/// A way the evidence stops covering the task.
#[derive(Debug, Clone, Copy)]
enum Stale {
    /// A new tree is recorded, for example after a rebase.
    NewTree(usize),
    /// The latest verifier fails the reproduction on the current tree.
    Failed,
    /// A new tree is recorded and passing evidence is recorded for it.
    Rebuilt(usize),
}

#[derive(Debug, Clone, Copy)]
enum Then {
    Advance,
    Confirm(Merge),
    BeginIntegration,
}

#[derive(Debug, Clone)]
enum Step {
    /// Whatever moves the task forward from where it is, as a cooperative
    /// worker, verifier and operator would; the other steps perturb it.
    Progress {
        pick: usize,
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
    /// For a verified or integrating task, the evidence goes stale or fails,
    /// then a move that must notice; for any other, progress.
    Stale {
        how: Stale,
        then: Then,
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

fn merge() -> impl Strategy<Value = Merge> {
    prop_oneof![
        4 => (0usize..=41, prop::bool::weighted(0.3)).prop_map(|(cut, upper)| Merge::Pinned { cut, upper }),
        1 => Just(Merge::Other),
        2 => Just(Merge::Refused),
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
    let stale = prop_oneof![
        1 => (0usize..3).prop_map(Stale::NewTree),
        2 => Just(Stale::Failed),
        1 => (0usize..3).prop_map(Stale::Rebuilt),
    ];
    let then = prop_oneof![
        1 => Just(Then::Advance),
        3 => merge().prop_map(Then::Confirm),
        1 => Just(Then::BeginIntegration),
    ];
    // Weighted so that most tasks get deep enough to meet every guard before
    // a failure or cancellation ends them, and integrating tasks often see
    // their evidence go stale before a merge is confirmed.
    prop_oneof![
        60 => (0usize..4).prop_map(|pick| Step::Progress { pick }),
        10 => prop::bool::weighted(0.6).prop_map(|upstream_done| Step::Ready { upstream_done }),
        20 => prop::bool::weighted(0.85).prop_map(|capable| Step::StartWorker { capable }),
        15 => prop::bool::weighted(0.85).prop_map(|capable| Step::StartVerifier { capable }),
        25 => (prop::option::weighted(0.25, 0usize..3), epoch, 0usize..3, prop::bool::weighted(0.75))
            .prop_map(|(late, epoch, tree, in_scope)| Step::Submit { late, epoch, tree, in_scope }),
        6 => (any::<bool>(), any::<bool>(), any::<bool>(), strength(), prop::option::weighted(0.2, 0usize..3))
            .prop_map(|(from_verifier, assessment, repro, strength, tree)| {
                Step::Evidence { from_verifier, assessment, repro, strength, tree }
            }),
        20 => prop::bool::weighted(0.85).prop_map(|pass| Step::Verify { pass }),
        8 => Just(Step::Claim),
        30 => Just(Step::Advance),
        14 => Just(Step::Retry),
        2 => Just(Step::Block),
        10 => Just(Step::Unblock),
        1 => Just(Step::Fail),
        1 => Just(Step::Cancel),
        12 => authority.prop_map(|authority| Step::BeginIntegration { authority }),
        12 => merge().prop_map(|merge| Step::ConfirmIntegration { merge }),
        4 => (0usize..3).prop_map(|tree| Step::NewTree { tree }),
        60 => (stale, then).prop_map(|(how, then)| Step::Stale { how, then }),
    ]
}

/// One case: the first task's attempt budget, whether it integrates and
/// whether it waits on another task, then the moves. A few moves after a
/// task ends, the next task starts, with the next budget and the other
/// answers, so one case covers several tasks.
fn case() -> impl Strategy<Value = (u32, bool, bool, Vec<Step>)> {
    (1u32..=4, any::<bool>(), any::<bool>(), prop::collection::vec(step(), 1..100))
}

/// Moves tried on a task that has ended, before the next task starts.
const MOVES_AFTER_THE_END: usize = 3;

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

/// Whether two object ids name the same object, as the design means it: at
/// least seven hex digits each, one a prefix of the other, in either case.
fn same_object(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_ascii_lowercase(), b.to_ascii_lowercase());
    a.len() >= 7 && b.len() >= 7 && (a.starts_with(&b) || b.starts_with(&a))
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
    /// The landing operation of the last G5.
    operation: Option<Operation>,
    blocked: Option<Held>,
    /// G4 has cleared since the last result or send-back.
    verified: bool,
    clock: i64,
    /// The task's attempt budget, integration and dependency, for the next task.
    config: (u32, bool, bool),
    /// Moves tried since the task ended.
    after_end: usize,
    /// How often each guard branch was met, for checking that the sequences reach them all.
    branches: HashMap<&'static str, usize>,
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
            config: (max_attempts, integration_required, upstream),
            after_end: 0,
            branches: HashMap::new(),
        }
    }

    /// Starts the next task once this one has ended and had its last moves
    /// tried: the next budget, and the other integration and dependency.
    fn next_task_if_ended(&mut self) {
        if !self.task.state.is_terminal() {
            return;
        }
        self.after_end += 1;
        if self.after_end > MOVES_AFTER_THE_END {
            // The next budget, and the next of the four answers to "integrates?" and "waits?".
            let (max_attempts, integration_required, upstream) = self.config;
            let branches = std::mem::take(&mut self.branches);
            let clock = self.clock;
            *self = World::new(max_attempts % 4 + 1, !integration_required, upstream != integration_required);
            self.branches = branches;
            self.clock = clock;
        }
    }

    /// Runs a case's moves.
    fn run(&mut self, steps: &[Step]) -> Result<(), TestCaseError> {
        for step in steps {
            self.step(step)?;
            self.next_task_if_ended();
        }
        Ok(())
    }

    fn now(&mut self) -> Timestamp {
        self.clock += 1;
        t(self.clock)
    }

    fn hit(&mut self, branch: &'static str) {
        *self.branches.entry(branch).or_default() += 1;
    }

    fn report(&self) -> EvidenceReport {
        eval(&self.task, &self.claims, &self.assessments)
    }

    fn current_tree(&self) -> String {
        self.task.current_tree.clone().unwrap_or_else(|| TREE_A.to_string())
    }

    fn budget_left(&self) -> bool {
        self.task.attempts_used < self.task.budget.max_attempts
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
            prop_assert_eq!(&after.current_attempt, &None, "a ready task holds no attempt");
        }

        // Done only through G7, or G5 then G6, after G4, on current passing evidence.
        let passing = report.is_some_and(|r| r.all_pass);
        match mv.signal {
            Signal::G4 => prop_assert!(passing, "G4 without current passing evidence"),
            Signal::G5 => prop_assert!(self.verified && passing, "G5 without G4 and evidence"),
            Signal::G7 => {
                prop_assert!(self.verified && passing, "G7 without G4 and evidence");
                prop_assert!(!before.integration_required, "G7 on a task that must integrate");
            }
            Signal::G6 => {
                prop_assert!(self.verified && passing, "G6 without current passing evidence");
                let pinned = self.operation.as_ref().and_then(|op| op.intent.tree.clone()).unwrap_or_default();
                let current = after.current_tree.clone().unwrap_or_default();
                prop_assert!(same_object(&pinned, &current), "G6 landed {pinned}, but the task's tree is {current}");
            }
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
        self.task = out.task;
        Ok(())
    }

    /// Asserts a move the oracle expects and applies it.
    fn expect(
        &mut self,
        got: Option<Outcome>,
        want: Option<(Signal, State)>,
        report: Option<&EvidenceReport>,
    ) -> Result<(), TestCaseError> {
        let seen = got.as_ref().map(|o| (o.mv.signal, o.task.state));
        prop_assert_eq!(seen, want, "in {} the guard list wants {:?}", self.task.state, want);
        if let Some(out) = got {
            self.apply(out, report)?;
        }
        Ok(())
    }

    /// The forward move for the task's state.
    fn progress(&self, pick: usize) -> Step {
        let report = self.report();
        match self.task.state {
            State::Pending => Step::Ready { upstream_done: pick > 0 },
            State::Ready => Step::StartWorker { capable: true },
            State::Running => Step::Submit { late: None, epoch: EpochPick::Own, tree: pick % 3, in_scope: true },
            State::AwaitingVerification if report.all_pass || report.any_fail => Step::Advance,
            State::AwaitingVerification if self.verifiers.iter().any(|v| v.epoch == self.task.lease_epoch) => {
                Step::Verify { pass: true }
            }
            State::AwaitingVerification => Step::StartVerifier { capable: true },
            // A task that must integrate tries G7 now and then, which must not
            // fire, and sometimes asks to land with no authority, which blocks.
            State::Verified if self.task.integration_required && report.all_pass && pick > 0 => {
                let authority = if pick == 1 { LandingAuthority::None } else { LandingAuthority::Coordinator };
                Step::BeginIntegration { authority }
            }
            State::Verified => Step::Advance,
            // An integrating task waits a while for the forge, as a real one does.
            State::Integrating if report.all_pass && pick > 1 => {
                Step::ConfirmIntegration { merge: Merge::Pinned { cut: 41, upper: false } }
            }
            State::Integrating => Step::Advance,
            State::Blocked => Step::Unblock,
            State::Done | State::Failed | State::Cancelled => Step::Advance,
        }
    }

    fn step(&mut self, step: &Step) -> Result<(), TestCaseError> {
        let now = self.now();
        let before = self.task.clone();
        let terminal = before.state.is_terminal();
        if terminal {
            self.hit("a move on a terminal task");
        }
        match step {
            Step::Progress { pick } => {
                let next = self.progress(*pick);
                return self.step(&next);
            }
            // Staleness matters once a task is verified; before that, the task moves on.
            Step::Stale { .. } if !matches!(before.state, State::Verified | State::Integrating) => {
                return self.step(&self.progress(3));
            }
            Step::Stale { how, then } => {
                match how {
                    Stale::NewTree(tree) => self.step(&Step::NewTree { tree: *tree })?,
                    Stale::Failed => self.step(&Step::Verify { pass: false })?,
                    Stale::Rebuilt(tree) => {
                        self.step(&Step::NewTree { tree: *tree })?;
                        self.step(&Step::Verify { pass: true })?;
                        self.step(&Step::Claim)?;
                    }
                }
                return match then {
                    Then::Advance => self.step(&Step::Advance),
                    Then::Confirm(merge) => self.step(&Step::ConfirmIntegration { merge: *merge }),
                    Then::BeginIntegration => {
                        self.step(&Step::BeginIntegration { authority: LandingAuthority::Coordinator })
                    }
                };
            }
            Step::Ready { upstream_done } => {
                let upstream = if *upstream_done { State::Done } else { State::Running };
                let snapshot = Snapshot {
                    repository: "/work/repo".into(),
                    base_commit: "e43c7ee".into(),
                    untracked_hash: None,
                    protected_paths: vec![],
                };
                let deps_done = before.dependencies.is_empty() || *upstream_done;
                let got = lifecycle::ready(&before, &[(UPSTREAM.into(), upstream)], snapshot, now);
                if before.state == State::Pending && !deps_done {
                    self.hit("G1 refused: a dependency is not done");
                    prop_assert_eq!(got.as_ref().map_err(|r| r.code).err(), Some(RefusalCode::DependenciesNotDone));
                }
                let want = (before.state == State::Pending && deps_done).then_some((Signal::G1, State::Ready));
                if want.is_some() {
                    self.hit("G1");
                }
                self.expect(got.ok(), want, None)?;
            }
            Step::StartWorker { capable } => {
                let grant = grant_for(&before, Role::Worker);
                let id = format!("w{}", self.workers.len() + 1);
                let caps = capabilities(*capable);
                let ready = before.state == State::Ready;
                match lifecycle::start_worker(&before, &bug_fix(), Mode::Headless, &caps, &grant, &id, now) {
                    Ok(Start::Allowed { epoch, outcome: Some(out), .. }) => {
                        prop_assert!(ready && *capable && self.budget_left(), "G2 from {}", before.state);
                        prop_assert_eq!(epoch, before.lease_epoch + 1);
                        prop_assert_eq!(out.task.current_attempt.as_deref(), Some(id.as_str()));
                        self.hit("G2");
                        self.apply(out, None)?;
                        self.workers.push(attempt(&self.task, &id, Role::Worker, epoch));
                    }
                    Ok(Start::Blocked(out)) => {
                        prop_assert!(ready && !capable);
                        prop_assert_eq!(out.task.resume_point, Some(State::Ready));
                        self.hit("G2 blocked: a capability is missing");
                        self.apply(out, None)?;
                    }
                    Ok(Start::Allowed { outcome: None, .. }) => prop_assert!(false, "a worker start moves the task"),
                    Err(r) => prop_assert!(!ready, "G2 refused from ready: {r}"),
                }
            }
            Step::StartVerifier { capable } => {
                let grant = grant_for(&before, Role::Verifier);
                let caps = capabilities(*capable);
                let awaiting = before.state == State::AwaitingVerification;
                match lifecycle::start_verifier(&before, &bug_fix(), Mode::Headless, &caps, &grant, now) {
                    Ok(Start::Allowed { epoch, outcome, .. }) => {
                        prop_assert!(awaiting && *capable && outcome.is_none());
                        prop_assert_eq!(epoch, before.lease_epoch);
                        let id = format!("v{}", self.verifiers.len() + 1);
                        self.hit("verifier opened");
                        self.verifiers.push(attempt(&self.task, &id, Role::Verifier, epoch));
                    }
                    Ok(Start::Blocked(out)) => {
                        prop_assert!(awaiting && !capable);
                        prop_assert_eq!(out.task.resume_point, Some(State::AwaitingVerification));
                        prop_assert_eq!(&out.task.current_tree, &before.current_tree, "the work is kept");
                        self.hit("verifier blocked: no independent verifier");
                        self.apply(out, None)?;
                    }
                    Err(r) => prop_assert!(!awaiting, "a verifier refused while awaiting verification: {r}"),
                }
            }
            Step::Submit { late, epoch, tree, in_scope } => {
                let Some(current) = self.workers.len().checked_sub(1) else { return Ok(()) };
                let index = late.map_or(current, |back| current.saturating_sub(back + 1));
                let attempt = self.workers[index].clone();
                let epoch = match epoch {
                    EpochPick::Own => attempt.epoch,
                    EpochPick::Current => before.lease_epoch,
                    EpochPick::Older => before.lease_epoch.saturating_sub(1),
                    EpochPick::Newer => before.lease_epoch + 1,
                };
                let paths = [if *in_scope { "src/export/retry.rs" } else { "README.md" }.to_string()];
                let current_attempt = before.current_attempt.as_deref() == Some(attempt.id.as_str());
                let current_epoch = epoch == before.lease_epoch && attempt.epoch == before.lease_epoch;
                let applies = current_attempt && current_epoch && before.state == State::Running;
                match lifecycle::submit_result(&before, &attempt, epoch, TREES[*tree], &paths, now).unwrap() {
                    Submission::Accepted(out) => {
                        prop_assert!(applies && *in_scope, "a result from {} at epoch {epoch} applied", attempt.id);
                        prop_assert_eq!(out.task.current_tree.as_deref(), Some(TREES[*tree]));
                        self.hit("G3");
                        self.expect(Some(out), Some((Signal::G3, State::AwaitingVerification)), None)?;
                        self.workers[index].status = AttemptStatus::Submitted;
                    }
                    Submission::Superseded { .. } => {
                        prop_assert!(!applies, "a current result was superseded");
                        self.hit(match () {
                            _ if !current_attempt => "G3 superseded: a late attempt",
                            _ if !current_epoch => "G3 superseded: a stale epoch",
                            _ => "G3 superseded: the task is not running",
                        });
                    }
                    Submission::Rejected { .. } => {
                        prop_assert!(applies && !in_scope);
                        self.hit("G3 rejected: out of scope");
                    }
                }
            }
            Step::Evidence { from_verifier, assessment, repro, strength, tree } => {
                let source = if *from_verifier { self.verifiers.last() } else { self.workers.last() };
                let Some(source) = source.cloned() else { return Ok(()) };
                let kind = if *assessment { EvidenceKind::Assessment } else { EvidenceKind::Claim };
                let criterion = if *repro { "repro" } else { "regression" };
                let allowed = lifecycle::check_evidence_source(&before, &source, kind, criterion);
                // The role comes from the attempt: workers never assess, verifiers never claim.
                if *from_verifier != *assessment {
                    let expected = if terminal { RefusalCode::Terminal } else { RefusalCode::WrongRole };
                    prop_assert_eq!(allowed.as_ref().map_err(|r| r.code), Err(expected));
                    self.hit("evidence refused: the wrong role");
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
                    if lifecycle::check_evidence_source(&before, &verifier, EvidenceKind::Assessment, criterion).is_ok()
                    {
                        self.record(EvidenceKind::Assessment, &verifier, criterion, strength, &tree);
                    }
                }
            }
            Step::Claim => {
                let Some(worker) = self.workers.last().cloned() else { return Ok(()) };
                if lifecycle::check_evidence_source(&before, &worker, EvidenceKind::Claim, "regression").is_ok() {
                    let tree = self.current_tree();
                    self.record(EvidenceKind::Claim, &worker, "regression", Strength::Tested, &tree);
                }
            }
            Step::Advance => {
                let report = self.report();
                let (want, branch) = match before.state {
                    State::AwaitingVerification if report.any_fail && self.budget_left() => {
                        (Some((Signal::R1, State::Ready)), "R1")
                    }
                    State::AwaitingVerification if report.any_fail => {
                        (Some((Signal::Fail, State::Failed)), "R1 with the budget spent: failed")
                    }
                    State::AwaitingVerification if report.all_pass => (Some((Signal::G4, State::Verified)), "G4"),
                    State::AwaitingVerification => (None, "G4 waits for evidence"),
                    State::Verified if !report.all_pass => {
                        (Some((Signal::R2, State::AwaitingVerification)), "R2 from verified")
                    }
                    State::Verified if before.integration_required => (None, "G7 withheld: the task must integrate"),
                    State::Verified => (Some((Signal::G7, State::Done)), "G7"),
                    State::Integrating if !report.all_pass => {
                        (Some((Signal::R2, State::AwaitingVerification)), "R2 from integrating: evidence stale")
                    }
                    _ => (None, "nothing to advance"),
                };
                self.hit(branch);
                let got = lifecycle::advance(&before, &report, now);
                self.expect(got, want, Some(&report))?;
            }
            Step::Retry => {
                let want = match before.state {
                    State::Running if self.budget_left() => Some((Signal::R3, State::Ready)),
                    State::Running => Some((Signal::Fail, State::Failed)),
                    _ => None,
                };
                if before.state == State::Running {
                    self.hit(if self.budget_left() { "R3" } else { "R3 with the budget spent: failed" });
                }
                let got = lifecycle::retry(&before, "the session timed out", now);
                self.expect(got.ok(), want, None)?;
            }
            Step::Block => {
                let want = (!terminal && before.state != State::Blocked).then_some((Signal::Block, State::Blocked));
                if want.is_some() {
                    self.hit("block");
                }
                self.expect(lifecycle::block(&before, "an operator gate", now).ok(), want, None)?;
            }
            Step::Unblock => {
                let want = (before.state == State::Blocked)
                    .then(|| (Signal::Unblock, self.blocked.as_ref().map_or(State::Pending, |h| h.state)));
                if want.is_some() {
                    self.hit("unblock");
                }
                self.expect(lifecycle::unblock(&before, now).ok(), want, None)?;
            }
            Step::Fail => {
                let want = (!terminal).then_some((Signal::Fail, State::Failed));
                self.expect(lifecycle::fail(&before, "unrecoverable", now).ok(), want, None)?;
            }
            Step::Cancel => {
                let want = (!terminal).then_some((Signal::Cancel, State::Cancelled));
                self.expect(lifecycle::cancel(&before, "the operator cancelled", now).ok(), want, None)?;
            }
            Step::BeginIntegration { authority } => {
                let report = self.report();
                let may = before.state == State::Verified && before.integration_required;
                let (want, branch) = match () {
                    _ if may && !report.all_pass => (None, "G5 refused: the evidence is not current"),
                    _ if may && *authority == LandingAuthority::None => {
                        (Some((Signal::Block, State::Blocked)), "G5 blocked: no landing authority")
                    }
                    _ if may => (Some((Signal::G5, State::Integrating)), "G5"),
                    _ => (None, "G5 refused: not a verified task that integrates"),
                };
                self.hit(branch);
                let got = lifecycle::begin_integration(&before, &report, *authority, now).ok();
                let landing = got.as_ref().is_some_and(|o| o.mv.signal == Signal::G5);
                self.expect(got, want, Some(&report))?;
                if landing {
                    // The operation pins the head built from the verified tree, and that tree.
                    let pinned = before.current_tree.clone();
                    self.operation = Some(Operation {
                        id: format!("op{}", self.clock),
                        task_id: self.task.id.clone(),
                        kind: OperationKind::Merge,
                        intent: OperationIntent {
                            expected_head_sha: pinned.clone(),
                            base: None,
                            pull_request: Some(7),
                            tree: pinned,
                        },
                        state: OperationState::Started,
                        outcome: None,
                        created_at: now,
                        updated_at: now,
                    });
                }
            }
            Step::ConfirmIntegration { merge } => {
                // The last G5's operation; before any, one pinned to the task's tree as it is.
                let op = self.operation.clone().unwrap_or_else(|| Operation {
                    id: "op-unplanned".into(),
                    task_id: self.task.id.clone(),
                    kind: OperationKind::Merge,
                    intent: OperationIntent {
                        expected_head_sha: Some(self.current_tree()),
                        base: None,
                        pull_request: Some(7),
                        tree: Some(self.current_tree()),
                    },
                    state: OperationState::Started,
                    outcome: None,
                    created_at: now,
                    updated_at: now,
                });
                let pinned = op.intent.expected_head_sha.clone().unwrap_or_default();
                let pinned_tree = op.intent.tree.clone().unwrap_or_default();
                let merged = match merge {
                    Merge::Pinned { cut, upper } => {
                        let head = &pinned[..(*cut).min(pinned.len())];
                        MergeReport::Merged { head_sha: if *upper { head.to_uppercase() } else { head.to_string() } }
                    }
                    Merge::Other => {
                        MergeReport::Merged { head_sha: "ddddddd4444444444444444444444444444444444".into() }
                    }
                    Merge::Refused => MergeReport::Refused { reason: "the head moved".into() },
                };
                let report = self.report();
                let current = self.current_tree();
                let (want, code, branch) = match &merged {
                    _ if before.state != State::Integrating => (None, None, "G6 refused: the task is not integrating"),
                    MergeReport::Refused { .. } => {
                        (Some((Signal::R2, State::AwaitingVerification)), None, "R2 from integrating: merge refused")
                    }
                    MergeReport::Merged { head_sha } if !same_object(head_sha, &pinned) => {
                        (None, Some(RefusalCode::HeadMismatch), "G6 refused: not the pinned head")
                    }
                    MergeReport::Merged { .. } if !same_object(&pinned_tree, &current) => {
                        (Some((Signal::Block, State::Blocked)), None, "G6 blocked: the tree changed")
                    }
                    MergeReport::Merged { .. } if !report.all_pass => {
                        (Some((Signal::Block, State::Blocked)), None, "G6 blocked: the evidence is not current")
                    }
                    MergeReport::Merged { .. } => (Some((Signal::G6, State::Done)), None, "G6"),
                };
                self.hit(branch);
                let got = lifecycle::confirm_integration(&before, &op, &merged, &report, now);
                if let Some(code) = code {
                    prop_assert_eq!(got.as_ref().map_err(|r| r.code).err(), Some(code));
                }
                self.expect(got.ok(), want, Some(&report))?;
            }
            Step::NewTree { tree } => {
                let may = matches!(before.state, State::AwaitingVerification | State::Verified | State::Integrating);
                match lifecycle::record_new_tree(&before, TREES[*tree], now) {
                    Ok(next) => {
                        prop_assert!(may, "a new tree recorded while {}", before.state);
                        prop_assert_eq!(next.state, before.state, "a new tree is not a move");
                        prop_assert_eq!(next.lease_epoch, before.lease_epoch);
                        self.task = next;
                    }
                    Err(_) => prop_assert!(!may, "a new tree refused while {}", before.state),
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
        World::new(max_attempts, integration_required, upstream).run(&steps)?;
    }
}

/// The sequences above meet every branch of every guard, often: a property
/// that never meets a branch proves nothing about it.
#[test]
fn the_sequences_reach_every_branch_of_every_guard() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let mut seen: HashMap<&'static str, usize> = HashMap::new();
    for _ in 0..512 {
        let (max_attempts, integration_required, upstream, steps) = case().new_tree(&mut runner).unwrap().current();
        let mut world = World::new(max_attempts, integration_required, upstream);
        world.run(&steps).unwrap();
        for (branch, n) in world.branches {
            *seen.entry(branch).or_default() += n;
        }
    }
    let mut counts: Vec<(&&str, &usize)> = seen.iter().collect();
    counts.sort_by_key(|(branch, n)| (**n, **branch));
    eprintln!("branches met over 512 sequences: {counts:#?}");
    for branch in [
        "G1",
        "G1 refused: a dependency is not done",
        "G2",
        "G2 blocked: a capability is missing",
        "verifier opened",
        "verifier blocked: no independent verifier",
        "G3",
        "G3 rejected: out of scope",
        "G3 superseded: a stale epoch",
        "G3 superseded: a late attempt",
        "G3 superseded: the task is not running",
        "evidence refused: the wrong role",
        "G4",
        "G4 waits for evidence",
        "R1",
        "R1 with the budget spent: failed",
        "R2 from verified",
        "R2 from integrating: evidence stale",
        "R2 from integrating: merge refused",
        "R3",
        "R3 with the budget spent: failed",
        "G5",
        "G5 blocked: no landing authority",
        "G5 refused: the evidence is not current",
        "G5 refused: not a verified task that integrates",
        "G6",
        "G6 refused: not the pinned head",
        "G6 refused: the task is not integrating",
        "G6 blocked: the tree changed",
        "G6 blocked: the evidence is not current",
        "G7",
        "G7 withheld: the task must integrate",
        "block",
        "unblock",
        "a move on a terminal task",
    ] {
        assert!(seen.get(branch).is_some_and(|n| *n >= 20), "{branch:?} was met too rarely: {seen:#?}");
    }
}
