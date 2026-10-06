//! Delivery of a verified task: G5, push the verified head, open the pull
//! request, wait until it is ready, merge it pinned to the verified head, and
//! G6. Each forge call that changes something gets an operation row first,
//! committed planned and then started, and is settled from what the forge
//! shows afterwards rather than from what the call returned. `reconcile`
//! does the same for operations a crashed controller left open.
//!
//! Landing authority decides who merges: with `coordinator` or `owner`,
//! interlock merges; with `operator`, interlock opens the pull request
//! pinned at the verified head and waits for the operator's merge, which
//! reconcile then checks like any other.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use interlock_core::delivery::{
    self, Observation, PrState, PullRequest, Verdict, branch_for, interlock_merges, lands, same_sha,
};
use interlock_core::lifecycle::{MergeReport, Move};
use interlock_schema::{LandingAuthority, Operation, OperationIntent, OperationKind, OperationState, State, Task};
use interlock_store::{Pin, Settled, Store, StoreError};
use serde::Serialize;
use serde_json::json;

use crate::{Forge, ForgeError, MergeMethod, NewPr, PrView, Readiness, forge_var, git, readiness};

#[derive(Debug, thiserror::Error)]
pub enum DeliverError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Git(#[from] git::GitError),
}

pub type Result<T> = std::result::Result<T, DeliverError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliverConfig {
    /// The branch to merge into. `None`: the forge's default branch.
    pub base: Option<String>,
    pub method: MergeMethod,
    /// Arm auto-merge, pinned to the verified head, instead of waiting for checks.
    pub auto_merge: bool,
    /// How long to wait for checks and mergeability before stopping.
    pub wait: Duration,
    pub poll: Duration,
    /// A pull request that reports no checks at all is not ready until this
    /// long after the push, so CI has time to report.
    pub checks_settle: Duration,
}

impl Default for DeliverConfig {
    fn default() -> DeliverConfig {
        DeliverConfig {
            base: None,
            method: MergeMethod::Merge,
            auto_merge: false,
            wait: Duration::from_secs(15 * 60),
            poll: Duration::from_secs(15),
            checks_settle: Duration::from_secs(30),
        }
    }
}

/// `90s`, `15m`, `1h`, or `0`.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s == "0" {
        return Some(Duration::ZERO);
    }
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: u64 = num.parse().ok()?;
    match unit {
        "s" => Some(Duration::from_secs(n)),
        "m" => Some(Duration::from_secs(n * 60)),
        "h" => Some(Duration::from_secs(n * 3600)),
        _ => None,
    }
}

impl DeliverConfig {
    /// Reads INTERLOCK_FORGE_BASE, INTERLOCK_FORGE_METHOD, INTERLOCK_FORGE_AUTO_MERGE,
    /// INTERLOCK_FORGE_WAIT, INTERLOCK_FORGE_POLL and INTERLOCK_FORGE_CHECKS_SETTLE
    /// over the defaults. Inside an attempt they are all ignored.
    pub fn from_env() -> std::result::Result<DeliverConfig, String> {
        let mut cfg = DeliverConfig { base: forge_var("INTERLOCK_FORGE_BASE"), ..DeliverConfig::default() };
        if let Some(m) = forge_var("INTERLOCK_FORGE_METHOD") {
            cfg.method =
                MergeMethod::parse(&m).ok_or(format!("INTERLOCK_FORGE_METHOD: {m} is not merge, squash or rebase"))?;
        }
        if let Some(a) = forge_var("INTERLOCK_FORGE_AUTO_MERGE") {
            cfg.auto_merge = matches!(a.as_str(), "1" | "true" | "yes");
        }
        for (key, slot) in [
            ("INTERLOCK_FORGE_WAIT", &mut cfg.wait),
            ("INTERLOCK_FORGE_POLL", &mut cfg.poll),
            ("INTERLOCK_FORGE_CHECKS_SETTLE", &mut cfg.checks_settle),
        ] {
            if let Some(v) = forge_var(key) {
                *slot = parse_duration(&v).ok_or(format!("{key}: {v} is not a duration such as 90s or 15m"))?;
            }
        }
        Ok(cfg)
    }
}

/// What one delivery pass did.
#[derive(Debug, Clone, Serialize)]
pub struct Delivery {
    pub task_id: String,
    pub final_state: State,
    pub stopped_because: String,
    /// The verified head: the only commit interlock will merge.
    pub head: Option<String>,
    pub pull_request: Option<u64>,
    pub moves: Vec<Move>,
    /// Operations left open by an earlier pass, settled before this one started.
    pub reconciled: Vec<Reconciled>,
    pub steps: Vec<String>,
}

/// One operation a reconcile settled.
#[derive(Debug, Clone, Serialize)]
pub struct Reconciled {
    pub operation: String,
    pub task_id: String,
    pub kind: OperationKind,
    pub before: OperationState,
    pub after: OperationState,
    pub verdict: Verdict,
    pub moves: Vec<Move>,
}

/// Where interlock fetches a base branch to see what a merge landed on.
fn landed_ref(task_id: &str) -> String {
    format!("{}/landed", git::ref_prefix(task_id))
}

/// The forge's view of a pull request, plus, for a merged one, where git
/// sees the merge on the base branch.
fn enrich(forge: &dyn Forge, task_id: &str, pr: &PrView) -> PullRequest {
    let mut seen = pr.observed();
    if pr.state == PrState::Merged {
        seen.landed =
            pr.merge_commit.as_deref().and_then(|m| forge.landed(m, &pr.base_branch, &landed_ref(task_id)).ok());
    }
    seen
}

fn observe(forge: &dyn Forge, op: &Operation) -> Observation {
    let seen = match (op.kind, op.intent.pull_request) {
        (OperationKind::OpenPr, _) | (_, None) => forge.find_pr(&branch_for(&op.task_id)),
        (_, Some(n)) => forge.view_pr(n).map(Some),
    };
    match seen {
        Ok(Some(pr)) => Observation::Pr(enrich(forge, &op.task_id, &pr)),
        Ok(None) => Observation::NoPr,
        Err(e) => Observation::Unreachable(e.to_string()),
    }
}

fn reconciled(op: &Operation, verdict: Verdict, settled: Settled) -> Reconciled {
    Reconciled {
        operation: op.id.clone(),
        task_id: op.task_id.clone(),
        kind: op.kind,
        before: op.state,
        after: settled.operation.state,
        verdict,
        moves: settled.moves,
    }
}

/// Asks the forge about every operation nobody confirmed (started or
/// unknown), and every planned landing tied to a pull request the operator
/// may have merged, and settles each from what it shows. A merge of the
/// pinned head, into the pinned branch, onto the verified base, with the
/// verified tree, is G6; a refusal or a moved head is R2; anything else that
/// landed blocks the task with the reason; no answer leaves the operation
/// unknown and the task blocked. An armed auto-merge whose landing authority
/// has ended is disarmed first. Refs of finished tasks are deleted.
pub fn reconcile(store: &mut Store, repo: &Path, forge: &dyn Forge, task_id: Option<&str>) -> Result<Vec<Reconciled>> {
    let mut out = Vec::new();
    let mut ops = store.open_operations(task_id)?;
    ops.extend(store.pinned_landings(task_id)?);
    for op in ops {
        out.extend(settle_one(store, forge, &op)?);
    }
    forget_finished(store, repo);
    Ok(out)
}

fn settle_one(store: &mut Store, forge: &dyn Forge, op: &Operation) -> Result<Vec<Reconciled>> {
    let task = store.task(&op.task_id)?;
    let armed = op.kind == OperationKind::ArmAutoMerge
        && matches!(op.state, OperationState::Started | OperationState::Unknown)
        && task.state == State::Integrating;
    if armed && !interlock_merges(store.landing_authority(&task.id, Utc::now())?) {
        return disarm(store, forge, op, &task);
    }
    let seen = observe(forge, op);
    let verdict = delivery::reconcile(op, &seen, &task);
    if op.state == OperationState::Planned && matches!(verdict, Verdict::Pending { .. }) {
        return Ok(vec![]);
    }
    let settled =
        store.settle_operation(&op.id, &verdict, json!({ "reconciled": true, "observation": seen }), Utc::now())?;
    Ok(vec![reconciled(op, verdict, settled)])
}

/// Landing authority ended while auto-merge was armed: turns it off, under
/// its own operation, and blocks the task with the reason. A merge the forge
/// already made is settled as seen; G6 then still needs authority at the
/// time of the merge.
fn disarm(store: &mut Store, forge: &dyn Forge, arm: &Operation, task: &Task) -> Result<Vec<Reconciled>> {
    let seen = observe(forge, arm);
    let still_armed = matches!(&seen, Observation::Pr(pr) if pr.state == PrState::Open && pr.auto_merge);
    if !still_armed {
        let verdict = delivery::reconcile(arm, &seen, task);
        let settled = store.settle_operation(&arm.id, &verdict, json!({ "observation": seen }), Utc::now())?;
        return Ok(vec![reconciled(arm, verdict, settled)]);
    }
    let n = arm.intent.pull_request.unwrap_or_default();
    let intent = OperationIntent { pull_request: arm.intent.pull_request, ..arm.intent.clone() };
    let planned = store.plan_operation(&task.id, OperationKind::DisarmAutoMerge, intent, Utc::now())?;
    let op = match store.start_operation(&planned.id, &Pin::default(), Utc::now())? {
        Ok(op) => op,
        Err(_) => return Ok(vec![]),
    };
    let call = forge.disarm(n);
    let after = observe(forge, &op);
    let verdict = delivery::reconcile(&op, &after, task);
    let call_said = match &call {
        Ok(()) => json!("ok"),
        Err(e) => json!(e.to_string()),
    };
    let settled =
        store.settle_operation(&op.id, &verdict, json!({ "call": call_said, "observation": after }), Utc::now())?;
    let mut out = vec![reconciled(&op, verdict.clone(), settled)];
    let why = format!("landing authority ended while auto-merge was armed on pull request #{n}");
    let arm_verdict = match (&verdict, &after) {
        (Verdict::Confirmed, _) => Verdict::block(format!("{why}; interlock disarmed it")),
        (_, Observation::Pr(pr)) if pr.state == PrState::Merged => delivery::reconcile(arm, &after, task),
        _ => Verdict::Unknown { reason: format!("{why}, and disarming it did not take effect") },
    };
    let settled = store.settle_operation(&arm.id, &arm_verdict, json!({ "observation": after }), Utc::now())?;
    out.push(reconciled(arm, arm_verdict, settled));
    Ok(out)
}

/// Deletes the refs interlock kept for tasks that are done, failed or cancelled.
fn forget_finished(store: &Store, repo: &Path) {
    for task in store.tasks().unwrap_or_default().iter().filter(|t| t.state.is_terminal()) {
        let _ = git::forget(repo, &task.id);
    }
}

/// Errors with which the forge says plainly that it will not merge.
fn refused_by_forge(e: &ForgeError) -> bool {
    const REFUSALS: &[&str] = &[
        "is not mergeable",
        "Head branch was modified",
        "Base branch was modified",
        "is closed",
        "Required status check",
        "review is required",
        "Pull request is in clean status",
    ];
    matches!(e, ForgeError::Failed { message, .. } if REFUSALS.iter().any(|r| message.contains(r)))
}

/// Delivers a verified task: G5, then push, pull request, readiness, the
/// pinned merge and G6. Resumes an integrating task where it stopped, after
/// reconciling its open operations. Stops, with the reason, when the forge
/// is not ready within `cfg.wait`, when the operator is to merge, when the
/// task is sent back or blocked, or when it is done.
pub fn integrate(
    store: &mut Store,
    repo: &Path,
    forge: &dyn Forge,
    cfg: &DeliverConfig,
    task_id: &str,
    cancel: &AtomicBool,
) -> Result<Delivery> {
    let reconciled = reconcile(store, repo, forge, Some(task_id))?;
    let mut pass = Pass {
        store,
        repo,
        forge,
        cfg,
        cancel,
        d: Delivery {
            task_id: task_id.to_string(),
            final_state: State::Pending,
            stopped_because: String::new(),
            head: None,
            pull_request: None,
            moves: reconciled.iter().flat_map(|r| r.moves.clone()).collect(),
            reconciled,
            steps: vec![],
        },
    };
    pass.d.stopped_because = pass.drive(task_id)?;
    pass.d.final_state = pass.store.task(task_id)?.state;
    forget_finished(pass.store, repo);
    Ok(pass.d)
}

enum Flow {
    /// A move was made or a step finished; read the task again.
    Next,
    Stop(String),
}

struct Pass<'a> {
    store: &'a mut Store,
    repo: &'a Path,
    forge: &'a dyn Forge,
    cfg: &'a DeliverConfig,
    cancel: &'a AtomicBool,
    d: Delivery,
}

impl Pass<'_> {
    fn note(&mut self, step: String) {
        self.d.steps.push(step);
    }

    fn take(&mut self, settled: Settled) -> Operation {
        self.d.moves.extend(settled.moves);
        settled.operation
    }

    fn settle(&mut self, op: &Operation, verdict: &Verdict, seen: serde_json::Value) -> Result<Operation> {
        let settled = self.store.settle_operation(&op.id, verdict, seen, Utc::now())?;
        Ok(self.take(settled))
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Sleeps `d`, waking early on cancel.
    fn sleep(&self, d: Duration) {
        let until = Instant::now() + d;
        while Instant::now() < until && !self.cancelled() {
            std::thread::sleep(Duration::from_millis(50).min(d));
        }
    }

    fn pause(&self) {
        self.sleep(self.cfg.poll);
    }

    fn drive(&mut self, task_id: &str) -> Result<String> {
        for _ in 0..8 {
            let task = self.store.task(task_id)?;
            let flow = match task.state {
                State::Verified => self.begin(&task)?,
                State::Integrating => self.land(&task)?,
                State::Done => return Ok("done".into()),
                State::Blocked => return Ok(format!("blocked: {}", task.blocked_reason.unwrap_or_default())),
                State::AwaitingVerification => {
                    let why = self.d.moves.last().map(|m| m.reason.clone()).unwrap_or_default();
                    return Ok(format!("sent back for verification: {why}"));
                }
                other => return Ok(format!("the task is {other}; only verified tasks integrate")),
            };
            if let Flow::Stop(why) = flow {
                return Ok(why);
            }
        }
        Ok("stopped: delivery kept moving without settling".into())
    }

    /// Whether `commit` is a head interlock set out to push for this task.
    fn pushed_before(&self, task_id: &str, commit: &str) -> Result<bool> {
        Ok(self.store.operations(task_id)?.iter().any(|o| {
            o.kind == OperationKind::OpenPr
                && o.intent.expected_head_sha.as_deref().is_some_and(|h| same_sha(h, commit))
        }))
    }

    fn base_for(&self, op: &Operation) -> Option<String> {
        op.intent.base.clone().or_else(|| self.cfg.base.clone()).or_else(|| self.forge.default_branch().ok())
    }

    fn landing_kind(&self, authority: LandingAuthority) -> OperationKind {
        if self.cfg.auto_merge && interlock_merges(authority) {
            OperationKind::ArmAutoMerge
        } else {
            OperationKind::Merge
        }
    }

    /// G5: R2 or G7 first if the evidence allows them; otherwise the landing
    /// operation is written, pinned to the verified head, before any call.
    fn begin(&mut self, task: &Task) -> Result<Flow> {
        let moves = self.store.advance(&task.id, Utc::now())?;
        if !moves.is_empty() {
            self.d.moves.extend(moves);
            return Ok(Flow::Next);
        }
        // Nothing here reaches the forge, and no ref is written: without landing authority G5 blocks first.
        let head = git::verified_head(self.repo, task)?;
        let authority = self.store.landing_authority(&task.id, Utc::now())?;
        let intent =
            OperationIntent { expected_head_sha: Some(head.clone()), base: self.cfg.base.clone(), pull_request: None };
        let (mv, op) = self.store.begin_integration(&task.id, self.landing_kind(authority), intent, Utc::now())?;
        self.d.moves.push(mv);
        if let Some(op) = op {
            git::keep_head(self.repo, &task.id, &head)?;
            self.note(format!("G5: operation {} will land only {head}", op.id));
            // Landings planned before an R2 never ran; this G5 replaces them.
            let stale: Vec<Operation> = self
                .store
                .operations(&task.id)?
                .into_iter()
                .filter(|o| o.id != op.id && lands(o.kind) && o.state == OperationState::Planned)
                .collect();
            for old in stale {
                let replaced = Verdict::Failed { reason: format!("superseded before any call by {}", op.id) };
                self.settle(&old, &replaced, json!({ "called": false }))?;
            }
        }
        Ok(Flow::Next)
    }

    fn land(&mut self, task: &Task) -> Result<Flow> {
        // Evidence first: a tree recorded while integrating, or anything else that made it stale, is R2.
        let moves = self.store.advance(&task.id, Utc::now())?;
        if !moves.is_empty() {
            self.d.moves.extend(moves);
            return Ok(Flow::Next);
        }
        let head = git::verified_head(self.repo, task)?;
        git::keep_head(self.repo, &task.id, &head)?;
        self.d.head = Some(head.clone());
        let authority = self.store.landing_authority(&task.id, Utc::now())?;
        let ops = self.store.operations(&task.id)?;
        if let Some(op) = ops.iter().rev().find(|o| lands(o.kind) && o.state == OperationState::Started) {
            self.d.pull_request = op.intent.pull_request;
            return self.watch(op.clone(), false);
        }
        let planned = ops.iter().rev().find(|o| lands(o.kind) && o.state == OperationState::Planned).cloned();
        let op = match planned {
            Some(op) if op.intent.expected_head_sha.as_deref().is_some_and(|h| same_sha(h, &head)) => op,
            Some(op) => {
                // The head G5 pinned is no longer the verified head (the tree or base changed): R2, never a quiet re-plan.
                let reason = format!(
                    "the verified head is now {head}, not {} that G5 pinned; landing starts again from verification",
                    op.intent.expected_head_sha.as_deref().unwrap_or("?")
                );
                let refused = Verdict::Land { report: MergeReport::Refused { reason }, merged_at: None };
                self.settle(&op, &refused, json!({ "called": false }))?;
                return Ok(Flow::Next);
            }
            None => {
                // An earlier landing at this head had no effect (the merge never happened): plan the next one.
                let intent = OperationIntent {
                    expected_head_sha: Some(head.clone()),
                    base: self.cfg.base.clone(),
                    pull_request: None,
                };
                let op = self.store.plan_operation(&task.id, self.landing_kind(authority), intent, Utc::now())?;
                self.note(format!("planned operation {} to land {head}", op.id));
                op
            }
        };
        let Some(base) = self.base_for(&op) else {
            return Ok(Flow::Stop("cannot tell which branch to merge into; set INTERLOCK_FORGE_BASE or --base".into()));
        };
        let pr = match self.ensure_pr(task, &head, &base)? {
            Ok(pr) => pr,
            Err(flow) => return Ok(flow),
        };
        let n = pr.number;
        self.d.pull_request = Some(n);
        let base_commit = task.input_snapshot.as_ref().map(|s| s.base_commit.clone()).unwrap_or_default();
        let pushed_at = self
            .store
            .operations(&task.id)?
            .iter()
            .rev()
            .find(|o| {
                o.kind == OperationKind::OpenPr
                    && o.state == OperationState::Confirmed
                    && o.intent.expected_head_sha.as_deref().is_some_and(|h| same_sha(h, &head))
            })
            .map(|o| o.updated_at);

        let since = Instant::now();
        let mut arm = false;
        loop {
            if self.cancelled() {
                return Ok(Flow::Stop("cancelled".into()));
            }
            let view = match self.forge.view_pr(n) {
                Ok(v) => v,
                Err(e) => return Ok(Flow::Stop(format!("the forge did not answer: {e}"))),
            };
            let mut ready = readiness(&view, &head, &base, &base_commit);
            if let Readiness::HeadMoved { found } = &ready {
                // Right after a push the forge may still show the head interlock pushed before.
                if self.pushed_before(&task.id, found)? {
                    ready = Readiness::Waiting(format!("pull request #{n} still shows the previous head"));
                }
            }
            if ready == Readiness::Ready && view.checks.is_empty() {
                let quiet = pushed_at.map_or(Duration::MAX, |t| (Utc::now() - t).to_std().unwrap_or_default());
                if quiet < self.cfg.checks_settle {
                    ready = Readiness::Waiting(format!(
                        "pull request #{n} reports no checks yet; giving CI {}s after the push",
                        self.cfg.checks_settle.as_secs()
                    ));
                }
            }
            let seen = json!({ "called": false, "readiness": ready, "pull_request": view });
            match ready {
                // The operator merges; interlock never does. Without any authority, starting the merge below refuses.
                Readiness::Ready | Readiness::Waiting(_) if authority == LandingAuthority::Operator => {
                    let pin = Pin { pull_request: Some(n), base: Some(base.clone()), tree: None };
                    self.store.pin_operation(&op.id, &pin, Utc::now())?;
                    return Ok(Flow::Stop(format!("waiting for the operator to merge pull request #{n} at {head}")));
                }
                Readiness::Ready => break,
                Readiness::Waiting(why) if op.kind == OperationKind::ArmAutoMerge => {
                    self.note(format!("{why}; arming auto-merge"));
                    arm = true;
                    break;
                }
                Readiness::Waiting(why) => {
                    if since.elapsed() >= self.cfg.wait {
                        return Ok(Flow::Stop(format!("waiting for the forge: {why}")));
                    }
                    self.pause();
                }
                Readiness::Merged { .. } => {
                    let verdict = delivery::reconcile(&op, &Observation::Pr(enrich(self.forge, &task.id, &view)), task);
                    if let Verdict::Pending { .. } = verdict {
                        // The forge says merged, but git does not show the merge on the base branch.
                        let reason = format!(
                            "pull request #{n} is reported merged, but {base} as git sees it does not contain the \
                             merge; unblock once it does"
                        );
                        let mv = self.store.block(&task.id, &reason, Utc::now())?;
                        self.d.moves.push(mv);
                        return Ok(Flow::Next);
                    }
                    self.note(format!("pull request #{n} is already merged"));
                    self.settle(&op, &verdict, seen)?;
                    return Ok(Flow::Next);
                }
                Readiness::HeadMoved { found } => {
                    let reason =
                        format!("the head of pull request #{n} is {found}, not the verified head {head}; not merging");
                    let verdict = Verdict::Land { report: MergeReport::Refused { reason }, merged_at: None };
                    self.settle(&op, &verdict, seen)?;
                    return Ok(Flow::Next);
                }
                Readiness::BaseMoved { .. } => return self.rebase(task, &op, &head, &base),
                Readiness::Refused(reason) => {
                    self.settle(&op, &Verdict::block(reason), seen)?;
                    return Ok(Flow::Next);
                }
            }
        }

        // The base as git sees it, right before the call: the API's baseRefOid can lag behind.
        match self.forge.branch_head(&base) {
            Ok(Some(tip)) if same_sha(&tip, &base_commit) => {}
            Ok(Some(_)) => return self.rebase(task, &op, &head, &base),
            Ok(None) => return Ok(Flow::Stop(format!("the base branch {base} is not on the forge"))),
            Err(e) => return Ok(Flow::Stop(format!("the forge did not answer: {e}"))),
        }
        let pin = Pin { pull_request: Some(n), base: Some(base.clone()), tree: task.current_tree.clone() };
        let op = match self.store.start_operation(&op.id, &pin, Utc::now())? {
            Ok(op) => op,
            Err(mv) => {
                self.d.moves.push(mv);
                return Ok(Flow::Next);
            }
        };
        self.note(format!(
            "{} #{n} pinned to {head} (operation {} started)",
            if arm { "arming auto-merge for" } else { "merging" },
            op.id
        ));
        let call = self.forge.merge(n, &head, self.cfg.method, arm);
        let seen = observe(self.forge, &op);
        let verdict = match (delivery::reconcile(&op, &seen, task), &call) {
            (Verdict::Failed { .. }, Err(e)) if refused_by_forge(e) => {
                Verdict::block(format!("the forge refused the merge: {e}"))
            }
            // No clear refusal and no merge to see: the request may still land. Reconcile decides later.
            (Verdict::Failed { .. }, Err(e)) => Verdict::Unknown {
                reason: format!("the merge call failed without a clear answer ({e}), and #{n} is still open"),
            },
            (Verdict::Failed { .. }, Ok(())) => {
                Verdict::Pending { reason: "the forge took the merge request but has not merged yet".into() }
            }
            (v, _) => v,
        };
        let said = match &call {
            Ok(()) => json!("ok"),
            Err(e) => json!(e.to_string()),
        };
        let op = self.settle(&op, &verdict, json!({ "call": said, "observation": seen }))?;
        if op.state == OperationState::Started {
            return self.watch(op, call.is_ok());
        }
        Ok(Flow::Next)
    }

    /// Watches a landing the forge holds (an armed auto-merge, a merge queue)
    /// until it settles or the wait runs out. Right after the forge accepted
    /// a merge request, a pull request still open at the verified head is
    /// taken as not merged yet; left open, reconcile settles it later. If
    /// landing authority ends while auto-merge is armed, it is disarmed.
    fn watch(&mut self, op: Operation, accepted: bool) -> Result<Flow> {
        let since = Instant::now();
        loop {
            let task = self.store.task(&op.task_id)?;
            if op.kind == OperationKind::ArmAutoMerge
                && !interlock_merges(self.store.landing_authority(&task.id, Utc::now())?)
            {
                let done = disarm(self.store, self.forge, &op, &task)?;
                self.d.moves.extend(done.iter().flat_map(|r| r.moves.clone()));
                self.d.reconciled.extend(done);
                return Ok(Flow::Next);
            }
            let seen = observe(self.forge, &op);
            let verdict = match delivery::reconcile(&op, &seen, &task) {
                Verdict::Failed { .. } if accepted => {
                    Verdict::Pending { reason: "the forge took the merge request but has not merged yet".into() }
                }
                v => v,
            };
            if let Verdict::Pending { reason } = &verdict {
                if since.elapsed() >= self.cfg.wait || self.cancelled() {
                    return Ok(Flow::Stop(format!("waiting for the forge: {reason}")));
                }
                self.pause();
                continue;
            }
            self.settle(&op, &verdict, json!({ "observation": seen }))?;
            return Ok(Flow::Next);
        }
    }

    /// Makes sure a pull request from `interlock/<task>` holds the verified
    /// head: pushes it and opens the pull request as needed, under one
    /// `open_pr` operation. Never overwrites a commit interlock did not push.
    fn ensure_pr(&mut self, task: &Task, head: &str, base: &str) -> Result<std::result::Result<PrView, Flow>> {
        let branch = branch_for(&task.id);
        let stop = |e: ForgeError| Ok(Err(Flow::Stop(format!("the forge did not answer: {e}"))));
        let existing = match self.forge.find_pr(&branch) {
            Ok(p) => p,
            Err(e) => return stop(e),
        };
        if let Some(pr) = existing.as_ref().filter(|p| same_sha(&p.head, head) && p.state != PrState::Closed) {
            return Ok(Ok(pr.clone()));
        }
        let open = existing.filter(|p| p.state == PrState::Open);
        let remote = match self.forge.branch_head(&branch) {
            Ok(h) => h,
            Err(e) => return stop(e),
        };
        if let Some(found) = remote.as_deref().filter(|r| !same_sha(r, head)) {
            if !self.pushed_before(&task.id, found)? {
                let reason = format!(
                    "the branch {branch} holds {found}, which interlock did not push; \
                     delete it or reset it to {head}, then unblock the task"
                );
                let mv = self.store.block(&task.id, &reason, Utc::now())?;
                self.d.moves.push(mv);
                return Ok(Err(Flow::Next));
            }
        }
        let intent = OperationIntent {
            expected_head_sha: Some(head.to_string()),
            base: Some(base.to_string()),
            pull_request: open.as_ref().map(|p| p.number),
        };
        let op = self.store.plan_operation(&task.id, OperationKind::OpenPr, intent, Utc::now())?;
        let pin = Pin { pull_request: None, base: None, tree: task.current_tree.clone() };
        let op = match self.store.start_operation(&op.id, &pin, Utc::now())? {
            Ok(op) => op,
            Err(mv) => {
                self.d.moves.push(mv);
                return Ok(Err(Flow::Next));
            }
        };
        let mut errors: Vec<ForgeError> = Vec::new();
        if remote.as_deref().is_none_or(|r| !same_sha(r, head)) {
            self.note(format!("pushing {head} to {branch} (operation {})", op.id));
            if let Err(e) = self.forge.push(head, &branch, remote.as_deref()) {
                errors.push(e);
            }
        }
        if open.is_none() && errors.is_empty() {
            self.note(format!("opening a pull request from {branch} into {base}"));
            if let Err(e) = self.forge.create_pr(&new_pr(task, &branch, base, head)) {
                errors.push(e);
            }
        }
        // Right after a push the forge may still show the previous head for a moment: read again, briefly.
        let mut reads = 0;
        let found = loop {
            let found = self.forge.find_pr(&branch);
            let lagging = match &found {
                Ok(Some(pr)) => {
                    pr.state == PrState::Open && !same_sha(&pr.head, head) && self.pushed_before(&task.id, &pr.head)?
                }
                _ => false,
            };
            if !lagging {
                break found;
            }
            reads += 1;
            if reads > 5 {
                return Ok(Err(Flow::Stop(format!(
                    "waiting for the forge: the pull request from {branch} still shows the previous head"
                ))));
            }
            self.sleep(self.cfg.poll.min(Duration::from_secs(2)));
        };
        let seen = match &found {
            Ok(Some(pr)) => Observation::Pr(pr.observed()),
            Ok(None) => Observation::NoPr,
            Err(e) => Observation::Unreachable(e.to_string()),
        };
        let said: Vec<String> = errors.iter().map(ToString::to_string).collect();
        let verdict = match delivery::reconcile(&op, &seen, task) {
            Verdict::Failed { reason } if errors.iter().any(|e| matches!(e, ForgeError::Unavailable(_))) => {
                Verdict::Unknown { reason: format!("{reason}; a call gave no answer: {}", said.join("; ")) }
            }
            Verdict::Failed { reason } => {
                Verdict::block(format!("could not open the pull request: {reason}; {}", said.join("; ")))
            }
            v => v,
        };
        let op = self.settle(&op, &verdict, json!({ "errors": said, "observation": seen }))?;
        match (op.state, found) {
            (OperationState::Confirmed, Ok(Some(pr))) => {
                self.note(format!("pull request #{} holds {head}", pr.number));
                Ok(Ok(pr))
            }
            _ => Ok(Err(Flow::Next)),
        }
    }

    /// The base moved: the evidence is about a tree that would no longer
    /// land. Computes the tree that would, records it with the new base, and
    /// sends the task back (R2). A conflict blocks the task instead.
    fn rebase(&mut self, task: &Task, op: &Operation, head: &str, base: &str) -> Result<Flow> {
        let into = format!("{}/base", git::ref_prefix(&task.id));
        let new_base = match self.forge.fetch(base, &into) {
            Ok(c) => c,
            Err(e) => return Ok(Flow::Stop(format!("the base moved, and fetching it failed: {e}"))),
        };
        let old_base = task.input_snapshot.as_ref().map(|s| s.base_commit.clone()).unwrap_or_default();
        let old_tree = task.current_tree.clone().unwrap_or_default();
        match git::merge_tree(self.repo, &new_base, head)? {
            Ok(tree) => {
                let reason = format!(
                    "the base {base} moved from {old_base} to {new_base}; the evidence covers tree {old_tree}, \
                     but {tree} is what would land"
                );
                self.note(reason.clone());
                let settled = self.store.rebase_and_withdraw(&op.id, &new_base, &tree, &reason, Utc::now())?;
                self.take(settled);
            }
            Err(paths) => {
                let reason = format!(
                    "the base {base} moved to {new_base}, and the verified change conflicts with it in {}; rework is needed",
                    paths.join(", ")
                );
                self.settle(op, &Verdict::block(reason), json!({ "called": false }))?;
            }
        }
        Ok(Flow::Next)
    }
}

fn new_pr(task: &Task, branch: &str, base: &str, head: &str) -> NewPr {
    let title: String = task.intent.lines().next().unwrap_or(&task.id).chars().take(120).collect();
    let snapshot = task.input_snapshot.as_ref().map(|s| s.base_commit.as_str()).unwrap_or("?");
    let criteria: Vec<String> = task.criteria.iter().map(|c| format!("- `{}`: {}", c.id, c.statement)).collect();
    let body = format!(
        "Verified by interlock. It may land only at `{head}` (`gh pr merge --match-head-commit`).\n\n\
         - Task: `{}`\n- Verified tree: `{}`\n- Built on: `{snapshot}`\n\nCriteria:\n{}\n",
        task.id,
        task.current_tree.as_deref().unwrap_or("?"),
        criteria.join("\n")
    );
    NewPr { branch: branch.to_string(), base: base.to_string(), title, body }
}
