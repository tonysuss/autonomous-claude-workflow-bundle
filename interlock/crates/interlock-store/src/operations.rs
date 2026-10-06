//! Forge operations. Each one is written planned before interlock calls the
//! forge, marked started in its own transaction right before the call, and
//! settled from what the forge reports, either straight after the call or
//! when a restarted controller reconciles it.

use interlock_core::delivery::{self, Verdict};
use interlock_core::lifecycle::RefusalCode;

use super::*;

/// An operation after a delivery step, and the moves that step made.
#[derive(Debug, Clone, Serialize)]
pub struct Settled {
    pub operation: Operation,
    pub moves: Vec<Move>,
}

/// What the caller is about to do with a started operation. The store checks
/// it in the same transaction that marks the operation started.
#[derive(Debug, Clone, Default)]
pub struct Pin {
    pub pull_request: Option<u64>,
    /// The branch the pull request targets.
    pub base: Option<String>,
    /// The tree the operation's head was built from. It must still be the
    /// task's current tree.
    pub tree: Option<String>,
}

fn get_operation(c: &Connection, id: &str) -> Result<Operation> {
    c.query_row("SELECT record FROM operations WHERE id = ?1", [id], |r| r.get::<_, String>(0))
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("operation {id}")))
        .and_then(decode)
}

fn operations_of(c: &Connection, task_id: Option<&str>) -> Result<Vec<Operation>> {
    let mut stmt = c.prepare("SELECT record FROM operations WHERE ?1 IS NULL OR task_id = ?1 ORDER BY rowid")?;
    let rows = stmt.query_map([task_id], |r| r.get::<_, String>(0))?;
    rows.map(|r| decode(r?)).collect()
}

/// Started or unknown: the forge may have acted, and nobody has confirmed what it did.
fn is_open(op: &Operation) -> bool {
    matches!(op.state, OperationState::Started | OperationState::Unknown)
}

fn refused(code: RefusalCode, message: String) -> StoreError {
    StoreError::Refused(Refusal { signal: None, code, message, missing: vec![] })
}

/// Fails an operation that was never called.
fn fail_uncalled(tx: &Transaction, v: &Validators, op: &mut Operation, why: &str, now: Timestamp) -> Result<()> {
    op.state = OperationState::Failed;
    op.outcome = Some(serde_json::json!({ "refused": why, "called": false }));
    op.updated_at = now;
    put_operation(tx, v, op, false)
}

impl Store {
    pub fn operation(&self, id: &str) -> Result<Operation> {
        get_operation(&self.conn, id)
    }

    pub fn operations(&self, task_id: &str) -> Result<Vec<Operation>> {
        operations_of(&self.conn, Some(task_id))
    }

    /// Operations whose outcome nobody has confirmed, for one task or all.
    pub fn open_operations(&self, task_id: Option<&str>) -> Result<Vec<Operation>> {
        Ok(operations_of(&self.conn, task_id)?.into_iter().filter(is_open).collect())
    }

    /// Landing operations still planned but tied to a pull request: the
    /// operator may merge it, so reconcile looks at them too.
    pub fn pinned_landings(&self, task_id: Option<&str>) -> Result<Vec<Operation>> {
        Ok(operations_of(&self.conn, task_id)?
            .into_iter()
            .filter(|o| {
                delivery::lands(o.kind) && o.state == OperationState::Planned && o.intent.pull_request.is_some()
            })
            .collect())
    }

    /// The landing authority the task's grants give now.
    pub fn landing_authority(&self, task_id: &str, now: Timestamp) -> Result<LandingAuthority> {
        Ok(grants::landing_authority(task_id, &all_grants(&self.conn)?, now))
    }

    /// Writes a planned operation for an integrating task. No call is made yet.
    pub fn plan_operation(
        &mut self,
        task_id: &str,
        kind: OperationKind,
        mut intent: OperationIntent,
        now: Timestamp,
    ) -> Result<Operation> {
        let (tx, v) = self.begin()?;
        let task = get_task(&tx, task_id)?;
        if task.state != State::Integrating {
            return Err(refused(RefusalCode::WrongState, format!("task {task_id} is {}, not integrating", task.state)));
        }
        if intent.tree.is_none() {
            intent.tree = task.current_tree.clone();
        }
        let op = Operation {
            id: new_id("op"),
            task_id: task.id,
            kind,
            intent,
            state: OperationState::Planned,
            outcome: None,
            created_at: now,
            updated_at: now,
        };
        put_operation(&tx, v, &op, true)?;
        tx.commit()?;
        Ok(op)
    }

    /// Ties a planned landing operation to its pull request without starting
    /// it: the operator will merge, and reconcile watches for that.
    pub fn pin_operation(&mut self, op_id: &str, pin: &Pin, now: Timestamp) -> Result<Operation> {
        let (tx, v) = self.begin()?;
        let mut op = get_operation(&tx, op_id)?;
        if op.state != OperationState::Planned {
            return Err(refused(
                RefusalCode::WrongState,
                format!("operation {} is {:?}, not planned", op.id, op.state),
            ));
        }
        op.intent.pull_request = pin.pull_request.or(op.intent.pull_request);
        op.intent.base = pin.base.clone().or(op.intent.base);
        op.updated_at = now;
        put_operation(&tx, v, &op, false)?;
        tx.commit()?;
        Ok(op)
    }

    /// Marks a planned operation started, committed before the forge call.
    /// In the same transaction the store checks that the evidence still
    /// covers the task's current tree, that the head was built from that
    /// tree, and that landing authority still holds. Stale evidence or a
    /// changed tree fails the operation without a call and applies R2; a
    /// revoked grant fails it and blocks the task. Either move comes back as
    /// `Err`. Only one operation per task may be in flight, except that an
    /// armed auto-merge may be withdrawn.
    pub fn start_operation(
        &mut self,
        op_id: &str,
        pin: &Pin,
        now: Timestamp,
    ) -> Result<std::result::Result<Operation, Move>> {
        let (tx, v) = self.begin()?;
        let mut op = get_operation(&tx, op_id)?;
        let task = get_task(&tx, &op.task_id)?;
        let busy = operations_of(&tx, Some(&task.id))?.into_iter().find(|o| {
            o.id != op.id
                && is_open(o)
                && !(op.kind == OperationKind::DisarmAutoMerge && o.kind == OperationKind::ArmAutoMerge)
        });
        if let Some(busy) = busy {
            return Err(refused(
                RefusalCode::WrongState,
                format!("operation {} is still in flight; reconcile it first", busy.id),
            ));
        }
        if op.kind != OperationKind::DisarmAutoMerge && task.state == State::Integrating && op.task_id == task.id {
            let report = report_for(&tx, &task)?;
            if !report.all_pass {
                let why = "the evidence no longer covers the task's current tree; nothing was sent to the forge";
                fail_uncalled(&tx, v, &mut op, why, now)?;
                let out = lifecycle::advance(&task, &report, now)
                    .ok_or_else(|| StoreError::Invalid("stale evidence while integrating, but no R2".into()))?;
                let mv = apply(&tx, v, &out, None, now)?;
                tx.commit()?;
                return Ok(Err(mv));
            }
            let current = task.current_tree.as_deref().unwrap_or("");
            if let Some(tree) = pin.tree.as_deref().filter(|t| !interlock_core::evidence::same_tree(t, current)) {
                let why = format!(
                    "the head was built from tree {tree}, but the task's tree is now {current}; landing starts again from verification"
                );
                fail_uncalled(&tx, v, &mut op, &why, now)?;
                let refused = MergeReport::Refused { reason: why };
                let out = lifecycle::confirm_integration(&task, &op, &refused, &report_for(&tx, &task)?, now)?;
                let mv = apply(&tx, v, &out, None, now)?;
                tx.commit()?;
                return Ok(Err(mv));
            }
        }
        let landing = grants::landing_authority(&task.id, &all_grants(&tx)?, now);
        if let Err(refusal) = delivery::authorize(&task, &op, landing) {
            if refusal.code != RefusalCode::GrantInsufficient {
                return Err(refusal.into());
            }
            fail_uncalled(&tx, v, &mut op, &refusal.message, now)?;
            let mv = apply(&tx, v, &lifecycle::block(&task, &refusal.message, now)?, None, now)?;
            tx.commit()?;
            return Ok(Err(mv));
        }
        op.state = OperationState::Started;
        op.intent.pull_request = pin.pull_request.or(op.intent.pull_request);
        op.intent.base = pin.base.clone().or(op.intent.base);
        op.updated_at = now;
        put_operation(&tx, v, &op, false)?;
        tx.commit()?;
        Ok(Ok(op))
    }

    /// Applies what the forge reported for an operation, in one transaction:
    /// G6 or R2 for a landing; the operation's own state otherwise; a block
    /// when the operator must decide or nobody can tell what happened. A task
    /// blocked only on unknown outcomes resumes once the forge has answered.
    ///
    /// G6 also needs, at that moment, current passing evidence for the task's
    /// tree and landing authority at the time the forge merged. Without
    /// either, the merge is recorded and the task is blocked with the reason.
    /// Confirmed and failed operations are final.
    pub fn settle_operation(
        &mut self,
        op_id: &str,
        verdict: &Verdict,
        seen: serde_json::Value,
        now: Timestamp,
    ) -> Result<Settled> {
        let (tx, v) = self.begin()?;
        let mut op = get_operation(&tx, op_id)?;
        if matches!(op.state, OperationState::Confirmed | OperationState::Failed) {
            return Err(refused(RefusalCode::WrongState, format!("operation {} is already {:?}", op.id, op.state)));
        }
        if op.state == OperationState::Planned && matches!(verdict, Verdict::Pending { .. } | Verdict::Unknown { .. }) {
            return Err(refused(
                RefusalCode::WrongState,
                format!("operation {} was never started; nothing can be pending", op.id),
            ));
        }
        let mut task = get_task(&tx, &op.task_id)?;
        let mut moves = Vec::new();
        let determinate = !matches!(verdict, Verdict::Unknown { .. });
        let others_unknown =
            operations_of(&tx, Some(&task.id))?.iter().any(|o| o.id != op.id && o.state == OperationState::Unknown);
        if determinate
            && !others_unknown
            && let Some(out) = delivery::resume(&task, now)
        {
            moves.push(apply(&tx, v, &out, None, now)?);
            task = out.task;
        }
        let block = |task: &mut Task, reason: &str, moves: &mut Vec<Move>| -> Result<()> {
            if !task.state.is_terminal() && task.state != State::Blocked {
                let out = lifecycle::block(task, reason, now)?;
                moves.push(apply(&tx, v, &out, None, now)?);
                *task = out.task;
            }
            Ok(())
        };
        op.state = match verdict {
            Verdict::Land { report: MergeReport::Merged { head_sha }, merged_at }
                if task.state == State::Integrating =>
            {
                let report = report_for(&tx, &task)?;
                let at = merged_at.unwrap_or(now);
                let landing = grants::landing_authority(&task.id, &all_grants(&tx)?, at);
                if !report.all_pass {
                    let why = format!(
                        "merged at {head_sha}, but the evidence no longer covers the task's current tree; reconcile by hand"
                    );
                    block(&mut task, &why, &mut moves)?;
                } else if landing == LandingAuthority::None {
                    let why = format!(
                        "merged at {head_sha} at {}, when no landing authority was granted for this task; reconcile by hand",
                        ts(at)
                    );
                    block(&mut task, &why, &mut moves)?;
                } else {
                    let merged = MergeReport::Merged { head_sha: head_sha.clone() };
                    match lifecycle::confirm_integration(&task, &op, &merged, &report, now) {
                        Ok(out) => moves.push(apply(&tx, v, &out, None, now)?),
                        Err(refusal) => block(&mut task, &refusal.message, &mut moves)?,
                    }
                }
                OperationState::Confirmed
            }
            Verdict::Land { report: report @ MergeReport::Refused { .. }, .. } if task.state == State::Integrating => {
                let evidence = report_for(&tx, &task)?;
                match lifecycle::confirm_integration(&task, &op, report, &evidence, now) {
                    Ok(out) => moves.push(apply(&tx, v, &out, None, now)?),
                    Err(refusal) => block(&mut task, &refusal.message, &mut moves)?,
                }
                OperationState::Failed
            }
            // The task left integrating meanwhile (the operator blocked or cancelled it): keep what the forge did.
            Verdict::Land { report: MergeReport::Merged { .. }, .. } => OperationState::Confirmed,
            Verdict::Land { report: MergeReport::Refused { .. }, .. } => OperationState::Failed,
            Verdict::Confirmed => OperationState::Confirmed,
            Verdict::Failed { .. } => OperationState::Failed,
            Verdict::Pending { .. } => OperationState::Started,
            Verdict::Block { reason, took_effect } => {
                block(&mut task, reason, &mut moves)?;
                if *took_effect { OperationState::Confirmed } else { OperationState::Failed }
            }
            Verdict::Unknown { reason } => {
                let why = format!(
                    "{}: {} ({:?}) {reason}; run `interlock reconcile {}` once the forge answers",
                    delivery::UNKNOWN_OUTCOME,
                    op.id,
                    op.kind,
                    task.id
                );
                block(&mut task, &why, &mut moves)?;
                OperationState::Unknown
            }
        };
        op.outcome = Some(serde_json::json!({ "verdict": verdict, "seen": seen }));
        op.updated_at = now;
        put_operation(&tx, v, &op, false)?;
        tx.commit()?;
        Ok(Settled { operation: op, moves })
    }

    /// The base moved before the merge. Records the new base and the tree
    /// that would land on it, fails the landing operation, and applies R2:
    /// evidence about the old tree no longer counts.
    pub fn rebase_and_withdraw(
        &mut self,
        op_id: &str,
        base: &str,
        tree: &str,
        reason: &str,
        now: Timestamp,
    ) -> Result<Settled> {
        let (tx, v) = self.begin()?;
        let mut op = get_operation(&tx, op_id)?;
        let task = get_task(&tx, &op.task_id)?;
        let rebased = delivery::rebase(&task, base, tree, now)?;
        let report = MergeReport::Refused { reason: reason.to_string() };
        let out = lifecycle::confirm_integration(&rebased, &op, &report, &report_for(&tx, &rebased)?, now)?;
        let mv = apply(&tx, v, &out, None, now)?;
        op.state = OperationState::Failed;
        op.outcome = Some(serde_json::json!({ "withdrawn": reason, "base": base, "tree": tree, "called": false }));
        op.updated_at = now;
        put_operation(&tx, v, &op, false)?;
        tx.commit()?;
        Ok(Settled { operation: op, moves: vec![mv] })
    }
}
