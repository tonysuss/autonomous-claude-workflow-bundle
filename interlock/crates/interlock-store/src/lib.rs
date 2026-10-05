//! The only writer of interlock state. Every change is one SQLite transaction:
//! load the records, ask the policy core, write what it returns, commit.

use std::path::{Path, PathBuf};

use interlock_core::brief::{self, Brief};
use interlock_core::capability::{CapabilitySet, Fallback};
use interlock_core::digest::sha256_hex;
use interlock_core::evidence::{self, EvidenceReport};
use interlock_core::grants::{self, HostPolicy, Profile};
use interlock_core::lifecycle::{self, EvidenceKind, MergeReport, Move, Outcome, Refusal, Start, Submission, TaskSpec};
use interlock_core::workflow::{self, Mode, Workflow};
use interlock_schema::*;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

const MIGRATIONS: &[&str] =
    &[include_str!("migrations/001_initial.sql"), include_str!("migrations/002_check_runs.sql")];

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("refused: {0}")]
    Refused(#[from] Refusal),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("the token does not match attempt {0}")]
    BadToken(String),
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Schema(#[from] SchemaError),
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("injected fault: {0}")]
    Fault(&'static str),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Test hooks for fault injection between applying an event and committing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// Abort the process, as a crash would.
    CrashBeforeCommit,
    /// Return an error, which rolls the transaction back.
    ErrorBeforeCommit,
}

pub struct Store {
    conn: Connection,
    validators: Validators,
    fault: Option<Fault>,
    /// Where check output is kept, content-addressed. None for in-memory stores.
    artifacts: Option<PathBuf>,
}

/// The outcome of an inbound report. A duplicate returns the first outcome unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Applied<T> {
    pub event_id: String,
    pub duplicate: bool,
    pub outcome: T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultApplied {
    pub result: TaskResult,
    pub moved: Option<Move>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceApplied {
    pub evidence: Evidence,
    /// Whether the evidence matches the tree under verification. `None` while
    /// no result has been accepted yet: a worker's claim then counts once its
    /// result is submitted with the same tree.
    pub current: Option<bool>,
}

pub struct StartAttempt {
    pub task_id: String,
    pub role: Role,
    pub mode: Mode,
    pub host: HostRef,
    pub capabilities: CapabilitySet,
    pub profile: Profile,
    pub host_policy: HostPolicy,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub worktree: Option<String>,
}

#[allow(clippy::large_enum_variant)] // short-lived return value
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "started", rename_all = "snake_case")]
pub enum Started {
    Yes {
        attempt: Attempt,
        /// Shown once. The store keeps only its hash.
        token: String,
        fallbacks: Vec<(Fallback, String)>,
        moved: Option<Move>,
    },
    Blocked {
        moved: Move,
    },
}

pub struct SubmitResult {
    pub attempt_id: String,
    pub token: String,
    pub epoch: u32,
    pub output_tree: String,
    pub changed_paths: Vec<String>,
    pub summary: String,
    pub open_questions: Vec<String>,
    pub event_id: Option<String>,
}

pub struct AddEvidence {
    pub attempt_id: String,
    pub token: String,
    pub criterion_id: String,
    pub strength: Strength,
    pub tree: String,
    pub environment: Option<String>,
    pub evidence_refs: Vec<String>,
    pub note: Option<String>,
    pub event_id: Option<String>,
}

pub struct NewGrant {
    pub principal: String,
    pub task_scope: Vec<String>,
    pub action_classes: Vec<ActionClass>,
    pub tools: ToolPolicy,
    pub landing_authority: LandingAuthority,
    pub origin: String,
    pub expires_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TransitionRow {
    pub seq: i64,
    pub signal: String,
    pub from: String,
    pub to: String,
    pub reason: String,
    pub attempt_id: Option<String>,
    pub at: String,
}

/// What happened when a check ran. Everything else is derived by the store.
pub struct NewCheckRun {
    pub task_id: String,
    pub criterion_id: String,
    /// The attempt's id and token; `None` for the operator or supervisor.
    pub attempt: Option<(String, String)>,
    pub target: RunTarget,
    pub tree: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub output: Vec<u8>,
}

/// Writes `bytes` once under `dir/sha256/<hex>` and returns its relative path.
fn write_artifact(dir: &Path, bytes: &[u8]) -> Result<String> {
    let hex = sha256_hex(bytes);
    let rel = format!("artifacts/sha256/{hex}");
    let path = dir.join("sha256").join(&hex);
    if !path.exists() {
        let io = |e: std::io::Error| StoreError::Invalid(format!("cannot write artifact {}: {e}", path.display()));
        std::fs::create_dir_all(path.parent().expect("artifact path has a parent")).map_err(io)?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, bytes).map_err(io)?;
        std::fs::rename(&tmp, &path).map_err(io)?;
    }
    Ok(rel)
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", &uuid::Uuid::new_v4().simple().to_string()[..16])
}

fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

fn ts(t: Timestamp) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

fn json_str<T: Serialize>(v: &T) -> Result<String> {
    Ok(serde_json::to_string(v)?)
}

fn enum_str<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

fn decode<T: DeserializeOwned>(s: String) -> Result<T> {
    Ok(serde_json::from_str(&s)?)
}

fn workflow_of(task: &Task) -> Result<Workflow> {
    workflow::builtin(&task.workflow.name, task.workflow.version)
        .ok_or_else(|| StoreError::NotFound(format!("workflow {} v{}", task.workflow.name, task.workflow.version)))
}

// ----- row helpers; each takes the open transaction or connection -----

fn get_task(c: &Connection, id: &str) -> Result<Task> {
    c.query_row("SELECT record FROM tasks WHERE id = ?1", [id], |r| r.get::<_, String>(0))
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("task {id}")))
        .and_then(decode)
}

fn put_task(tx: &Transaction, v: &Validators, task: &Task, insert: bool) -> Result<()> {
    v.check(RecordKind::Task, task)?;
    let rec = json_str(task)?;
    if insert {
        tx.execute(
            "INSERT INTO tasks (id, state, record, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![task.id, task.state.as_str(), rec, ts(task.created_at), ts(task.updated_at)],
        )?;
    } else {
        tx.execute(
            "UPDATE tasks SET state = ?2, record = ?3, updated_at = ?4 WHERE id = ?1",
            params![task.id, task.state.as_str(), rec, ts(task.updated_at)],
        )?;
    }
    Ok(())
}

fn log_move(tx: &Transaction, task_id: &str, mv: &Move, attempt_id: Option<&str>, now: Timestamp) -> Result<()> {
    tx.execute(
        "INSERT INTO transitions (task_id, signal, from_state, to_state, reason, attempt_id, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![task_id, enum_str(&mv.signal), mv.from.as_str(), mv.to.as_str(), mv.reason, attempt_id, ts(now)],
    )?;
    Ok(())
}

fn apply(tx: &Transaction, v: &Validators, out: &Outcome, attempt_id: Option<&str>, now: Timestamp) -> Result<Move> {
    put_task(tx, v, &out.task, false)?;
    log_move(tx, &out.task.id, &out.mv, attempt_id, now)?;
    Ok(out.mv.clone())
}

fn get_attempt(c: &Connection, id: &str) -> Result<Attempt> {
    c.query_row("SELECT record FROM attempts WHERE id = ?1", [id], |r| r.get::<_, String>(0))
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("attempt {id}")))
        .and_then(decode)
}

fn put_attempt(tx: &Transaction, v: &Validators, a: &Attempt, insert: bool) -> Result<()> {
    v.check(RecordKind::Attempt, a)?;
    let rec = json_str(a)?;
    if insert {
        tx.execute(
            "INSERT INTO attempts (id, task_id, epoch, role, status, token_hash, record) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![a.id, a.task_id, a.epoch, enum_str(&a.role), enum_str(&a.status), a.token_hash, rec],
        )?;
    } else {
        tx.execute(
            "UPDATE attempts SET status = ?2, record = ?3 WHERE id = ?1",
            params![a.id, enum_str(&a.status), rec],
        )?;
    }
    Ok(())
}

fn attempts_of(c: &Connection, task_id: &str) -> Result<Vec<Attempt>> {
    let mut stmt = c.prepare("SELECT record FROM attempts WHERE task_id = ?1 ORDER BY rowid")?;
    let rows = stmt.query_map([task_id], |r| r.get::<_, String>(0))?;
    rows.map(|r| decode(r?)).collect()
}

/// Ends attempts in the given statuses. Used when a new worker supersedes
/// older ones, and when the task is cancelled or fails.
fn end_attempts(
    tx: &Transaction,
    v: &Validators,
    task_id: &str,
    roles: &[Role],
    from: &[AttemptStatus],
    to: AttemptStatus,
    now: Timestamp,
) -> Result<()> {
    for mut a in attempts_of(tx, task_id)? {
        if roles.contains(&a.role) && from.contains(&a.status) {
            a.status = to;
            a.ended_at = Some(now);
            put_attempt(tx, v, &a, false)?;
        }
    }
    Ok(())
}

fn authenticate(c: &Connection, attempt_id: &str, token: &str) -> Result<Attempt> {
    let attempt = get_attempt(c, attempt_id)?;
    if sha256_hex(token.as_bytes()) != attempt.token_hash {
        return Err(StoreError::BadToken(attempt_id.to_string()));
    }
    Ok(attempt)
}

fn evidence_of(c: &Connection, table: &str, task_id: &str) -> Result<Vec<Evidence>> {
    let mut stmt = c.prepare(&format!("SELECT record FROM {table} WHERE task_id = ?1 ORDER BY seq"))?;
    let rows = stmt.query_map([task_id], |r| r.get::<_, String>(0))?;
    rows.map(|r| decode(r?)).collect()
}

fn check_runs_of(c: &Connection, task_id: &str) -> Result<Vec<CheckRun>> {
    let mut stmt = c.prepare("SELECT record FROM check_runs WHERE task_id = ?1 ORDER BY seq")?;
    let rows = stmt.query_map([task_id], |r| r.get::<_, String>(0))?;
    rows.map(|r| decode(r?)).collect()
}

fn report_for(c: &Connection, task: &Task) -> Result<EvidenceReport> {
    let claims = evidence_of(c, "claims", &task.id)?;
    let assessments = evidence_of(c, "assessments", &task.id)?;
    let runs = check_runs_of(c, &task.id)?;
    Ok(evidence::evaluate(task, evidence::Records { claims: &claims, assessments: &assessments, runs: &runs }))
}

fn all_grants(c: &Connection) -> Result<Vec<Grant>> {
    let mut stmt = c.prepare("SELECT record FROM grants ORDER BY rowid")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.map(|r| decode(r?)).collect()
}

fn seen_event(c: &Connection, id: &str) -> Result<Option<Event>> {
    c.query_row("SELECT record FROM events WHERE id = ?1", [id], |r| r.get::<_, String>(0))
        .optional()?
        .map(decode)
        .transpose()
}

fn put_event(tx: &Transaction, v: &Validators, e: &Event) -> Result<()> {
    v.check(RecordKind::Event, e)?;
    tx.execute(
        "INSERT INTO events (id, task_id, attempt_id, type, received_at, record) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![e.id, e.task_id, e.attempt_id, e.kind.as_str(), ts(e.received_at), json_str(e)?],
    )?;
    Ok(())
}

fn put_operation(tx: &Transaction, v: &Validators, op: &Operation, insert: bool) -> Result<()> {
    v.check(RecordKind::Operation, op)?;
    let rec = json_str(op)?;
    if insert {
        tx.execute(
            "INSERT INTO operations (id, task_id, kind, state, record) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![op.id, op.task_id, enum_str(&op.kind), enum_str(&op.state), rec],
        )?;
    } else {
        tx.execute(
            "UPDATE operations SET state = ?2, record = ?3 WHERE id = ?1",
            params![op.id, enum_str(&op.state), rec],
        )?;
    }
    Ok(())
}

fn check_fault(fault: Option<Fault>) -> Result<()> {
    match fault {
        Some(Fault::CrashBeforeCommit) => std::process::abort(),
        Some(Fault::ErrorBeforeCommit) => Err(StoreError::Fault("error before commit")),
        None => Ok(()),
    }
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| StoreError::Invalid(format!("cannot create {}: {e}", dir.display())))?;
        }
        Store::init(Connection::open(path)?, path.parent().map(|d| d.join("artifacts")))
    }

    pub fn open_in_memory() -> Result<Store> {
        Store::init(Connection::open_in_memory()?, None)
    }

    fn init(conn: Connection, artifacts: Option<PathBuf>) -> Result<Store> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let mut store = Store { conn, validators: Validators::new()?, fault: None, artifacts };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self.conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let version = usize::try_from(version).unwrap_or(0);
        if version > MIGRATIONS.len() {
            return Err(StoreError::Invalid(format!(
                "store schema {version} is newer than this binary supports ({})",
                MIGRATIONS.len()
            )));
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn set_fault(&mut self, fault: Option<Fault>) {
        self.fault = fault;
    }

    fn begin(&mut self) -> Result<(Transaction<'_>, &Validators)> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Ok((tx, &self.validators))
    }

    pub fn validators(&self) -> &Validators {
        &self.validators
    }

    // ----- tasks -----

    pub fn create_task(&mut self, spec: TaskSpec, now: Timestamp) -> Result<Task> {
        let workflow = workflow::latest(&spec.workflow)
            .ok_or_else(|| StoreError::NotFound(format!("workflow {}", spec.workflow)))?;
        let task = lifecycle::create(spec, &workflow, now)?;
        let (tx, v) = self.begin()?;
        if tx.query_row("SELECT 1 FROM tasks WHERE id = ?1", [&task.id], |_| Ok(())).optional()?.is_some() {
            return Err(StoreError::Invalid(format!("task {} already exists", task.id)));
        }
        put_task(&tx, v, &task, true)?;
        tx.commit()?;
        Ok(task)
    }

    pub fn task(&self, id: &str) -> Result<Task> {
        get_task(&self.conn, id)
    }

    pub fn tasks(&self) -> Result<Vec<Task>> {
        let mut stmt = self.conn.prepare("SELECT record FROM tasks ORDER BY created_at, id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| decode(r?)).collect()
    }

    /// G1.
    pub fn ready(&mut self, task_id: &str, snapshot: Snapshot, now: Timestamp) -> Result<Move> {
        let (tx, v) = self.begin()?;
        let task = get_task(&tx, task_id)?;
        let mut deps = Vec::new();
        for d in &task.dependencies {
            if let Ok(t) = get_task(&tx, d) {
                deps.push((d.clone(), t.state));
            }
        }
        let out = lifecycle::ready(&task, &deps, snapshot, now)?;
        let mv = apply(&tx, v, &out, None, now)?;
        tx.commit()?;
        Ok(mv)
    }

    /// G2 for workers; verifier attempts open without a state change.
    pub fn start_attempt(&mut self, req: StartAttempt, now: Timestamp) -> Result<Started> {
        let (tx, v) = self.begin()?;
        let task = get_task(&tx, &req.task_id)?;
        let workflow = workflow_of(&task)?;
        let grant =
            grants::effective(&task.id, req.profile, &all_grants(&tx)?, &req.host_policy, workflow.role(req.role), now);
        let attempt_id = new_id("att");
        let start = match req.role {
            Role::Worker => {
                lifecycle::start_worker(&task, &workflow, req.mode, &req.capabilities, &grant, &attempt_id, now)?
            }
            Role::Verifier | Role::Reviewer => {
                lifecycle::start_verifier(&task, &workflow, req.mode, &req.capabilities, &grant, now)?
            }
        };
        let started = match start {
            Start::Blocked(out) => Started::Blocked { moved: apply(&tx, v, &out, None, now)? },
            Start::Allowed { epoch, outcome, fallbacks } => {
                if req.role == Role::Worker {
                    end_attempts(
                        &tx,
                        v,
                        &task.id,
                        &[Role::Worker],
                        &[AttemptStatus::Running, AttemptStatus::Submitted],
                        AttemptStatus::Superseded,
                        now,
                    )?;
                }
                let token = new_token();
                let attempt = Attempt {
                    id: attempt_id,
                    task_id: task.id.clone(),
                    epoch,
                    role: req.role,
                    host: req.host,
                    agent: req.agent,
                    model: req.model,
                    worktree: req.worktree,
                    effective_grant: grant,
                    token_hash: sha256_hex(token.as_bytes()),
                    status: AttemptStatus::Running,
                    started_at: now,
                    ended_at: None,
                };
                put_attempt(&tx, v, &attempt, true)?;
                let moved = match &outcome {
                    Some(out) => Some(apply(&tx, v, out, Some(&attempt.id), now)?),
                    None => None,
                };
                Started::Yes { attempt, token, fallbacks, moved }
            }
        };
        tx.commit()?;
        Ok(started)
    }

    /// G3. Results from anything but the current attempt and epoch are stored as superseded.
    pub fn submit_result(&mut self, req: SubmitResult, now: Timestamp) -> Result<Applied<ResultApplied>> {
        let fault = self.fault;
        let (tx, v) = self.begin()?;
        let event_id = req.event_id.clone().unwrap_or_else(|| new_id("evt"));
        if let Some(seen) = seen_event(&tx, &event_id)? {
            let outcome = serde_json::from_value(seen.outcome.unwrap_or_default())?;
            return Ok(Applied { event_id, duplicate: true, outcome });
        }
        if req.epoch == 0 {
            return Err(StoreError::Invalid("epochs start at 1".into()));
        }
        let mut attempt = authenticate(&tx, &req.attempt_id, &req.token)?;
        let task = get_task(&tx, &attempt.task_id)?;
        let decision = lifecycle::submit_result(&task, &attempt, req.epoch, &req.output_tree, &req.changed_paths, now)?;
        let (status, superseded_reason) = match &decision {
            Submission::Accepted(_) => (ResultStatus::Accepted, None),
            Submission::Superseded { reason } => (ResultStatus::Superseded, Some(reason.clone())),
            Submission::Rejected { reason } => (ResultStatus::Rejected, Some(reason.clone())),
        };
        let result = TaskResult {
            id: new_id("res"),
            task_id: task.id.clone(),
            attempt_id: attempt.id.clone(),
            epoch: req.epoch,
            output_tree: req.output_tree,
            changed_paths: req.changed_paths,
            summary: req.summary,
            open_questions: req.open_questions,
            status,
            superseded_reason,
            received_at: now,
        };
        v.check(RecordKind::Result, &result)?;
        tx.execute(
            "INSERT INTO results (id, task_id, attempt_id, epoch, status, record) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                result.id,
                result.task_id,
                result.attempt_id,
                result.epoch,
                enum_str(&result.status),
                json_str(&result)?
            ],
        )?;
        let moved = match decision {
            Submission::Accepted(out) => {
                attempt.status = AttemptStatus::Submitted;
                put_attempt(&tx, v, &attempt, false)?;
                Some(apply(&tx, v, &out, Some(&attempt.id), now)?)
            }
            Submission::Superseded { .. } | Submission::Rejected { .. } => None,
        };
        let outcome = ResultApplied { result, moved };
        check_fault(fault)?;
        put_event(
            &tx,
            v,
            &Event {
                id: event_id.clone(),
                task_id: task.id,
                attempt_id: Some(attempt.id),
                epoch: Some(req.epoch),
                kind: EventKind::ResultSubmitted,
                payload_ref: None,
                received_at: now,
                acknowledged: true,
                outcome: Some(serde_json::to_value(&outcome)?),
            },
        )?;
        tx.commit()?;
        Ok(Applied { event_id, duplicate: false, outcome })
    }

    /// Records a worker claim or a verifier assessment. Append-only.
    pub fn add_evidence(
        &mut self,
        kind: EvidenceKind,
        req: AddEvidence,
        now: Timestamp,
    ) -> Result<Applied<EvidenceApplied>> {
        let fault = self.fault;
        let (tx, v) = self.begin()?;
        let event_id = req.event_id.clone().unwrap_or_else(|| new_id("evt"));
        if let Some(seen) = seen_event(&tx, &event_id)? {
            let outcome = serde_json::from_value(seen.outcome.unwrap_or_default())?;
            return Ok(Applied { event_id, duplicate: true, outcome });
        }
        let attempt = authenticate(&tx, &req.attempt_id, &req.token)?;
        let task = get_task(&tx, &attempt.task_id)?;
        lifecycle::check_evidence_source(&task, &attempt, kind, &req.criterion_id)?;
        let criterion = task
            .criteria
            .iter()
            .find(|c| c.id == req.criterion_id)
            .ok_or_else(|| StoreError::NotFound(format!("criterion {}", req.criterion_id)))?;
        let record = Evidence {
            id: new_id(if kind == EvidenceKind::Claim { "clm" } else { "asm" }),
            task_id: task.id.clone(),
            criterion_id: criterion.id.clone(),
            attempt_id: attempt.id.clone(),
            strength: req.strength,
            currency: Currency {
                tree: req.tree,
                check_version: criterion.check_version.clone(),
                environment: req.environment.unwrap_or_else(|| task.environment.clone()),
                policy_digest: task.policy_digest.clone(),
            },
            evidence_refs: req.evidence_refs,
            note: req.note,
            recorded_at: now,
        };
        let (table, record_kind, event_kind) = match kind {
            EvidenceKind::Claim => ("claims", RecordKind::Claim, EventKind::ClaimAdded),
            EvidenceKind::Assessment => ("assessments", RecordKind::Assessment, EventKind::AssessmentAdded),
        };
        v.check(record_kind, &record)?;
        tx.execute(
            &format!("INSERT INTO {table} (id, task_id, criterion_id, attempt_id, record) VALUES (?1, ?2, ?3, ?4, ?5)"),
            params![record.id, record.task_id, record.criterion_id, record.attempt_id, json_str(&record)?],
        )?;
        let current =
            evidence::currency_for(&task, criterion).map(|key| evidence::same_currency(&key, &record.currency));
        let outcome = EvidenceApplied { evidence: record, current };
        check_fault(fault)?;
        put_event(
            &tx,
            v,
            &Event {
                id: event_id.clone(),
                task_id: task.id,
                attempt_id: Some(attempt.id),
                epoch: Some(attempt.epoch),
                kind: event_kind,
                payload_ref: None,
                received_at: now,
                acknowledged: true,
                outcome: Some(serde_json::to_value(&outcome)?),
            },
        )?;
        tx.commit()?;
        Ok(Applied { event_id, duplicate: false, outcome })
    }

    /// Supervisor path for restart recovery: ends every attempt a previous
    /// controller left running as failed, each with a synthetic failure report.
    pub fn reconcile_running(&mut self, task_id: &str, reason: &str, now: Timestamp) -> Result<Vec<Attempt>> {
        let (tx, v) = self.begin()?;
        let mut ended = Vec::new();
        for mut attempt in attempts_of(&tx, task_id)? {
            if attempt.status != AttemptStatus::Running {
                continue;
            }
            attempt.status = AttemptStatus::Failed;
            attempt.ended_at = Some(now);
            put_attempt(&tx, v, &attempt, false)?;
            put_event(
                &tx,
                v,
                &Event {
                    id: new_id("evt"),
                    task_id: task_id.to_string(),
                    attempt_id: Some(attempt.id.clone()),
                    epoch: Some(attempt.epoch),
                    kind: EventKind::AttemptFailed,
                    payload_ref: None,
                    received_at: now,
                    acknowledged: true,
                    outcome: Some(serde_json::json!({ "synthetic": true, "reason": reason })),
                },
            )?;
            ended.push(attempt);
        }
        tx.commit()?;
        Ok(ended)
    }

    /// Marks an attempt finished. A worker that ends without a result has its
    /// end recorded so nothing is left running.
    pub fn end_attempt(
        &mut self,
        attempt_id: &str,
        token: &str,
        failed: bool,
        note: Option<String>,
        now: Timestamp,
    ) -> Result<Attempt> {
        let (tx, v) = self.begin()?;
        let mut attempt = authenticate(&tx, attempt_id, token)?;
        if matches!(attempt.status, AttemptStatus::Running | AttemptStatus::Submitted) {
            attempt.status = if failed { AttemptStatus::Failed } else { AttemptStatus::Completed };
            attempt.ended_at = Some(now);
            put_attempt(&tx, v, &attempt, false)?;
            put_event(
                &tx,
                v,
                &Event {
                    id: new_id("evt"),
                    task_id: attempt.task_id.clone(),
                    attempt_id: Some(attempt.id.clone()),
                    epoch: Some(attempt.epoch),
                    kind: if failed { EventKind::AttemptFailed } else { EventKind::AttemptCompleted },
                    payload_ref: None,
                    received_at: now,
                    acknowledged: true,
                    outcome: note.map(serde_json::Value::String),
                },
            )?;
        }
        tx.commit()?;
        Ok(attempt)
    }

    /// Records interlock's own run of a criterion's check. The store derives
    /// everything trust depends on: the producer from the attempt's role, the
    /// command and check version from the criterion, and whether the output
    /// shows the check ran nothing. The caller supplies only what happened.
    pub fn record_check_run(&mut self, req: NewCheckRun, now: Timestamp) -> Result<CheckRun> {
        let artifacts = self.artifacts.clone();
        let (tx, v) = self.begin()?;
        let task = get_task(&tx, &req.task_id)?;
        let criterion = task
            .criteria
            .iter()
            .find(|c| c.id == req.criterion_id)
            .ok_or_else(|| StoreError::NotFound(format!("criterion {}", req.criterion_id)))?;
        let command = criterion
            .check
            .clone()
            .ok_or_else(|| StoreError::Invalid(format!("criterion {} has no check to run", criterion.id)))?;
        let (attempt_id, producer) = match &req.attempt {
            Some((id, token)) => {
                let attempt = authenticate(&tx, id, token)?;
                if attempt.task_id != task.id {
                    return Err(StoreError::Invalid("the attempt belongs to another task".into()));
                }
                if !matches!(attempt.status, AttemptStatus::Running | AttemptStatus::Submitted) {
                    return Err(StoreError::Invalid(format!("attempt {} is {:?}", attempt.id, attempt.status)));
                }
                let producer = match attempt.role {
                    Role::Worker => RunProducer::Worker,
                    Role::Verifier | Role::Reviewer => RunProducer::Verifier,
                };
                (Some(attempt.id), producer)
            }
            None => (None, RunProducer::Operator),
        };
        let text = String::from_utf8_lossy(&req.output);
        let output_ref = match &artifacts {
            Some(dir) => Some(write_artifact(dir, &req.output)?),
            None => None,
        };
        let tail_start = text.len().saturating_sub(2000);
        let tail_start = (tail_start..text.len()).find(|i| text.is_char_boundary(*i)).unwrap_or(text.len());
        let run = CheckRun {
            id: new_id("run"),
            task_id: task.id.clone(),
            criterion_id: criterion.id.clone(),
            attempt_id,
            producer,
            target: req.target,
            tree: req.tree,
            check_version: criterion.check_version.clone(),
            environment: task.environment.clone(),
            command,
            exit_code: req.exit_code,
            timed_out: req.timed_out,
            vacuous: interlock_core::vacuity::detect(&text).map(str::to_string),
            duration_ms: req.duration_ms,
            output_ref,
            output_tail: text[tail_start..].to_string(),
            recorded_at: now,
        };
        v.check(RecordKind::CheckRun, &run)?;
        tx.execute(
            "INSERT INTO check_runs (id, task_id, criterion_id, attempt_id, target, record) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![run.id, run.task_id, run.criterion_id, run.attempt_id, enum_str(&run.target), json_str(&run)?],
        )?;
        tx.commit()?;
        Ok(run)
    }

    pub fn check_runs(&self, task_id: &str) -> Result<Vec<CheckRun>> {
        check_runs_of(&self.conn, task_id)
    }

    pub fn evaluate(&self, task_id: &str) -> Result<(Task, EvidenceReport)> {
        let task = get_task(&self.conn, task_id)?;
        let report = report_for(&self.conn, &task)?;
        Ok((task, report))
    }

    /// Applies every automatic move the evidence allows (G4, R1, R2, G7, or
    /// failure when the budget is spent), in one transaction.
    pub fn advance(&mut self, task_id: &str, now: Timestamp) -> Result<Vec<Move>> {
        let (tx, v) = self.begin()?;
        let mut moves = Vec::new();
        for _ in 0..4 {
            let task = get_task(&tx, task_id)?;
            let report = report_for(&tx, &task)?;
            let Some(out) = lifecycle::advance(&task, &report, now) else { break };
            if out.mv.from == State::AwaitingVerification {
                end_attempts(
                    &tx,
                    v,
                    task_id,
                    &[Role::Verifier, Role::Reviewer],
                    &[AttemptStatus::Running],
                    AttemptStatus::Completed,
                    now,
                )?;
            }
            if out.mv.to == State::Failed {
                end_attempts(
                    &tx,
                    v,
                    task_id,
                    &[Role::Worker],
                    &[AttemptStatus::Running, AttemptStatus::Submitted],
                    AttemptStatus::Completed,
                    now,
                )?;
            }
            moves.push(apply(&tx, v, &out, None, now)?);
        }
        tx.commit()?;
        Ok(moves)
    }

    /// R3: ends the current worker attempt as cancelled and returns the task to
    /// ready, so a respawned attempt gets the next epoch.
    pub fn retry(&mut self, task_id: &str, reason: &str, now: Timestamp) -> Result<Move> {
        let (tx, v) = self.begin()?;
        let task = get_task(&tx, task_id)?;
        let out = lifecycle::retry(&task, reason, now)?;
        end_attempts(&tx, v, task_id, &[Role::Worker], &[AttemptStatus::Running], AttemptStatus::Cancelled, now)?;
        let mv = apply(&tx, v, &out, task.current_attempt.as_deref(), now)?;
        tx.commit()?;
        Ok(mv)
    }

    /// The code under evidence changed. The next `advance` re-verifies through R2.
    pub fn record_new_tree(&mut self, task_id: &str, tree: &str, now: Timestamp) -> Result<Task> {
        let (tx, v) = self.begin()?;
        let task = lifecycle::record_new_tree(&get_task(&tx, task_id)?, tree, now)?;
        put_task(&tx, v, &task, false)?;
        tx.commit()?;
        Ok(task)
    }

    pub fn block(&mut self, task_id: &str, reason: &str, now: Timestamp) -> Result<Move> {
        let (tx, v) = self.begin()?;
        let out = lifecycle::block(&get_task(&tx, task_id)?, reason, now)?;
        let mv = apply(&tx, v, &out, None, now)?;
        tx.commit()?;
        Ok(mv)
    }

    pub fn unblock(&mut self, task_id: &str, now: Timestamp) -> Result<Move> {
        let (tx, v) = self.begin()?;
        let out = lifecycle::unblock(&get_task(&tx, task_id)?, now)?;
        let mv = apply(&tx, v, &out, None, now)?;
        tx.commit()?;
        Ok(mv)
    }

    /// Fails or cancels the task and ends every attempt still open.
    pub fn stop(&mut self, task_id: &str, cancel: bool, reason: &str, now: Timestamp) -> Result<Move> {
        let (tx, v) = self.begin()?;
        let task = get_task(&tx, task_id)?;
        let out = if cancel { lifecycle::cancel(&task, reason, now)? } else { lifecycle::fail(&task, reason, now)? };
        end_attempts(
            &tx,
            v,
            task_id,
            &[Role::Worker, Role::Verifier, Role::Reviewer],
            &[AttemptStatus::Running, AttemptStatus::Submitted],
            AttemptStatus::Cancelled,
            now,
        )?;
        let mv = apply(&tx, v, &out, None, now)?;
        tx.commit()?;
        Ok(mv)
    }

    /// G5. The operation row is written before any call to the forge.
    pub fn begin_integration(
        &mut self,
        task_id: &str,
        kind: OperationKind,
        mut intent: OperationIntent,
        now: Timestamp,
    ) -> Result<(Move, Option<Operation>)> {
        let (tx, v) = self.begin()?;
        let task = get_task(&tx, task_id)?;
        let report = report_for(&tx, &task)?;
        let landing = grants::landing_authority(&task.id, &all_grants(&tx)?, now);
        let out = lifecycle::begin_integration(&task, &report, landing, now)?;
        let op = if out.task.state == State::Integrating {
            if intent.expected_head_sha.is_none() {
                intent.expected_head_sha = task.current_tree.clone();
            }
            let op = Operation {
                id: new_id("op"),
                task_id: task.id.clone(),
                kind,
                intent,
                state: OperationState::Planned,
                outcome: None,
                created_at: now,
                updated_at: now,
            };
            put_operation(&tx, v, &op, true)?;
            Some(op)
        } else {
            None
        };
        let mv = apply(&tx, v, &out, None, now)?;
        tx.commit()?;
        Ok((mv, op))
    }

    /// G6, or R2 when the forge refused the pinned merge.
    pub fn confirm_integration(
        &mut self,
        task_id: &str,
        operation_id: &str,
        report: MergeReport,
        now: Timestamp,
    ) -> Result<Move> {
        let (tx, v) = self.begin()?;
        let task = get_task(&tx, task_id)?;
        let mut op: Operation = tx
            .query_row("SELECT record FROM operations WHERE id = ?1", [operation_id], |r| r.get::<_, String>(0))
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("operation {operation_id}")))
            .and_then(decode)?;
        let out = lifecycle::confirm_integration(&task, &op, &report, now)?;
        op.state = match report {
            MergeReport::Merged { .. } => OperationState::Confirmed,
            MergeReport::Refused { .. } => OperationState::Failed,
        };
        op.outcome = Some(serde_json::to_value(&report)?);
        op.updated_at = now;
        put_operation(&tx, v, &op, false)?;
        let mv = apply(&tx, v, &out, None, now)?;
        tx.commit()?;
        Ok(mv)
    }

    // ----- grants: the operator path only -----

    pub fn create_grant(&mut self, req: NewGrant, now: Timestamp) -> Result<Grant> {
        let grant = Grant {
            id: new_id("grant"),
            principal: req.principal,
            task_scope: req.task_scope,
            action_classes: req.action_classes,
            tools: req.tools,
            landing_authority: req.landing_authority,
            origin: req.origin,
            created_at: now,
            expires_at: req.expires_at,
            revoked_at: None,
        };
        grants::validate(&grant).map_err(StoreError::Invalid)?;
        let (tx, v) = self.begin()?;
        v.check(RecordKind::Grant, &grant)?;
        tx.execute("INSERT INTO grants (id, record) VALUES (?1, ?2)", params![grant.id, json_str(&grant)?])?;
        tx.commit()?;
        Ok(grant)
    }

    pub fn revoke_grant(&mut self, id: &str, now: Timestamp) -> Result<Grant> {
        let (tx, v) = self.begin()?;
        let mut grant: Grant = tx
            .query_row("SELECT record FROM grants WHERE id = ?1", [id], |r| r.get::<_, String>(0))
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("grant {id}")))
            .and_then(decode)?;
        if grant.revoked_at.is_none() {
            grant.revoked_at = Some(now);
            v.check(RecordKind::Grant, &grant)?;
            tx.execute("UPDATE grants SET record = ?2 WHERE id = ?1", params![grant.id, json_str(&grant)?])?;
        }
        tx.commit()?;
        Ok(grant)
    }

    pub fn grants(&self) -> Result<Vec<Grant>> {
        all_grants(&self.conn)
    }

    // ----- reads -----

    pub fn attempts(&self, task_id: &str) -> Result<Vec<Attempt>> {
        attempts_of(&self.conn, task_id)
    }

    pub fn attempt(&self, id: &str) -> Result<Attempt> {
        get_attempt(&self.conn, id)
    }

    pub fn results(&self, task_id: &str) -> Result<Vec<TaskResult>> {
        let mut stmt = self.conn.prepare("SELECT record FROM results WHERE task_id = ?1 ORDER BY seq")?;
        let rows = stmt.query_map([task_id], |r| r.get::<_, String>(0))?;
        rows.map(|r| decode(r?)).collect()
    }

    pub fn claims(&self, task_id: &str) -> Result<Vec<Evidence>> {
        evidence_of(&self.conn, "claims", task_id)
    }

    pub fn assessments(&self, task_id: &str) -> Result<Vec<Evidence>> {
        evidence_of(&self.conn, "assessments", task_id)
    }

    pub fn transitions(&self, task_id: &str) -> Result<Vec<TransitionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, signal, from_state, to_state, reason, attempt_id, at FROM transitions WHERE task_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([task_id], |r| {
            Ok(TransitionRow {
                seq: r.get(0)?,
                signal: r.get(1)?,
                from: r.get(2)?,
                to: r.get(3)?,
                reason: r.get(4)?,
                attempt_id: r.get(5)?,
                at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn event_count(&self, task_id: &str) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM events WHERE task_id = ?1", [task_id], |r| r.get(0))?)
    }

    /// A fresh brief built from records, for starting or resuming work.
    pub fn brief(
        &self,
        task_id: &str,
        role: Role,
        profile: Profile,
        host_policy: &HostPolicy,
        now: Timestamp,
    ) -> Result<Brief> {
        let (task, report) = self.evaluate(task_id)?;
        let workflow = workflow_of(&task)?;
        let grant = grants::effective(&task.id, profile, &self.grants()?, host_policy, workflow.role(role), now);
        let results = self.results(task_id)?;
        let last = results.iter().rev().find(|r| r.status == ResultStatus::Accepted);
        let rejected = results.iter().rev().find(|r| r.status == ResultStatus::Rejected);
        let runs = self.check_runs(task_id)?;
        Ok(brief::build(&task, role, &report, last, rejected, &runs, Some(&grant)))
    }

    pub fn save_host_report(&mut self, host: &str, record: &serde_json::Value, now: Timestamp) -> Result<()> {
        self.conn.execute(
            "INSERT INTO host_capabilities (host, record, inspected_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(host) DO UPDATE SET record = excluded.record, inspected_at = excluded.inspected_at",
            params![host, json_str(record)?, ts(now)],
        )?;
        Ok(())
    }

    pub fn host_report(&self, host: &str) -> Result<Option<serde_json::Value>> {
        self.conn
            .query_row("SELECT record FROM host_capabilities WHERE host = ?1", [host], |r| r.get::<_, String>(0))
            .optional()?
            .map(decode)
            .transpose()
    }

    /// Raw access for tests that try to bypass the rules.
    #[doc(hidden)]
    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}
