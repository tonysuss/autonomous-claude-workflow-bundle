//! Delivery of a verified task: G5, push the verified head, open the pull
//! request, wait until it is ready, merge it pinned to the verified head, and
//! G6. Each forge call that changes something gets an operation row first,
//! committed planned and then started, and is settled from what the forge
//! shows afterwards rather than from what the call returned. `reconcile`
//! does the same for operations a crashed controller left open.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use interlock_core::delivery::{self, Observation, PrState, Verdict, branch_for, lands, same_sha};
use interlock_core::lifecycle::{MergeReport, Move};
use interlock_schema::{Operation, OperationIntent, OperationKind, OperationState, State, Task};
use interlock_store::{Settled, Store, StoreError};
use serde::Serialize;
use serde_json::json;

use crate::{Forge, ForgeError, MergeMethod, NewPr, PrView, Readiness, git, readiness};

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
}

impl Default for DeliverConfig {
    fn default() -> DeliverConfig {
        DeliverConfig {
            base: None,
            method: MergeMethod::Merge,
            auto_merge: false,
            wait: Duration::from_secs(15 * 60),
            poll: Duration::from_secs(15),
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
    /// INTERLOCK_FORGE_WAIT and INTERLOCK_FORGE_POLL over the defaults.
    pub fn from_env() -> std::result::Result<DeliverConfig, String> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let mut cfg = DeliverConfig { base: var("INTERLOCK_FORGE_BASE"), ..DeliverConfig::default() };
        if let Some(m) = var("INTERLOCK_FORGE_METHOD") {
            cfg.method =
                MergeMethod::parse(&m).ok_or(format!("INTERLOCK_FORGE_METHOD: {m} is not merge, squash or rebase"))?;
        }
        if let Some(a) = var("INTERLOCK_FORGE_AUTO_MERGE") {
            cfg.auto_merge = matches!(a.as_str(), "1" | "true" | "yes");
        }
        for (key, slot) in [("INTERLOCK_FORGE_WAIT", &mut cfg.wait), ("INTERLOCK_FORGE_POLL", &mut cfg.poll)] {
            if let Some(v) = var(key) {
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

fn observe(forge: &dyn Forge, op: &Operation) -> Observation {
    let seen = match (op.kind, op.intent.pull_request) {
        (OperationKind::OpenPr, _) | (_, None) => forge.find_pr(&branch_for(&op.task_id)),
        (_, Some(n)) => forge.view_pr(n).map(Some),
    };
    match seen {
        Ok(Some(pr)) => Observation::Pr(pr.observed()),
        Ok(None) => Observation::NoPr,
        Err(e) => Observation::Unreachable(e.to_string()),
    }
}

/// Asks the forge about every operation nobody confirmed (started or
/// unknown) and settles each from what it shows: a merge of the expected head
/// is G6; a refusal or a moved head is R2; no answer leaves the operation
/// unknown and the task blocked with the reason.
pub fn reconcile(store: &mut Store, forge: &dyn Forge, task_id: Option<&str>) -> Result<Vec<Reconciled>> {
    let mut out = Vec::new();
    for op in store.open_operations(task_id)? {
        let seen = observe(forge, &op);
        let verdict = delivery::reconcile(&op, &seen);
        let settled =
            store.settle_operation(&op.id, &verdict, json!({ "reconciled": true, "observation": seen }), Utc::now())?;
        out.push(Reconciled {
            operation: op.id.clone(),
            task_id: op.task_id.clone(),
            kind: op.kind,
            before: op.state,
            after: settled.operation.state,
            verdict,
            moves: settled.moves,
        });
    }
    Ok(out)
}

/// Delivers a verified task: G5, then push, pull request, readiness, the
/// pinned merge and G6. Resumes an integrating task where it stopped, after
/// reconciling its open operations. Stops, with the reason, when the forge
/// is not ready within `cfg.wait`, when the task is sent back or blocked, or
/// when it is done.
pub fn integrate(
    store: &mut Store,
    repo: &Path,
    forge: &dyn Forge,
    cfg: &DeliverConfig,
    task_id: &str,
    cancel: &AtomicBool,
) -> Result<Delivery> {
    let reconciled = reconcile(store, forge, Some(task_id))?;
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

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Sleeps one poll interval, waking early on cancel.
    fn pause(&self) {
        let until = Instant::now() + self.cfg.poll;
        while Instant::now() < until && !self.cancelled() {
            std::thread::sleep(Duration::from_millis(50).min(self.cfg.poll));
        }
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

    fn landing_kind(&self) -> OperationKind {
        if self.cfg.auto_merge { OperationKind::ArmAutoMerge } else { OperationKind::Merge }
    }

    /// G5: R2 or G7 first if the evidence allows them; otherwise the landing
    /// operation is written, pinned to the verified head, before any call.
    fn begin(&mut self, task: &Task) -> Result<Flow> {
        let moves = self.store.advance(&task.id, Utc::now())?;
        if !moves.is_empty() {
            self.d.moves.extend(moves);
            return Ok(Flow::Next);
        }
        // Nothing here reaches the forge: without landing authority G5 blocks before any call.
        let head = git::verified_head(self.repo, task)?;
        let intent =
            OperationIntent { expected_head_sha: Some(head.clone()), base: self.cfg.base.clone(), pull_request: None };
        let (mv, op) = self.store.begin_integration(&task.id, self.landing_kind(), intent, Utc::now())?;
        self.d.moves.push(mv);
        if let Some(op) = op {
            self.note(format!("G5: operation {} will land only {head}", op.id));
        }
        Ok(Flow::Next)
    }

    fn land(&mut self, task: &Task) -> Result<Flow> {
        let head = git::verified_head(self.repo, task)?;
        self.d.head = Some(head.clone());
        let ops = self.store.operations(&task.id)?;
        if let Some(op) = ops.iter().rev().find(|o| lands(o.kind) && o.state == OperationState::Started) {
            self.d.pull_request = op.intent.pull_request;
            return self.watch(op.clone(), false);
        }
        let planned = ops.iter().rev().find(|o| lands(o.kind) && o.state == OperationState::Planned);
        let op = match planned {
            Some(op) if op.intent.expected_head_sha.as_deref().is_some_and(|h| same_sha(h, &head)) => op.clone(),
            stale => {
                let mut base = None;
                if let Some(op) = stale {
                    base = op.intent.base.clone();
                    let replaced =
                        Verdict::Failed { reason: format!("replaced before any call: the verified head is {head}") };
                    let settled =
                        self.store.settle_operation(&op.id, &replaced, json!({ "called": false }), Utc::now())?;
                    self.take(settled);
                }
                let intent = OperationIntent { expected_head_sha: Some(head.clone()), base, pull_request: None };
                let op = self.store.plan_operation(&task.id, self.landing_kind(), intent, Utc::now())?;
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
        self.d.pull_request = Some(pr.number);
        let base_commit = task.input_snapshot.as_ref().map(|s| s.base_commit.clone()).unwrap_or_default();

        let since = Instant::now();
        let mut arm = false;
        loop {
            if self.cancelled() {
                return Ok(Flow::Stop("cancelled".into()));
            }
            let view = match self.forge.view_pr(pr.number) {
                Ok(v) => v,
                Err(e) => return Ok(Flow::Stop(format!("the forge did not answer: {e}"))),
            };
            let mut ready = readiness(&view, &head, &base_commit);
            if let Readiness::HeadMoved { found } = &ready {
                // Right after a push the forge may still show the head interlock pushed before.
                if self.pushed_before(&task.id, found)? {
                    ready = Readiness::Waiting(format!("pull request #{} still shows the previous head", pr.number));
                }
            }
            let seen = json!({ "called": false, "readiness": ready, "pull_request": view });
            match ready {
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
                    let verdict = delivery::reconcile(&op, &Observation::Pr(view.observed()));
                    self.note(format!("pull request #{} is already merged", pr.number));
                    let settled = self.store.settle_operation(&op.id, &verdict, seen, Utc::now())?;
                    self.take(settled);
                    return Ok(Flow::Next);
                }
                Readiness::HeadMoved { found } => {
                    let reason = format!(
                        "the head of pull request #{} is {found}, not the verified head {head}; not merging",
                        pr.number
                    );
                    let verdict = Verdict::Land { report: MergeReport::Refused { reason } };
                    let settled = self.store.settle_operation(&op.id, &verdict, seen, Utc::now())?;
                    self.take(settled);
                    return Ok(Flow::Next);
                }
                Readiness::BaseMoved { .. } => return self.rebase(task, &op, &head, &base),
                Readiness::Refused(reason) => {
                    let settled = self.store.settle_operation(&op.id, &Verdict::Block { reason }, seen, Utc::now())?;
                    self.take(settled);
                    return Ok(Flow::Next);
                }
            }
        }

        let op = match self.store.start_operation(&op.id, Some(pr.number), Utc::now())? {
            Ok(op) => op,
            Err(mv) => {
                self.d.moves.push(mv);
                return Ok(Flow::Next);
            }
        };
        // An arm_auto_merge operation merges directly when the pull request is already ready.
        self.note(format!(
            "{} #{} pinned to {head} (operation {} started)",
            if arm { "arming auto-merge for" } else { "merging" },
            pr.number,
            op.id
        ));
        let call = self.forge.merge(pr.number, &head, self.cfg.method, arm);
        let seen = observe(self.forge, &op);
        let verdict = match (delivery::reconcile(&op, &seen), &call) {
            (Verdict::Failed { .. }, Err(e)) => Verdict::Block { reason: format!("the forge refused the merge: {e}") },
            (Verdict::Failed { .. }, Ok(())) => {
                Verdict::Pending { reason: "the forge took the merge request but has not merged yet".into() }
            }
            (v, _) => v,
        };
        let call = match &call {
            Ok(()) => json!("ok"),
            Err(e) => json!(e.to_string()),
        };
        let settled =
            self.store.settle_operation(&op.id, &verdict, json!({ "call": call, "observation": seen }), Utc::now())?;
        let op = self.take(settled);
        if op.state == OperationState::Started {
            return self.watch(op, call == json!("ok"));
        }
        Ok(Flow::Next)
    }

    /// Watches a landing the forge holds (an armed auto-merge, a merge queue)
    /// until it settles or the wait runs out. Right after the forge accepted
    /// a merge request, a pull request still open at the verified head is
    /// taken as not merged yet; left open, reconcile settles it later.
    fn watch(&mut self, op: Operation, accepted: bool) -> Result<Flow> {
        let since = Instant::now();
        loop {
            let seen = observe(self.forge, &op);
            let verdict = match delivery::reconcile(&op, &seen) {
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
            let settled = self.store.settle_operation(&op.id, &verdict, json!({ "observation": seen }), Utc::now())?;
            self.take(settled);
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
        let op = match self.store.start_operation(&op.id, None, Utc::now())? {
            Ok(op) => op,
            Err(mv) => {
                self.d.moves.push(mv);
                return Ok(Err(Flow::Next));
            }
        };
        let mut errors = Vec::new();
        if remote.as_deref().is_none_or(|r| !same_sha(r, head)) {
            self.note(format!("pushing {head} to {branch} (operation {})", op.id));
            if let Err(e) = self.forge.push(head, &branch, remote.as_deref()) {
                errors.push(format!("push: {e}"));
            }
        }
        if open.is_none() && errors.is_empty() {
            self.note(format!("opening a pull request from {branch} into {base}"));
            if let Err(e) = self.forge.create_pr(&new_pr(task, &branch, base, head)) {
                errors.push(format!("create: {e}"));
            }
        }
        let found = self.forge.find_pr(&branch);
        let seen = match &found {
            Ok(Some(pr)) => Observation::Pr(pr.observed()),
            Ok(None) => Observation::NoPr,
            Err(e) => Observation::Unreachable(e.to_string()),
        };
        let verdict = match delivery::reconcile(&op, &seen) {
            Verdict::Failed { reason } => {
                Verdict::Block { reason: format!("could not open the pull request: {reason}; {}", errors.join("; ")) }
            }
            v => v,
        };
        let settled = self.store.settle_operation(
            &op.id,
            &verdict,
            json!({ "errors": errors, "observation": seen }),
            Utc::now(),
        )?;
        let op = self.take(settled);
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
                let settled = self.store.settle_operation(
                    &op.id,
                    &Verdict::Block { reason },
                    json!({ "called": false }),
                    Utc::now(),
                )?;
                self.take(settled);
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
        "Verified by interlock. interlock merges this pull request only at `{head}` \
         (`gh pr merge --match-head-commit`).\n\n- Task: `{}`\n- Verified tree: `{}`\n- Built on: `{snapshot}`\n\n\
         Criteria:\n{}\n",
        task.id,
        task.current_tree.as_deref().unwrap_or("?"),
        criteria.join("\n")
    );
    NewPr { branch: branch.to_string(), base: base.to_string(), title, body }
}
