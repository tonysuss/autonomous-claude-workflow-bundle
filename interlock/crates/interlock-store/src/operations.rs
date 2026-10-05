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

    /// Writes a planned operation for an integrating task. No call is made yet.
    pub fn plan_operation(
        &mut self,
        task_id: &str,
        kind: OperationKind,
        intent: OperationIntent,
        now: Timestamp,
    ) -> Result<Operation> {
        let (tx, v) = self.begin()?;
        let task = get_task(&tx, task_id)?;
        if task.state != State::Integrating {
            return Err(refused(RefusalCode::WrongState, format!("task {task_id} is {}, not integrating", task.state)));
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

    /// Marks a planned operation started, committed before the forge call.
    /// The core checks landing authority again here: if it was revoked, the
    /// operation fails without a call and the task is blocked, the move
    /// returned as `Err`. Only one operation per task may be in flight.
    pub fn start_operation(
        &mut self,
        op_id: &str,
        pull_request: Option<u64>,
        now: Timestamp,
    ) -> Result<std::result::Result<Operation, Move>> {
        let (tx, v) = self.begin()?;
        let mut op = get_operation(&tx, op_id)?;
        let task = get_task(&tx, &op.task_id)?;
        if let Some(busy) = operations_of(&tx, Some(&task.id))?.into_iter().find(|o| o.id != op.id && is_open(o)) {
            return Err(refused(
                RefusalCode::WrongState,
                format!("operation {} is still in flight; reconcile it first", busy.id),
            ));
        }
        let landing = grants::landing_authority(&task.id, &all_grants(&tx)?, now);
        if let Err(refusal) = delivery::authorize(&task, &op, landing) {
            if refusal.code != RefusalCode::GrantInsufficient {
                return Err(refusal.into());
            }
            op.state = OperationState::Failed;
            op.outcome = Some(serde_json::json!({ "refused": refusal.message, "called": false }));
            op.updated_at = now;
            put_operation(&tx, v, &op, false)?;
            let mv = apply(&tx, v, &lifecycle::block(&task, &refusal.message, now)?, None, now)?;
            tx.commit()?;
            return Ok(Err(mv));
        }
        op.state = OperationState::Started;
        if pull_request.is_some() {
            op.intent.pull_request = pull_request;
        }
        op.updated_at = now;
        put_operation(&tx, v, &op, false)?;
        tx.commit()?;
        Ok(Ok(op))
    }

    /// Applies what the forge reported for an operation, in one transaction:
    /// G6 or R2 for a landing; the operation's own state otherwise; a block
    /// when the operator must decide or nobody can tell what happened. A task
    /// blocked only on unknown outcomes resumes once the forge has answered.
    pub fn settle_operation(
        &mut self,
        op_id: &str,
        verdict: &Verdict,
        seen: serde_json::Value,
        now: Timestamp,
    ) -> Result<Settled> {
        let (tx, v) = self.begin()?;
        let mut op = get_operation(&tx, op_id)?;
        let mut task = get_task(&tx, &op.task_id)?;
        let mut moves = Vec::new();
        let determinate = !matches!(verdict, Verdict::Unknown { .. });
        let others_unknown =
            operations_of(&tx, Some(&task.id))?.iter().any(|o| o.id != op.id && o.state == OperationState::Unknown);
        if determinate && !others_unknown {
            if let Some(out) = delivery::resume(&task, now) {
                moves.push(apply(&tx, v, &out, None, now)?);
                task = out.task;
            }
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
            Verdict::Land { report } if task.state == State::Integrating => {
                match lifecycle::confirm_integration(&task, &op, report, now) {
                    Ok(out) => {
                        moves.push(apply(&tx, v, &out, None, now)?);
                        match report {
                            MergeReport::Merged { .. } => OperationState::Confirmed,
                            MergeReport::Refused { .. } => OperationState::Failed,
                        }
                    }
                    Err(refusal) => {
                        block(&mut task, &refusal.message, &mut moves)?;
                        OperationState::Failed
                    }
                }
            }
            // The task left integrating meanwhile (the operator blocked or cancelled it): keep what the forge did.
            Verdict::Land { report: MergeReport::Merged { .. } } => OperationState::Confirmed,
            Verdict::Land { report: MergeReport::Refused { .. } } => OperationState::Failed,
            Verdict::Confirmed => OperationState::Confirmed,
            Verdict::Failed { .. } => OperationState::Failed,
            Verdict::Pending { .. } => OperationState::Started,
            Verdict::Block { reason } => {
                block(&mut task, reason, &mut moves)?;
                OperationState::Failed
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
        let out = lifecycle::confirm_integration(&rebased, &op, &report, now)?;
        let mv = apply(&tx, v, &out, None, now)?;
        op.state = OperationState::Failed;
        op.outcome = Some(serde_json::json!({ "withdrawn": reason, "base": base, "tree": tree, "called": false }));
        op.updated_at = now;
        put_operation(&tx, v, &op, false)?;
        tx.commit()?;
        Ok(Settled { operation: op, moves: vec![mv] })
    }
}
