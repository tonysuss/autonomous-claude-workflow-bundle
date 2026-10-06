//! `interlock`: agents propose moves, and this command checks each one against
//! recorded evidence, the current attempt and granted authority. Output is JSON.

mod forge;
mod skills_cmd;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Duration, Utc};
use clap::{Args, Parser, Subcommand, ValueEnum};
use interlock_adapter::{Host, HostReport, Probe};
use interlock_core::capability::{Capability, CapabilitySet};
use interlock_core::classify::classify_shell;
use interlock_core::grants::{self, HostPolicy, Profile};
use interlock_core::lifecycle::{EvidenceKind, MergeReport, TaskSpec, next_moves};
use interlock_core::workflow::{self, Mode};
use interlock_schema::*;
use interlock_store::*;
use serde_json::{Value, json};

#[derive(Parser)]
#[command(name = "interlock", version, about = "Lets work advance only on evidence")]
struct Cli {
    /// Store location. Defaults to .interlock/state.db at the repository root.
    #[arg(long, global = true, env = "INTERLOCK_DB")]
    db: Option<PathBuf>,
    /// What is allowed without an explicit grant.
    #[arg(long, global = true, env = "INTERLOCK_PROFILE", value_enum, default_value = "conservative")]
    profile: ProfileArg,
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum ProfileArg {
    Conservative,
    Permissive,
}

#[derive(Subcommand)]
enum Command {
    /// Create the store.
    Init,
    /// Where this directory's store is, found exactly as every other command finds it (--db,
    /// INTERLOCK_DB, an attempt's worktree, the repository root). Creates nothing. Exits 0 when the
    /// store exists, 3 when it does not.
    Where,
    /// Create, inspect and move tasks.
    #[command(subcommand)]
    Task(TaskCmd),
    /// Open and end attempts.
    #[command(subcommand)]
    Attempt(AttemptCmd),
    /// Submit a worker's result for the current attempt.
    #[command(subcommand)]
    Result(ResultCmd),
    /// Record a worker claim against one criterion.
    #[command(subcommand)]
    Claim(EvidenceCmd),
    /// Record a verifier assessment against one criterion.
    #[command(subcommand)]
    Assess(EvidenceCmd),
    /// State, missing evidence and the next allowed moves.
    Status { task: String },
    /// Apply every move the evidence allows now (G4, R1, R2, G7).
    Advance { task: String },
    /// A fresh brief built from records, for starting or resuming work.
    Brief {
        task: String,
        #[arg(long, value_enum, default_value = "worker")]
        role: RoleArg,
        #[arg(long, value_enum, default_value = "markdown")]
        format: Format,
    },
    /// Hand verified work to the forge (G5) and record what it did (G6).
    #[command(subcommand)]
    Integrate(IntegrateCmd),
    /// Ask the forge what happened to every operation nobody confirmed, and settle each one.
    Reconcile {
        /// One task; all tasks when left out.
        task: Option<String>,
    },
    /// Operator only: create, revoke and list grants.
    #[command(subcommand)]
    Grant(GrantCmd),
    /// Inspect hosts and translate policies into their patterns.
    #[command(subcommand)]
    Host(HostCmd),
    /// Decide one proposed action against an attempt's effective grant.
    #[command(subcommand)]
    Policy(PolicyCmd),
    /// Print or check record schemas.
    #[command(subcommand)]
    Schema(SchemaCmd),
    /// List the built-in workflows.
    Workflows,
    /// Drive a task through headless worker and verifier sessions on a host.
    Run {
        task: String,
        /// copilot or claude-code.
        #[arg(long)]
        host: String,
        #[arg(long)]
        model: Option<String>,
        /// Per-session limit, for example 20m.
        #[arg(long, default_value = "20m")]
        timeout: String,
        #[arg(long, default_value_t = 80)]
        max_turns: u32,
        /// Sessions this run may start, across workers and verifiers. 0 runs
        /// the baseline checks only.
        #[arg(long, default_value_t = 6)]
        max_sessions: u32,
        /// Keep each attempt's worktree for inspection.
        #[arg(long)]
        keep_worktrees: bool,
        /// Declare capabilities instead of inspecting the host (comma-separated).
        #[arg(long, value_delimiter = ',')]
        capabilities: Option<Vec<String>>,
        /// Reasoning effort for every session: Claude Code's --effort, Copilot CLI's --reasoning-effort.
        #[arg(long)]
        effort: Option<String>,
        /// A skills plugin (from `interlock skills generate`) to load into every session.
        #[arg(long)]
        skills: Option<PathBuf>,
    },
    /// interlock runs a criterion's check itself and records what happened.
    #[command(subcommand)]
    Check(CheckCmd),
    /// Called by interlock's hooks plugin with the host's payload on stdin.
    Hook {
        #[arg(value_enum)]
        event: HookArg,
    },
    /// Generate, list and validate the interlock skills for a host.
    #[command(subcommand)]
    Skills(skills_cmd::SkillsCmd),
    /// Install the skills, the verifier agent and the guided hooks for a host in this repository.
    Setup {
        /// copilot or claude-code.
        #[arg(long)]
        host: String,
        /// Read canonical skills from this interlock/ directory instead of the built-in copy.
        #[arg(long)]
        from: Option<PathBuf>,
        /// Overwrite files interlock did not generate, or that changed since it did.
        #[arg(long)]
        force: bool,
    },
    /// Launch an independent verifier session for a task awaiting verification.
    Verify {
        task: String,
        /// copilot or claude-code.
        #[arg(long)]
        host: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value = "20m")]
        timeout: String,
        #[arg(long, default_value_t = 60)]
        max_turns: u32,
        /// Reasoning effort for the session: Claude Code's --effort, Copilot CLI's --reasoning-effort.
        #[arg(long)]
        effort: Option<String>,
        /// A skills plugin (from `interlock skills generate`) to load into the session.
        #[arg(long)]
        skills: Option<PathBuf>,
    },
    /// Notes kept on a task: designs, reviews, answers.
    #[command(subcommand)]
    Note(NoteCmd),
}

#[derive(Subcommand)]
enum CheckCmd {
    /// Run a criterion's check on the current worktree (or the task's input snapshot) and record it.
    Run {
        #[arg(long)]
        criterion: String,
        #[arg(long, value_enum, default_value = "output")]
        target: TargetArg,
        /// Needed only outside an attempt.
        #[arg(long)]
        task: Option<String>,
        /// Record the run as the operator's, outside any attempt. Agents may not.
        #[arg(long)]
        operator: bool,
        #[arg(long, default_value = "10m")]
        timeout: String,
        #[arg(long, env = "INTERLOCK_ATTEMPT")]
        attempt: Option<String>,
        #[arg(long, env = "INTERLOCK_TOKEN")]
        token: Option<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum TargetArg {
    Output,
    Base,
}

#[derive(Clone, Copy, ValueEnum)]
enum HookArg {
    PreToolUse,
    Stop,
}

#[derive(Subcommand)]
enum TaskCmd {
    /// Create a task from a TOML or JSON file ("-" reads JSON from stdin).
    Create {
        file: PathBuf,
    },
    Show {
        task: String,
    },
    List,
    /// G1: record the input snapshot once dependencies are done.
    Ready {
        task: String,
        #[arg(long)]
        base: String,
        #[arg(long)]
        repository: Option<String>,
        #[arg(long)]
        untracked_hash: Option<String>,
    },
    /// Record that the code under evidence changed, for example after a rebase.
    Tree {
        task: String,
        #[arg(long)]
        tree: String,
    },
    Block {
        task: String,
        #[arg(long)]
        reason: String,
    },
    Unblock {
        task: String,
    },
    /// R3: end the current worker attempt so a fresh one can start.
    Retry {
        task: String,
        #[arg(long)]
        reason: String,
    },
    Fail {
        task: String,
        #[arg(long)]
        reason: String,
    },
    Cancel {
        task: String,
        #[arg(long)]
        reason: String,
    },
    /// The task's transition log.
    Log {
        task: String,
    },
    /// The task's events: results, evidence, and how each attempt's session ended.
    Events {
        task: String,
    },
    /// Pause safely: commit the current worker's worktree as a `wip:` commit on
    /// interlock/wip/<task>, with a resume note built from records.
    Export {
        task: String,
    },
    /// Make an export the next worker's starting point. The task must be ready.
    Resume {
        task: String,
        /// A commit or ref; defaults to interlock/wip/<task>.
        #[arg(long)]
        from: Option<String>,
    },
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum RoleArg {
    Worker,
    Verifier,
    Reviewer,
}

impl From<RoleArg> for Role {
    fn from(r: RoleArg) -> Role {
        match r {
            RoleArg::Worker => Role::Worker,
            RoleArg::Verifier => Role::Verifier,
            RoleArg::Reviewer => Role::Reviewer,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum ModeArg {
    Interactive,
    Headless,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Markdown,
    Json,
}

#[derive(Subcommand)]
enum AttemptCmd {
    /// Open an attempt. Prints the attempt and its token, shown only once.
    Start {
        task: String,
        #[arg(long, value_enum)]
        role: RoleArg,
        /// Host running the attempt, for example copilot or claude-code.
        #[arg(long)]
        host: String,
        #[arg(long, value_enum, default_value = "interactive")]
        mode: ModeArg,
        /// Declare capabilities instead of inspecting the host (comma-separated).
        #[arg(long, value_delimiter = ',')]
        capabilities: Option<Vec<String>>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        worktree: Option<String>,
    },
    /// Mark an attempt finished.
    End {
        attempt: String,
        #[arg(long, env = "INTERLOCK_TOKEN")]
        token: String,
        #[arg(long)]
        failed: bool,
        #[arg(long)]
        note: Option<String>,
    },
    List {
        task: String,
    },
}

#[derive(Args)]
struct Auth {
    #[arg(long, env = "INTERLOCK_ATTEMPT")]
    attempt: String,
    #[arg(long, env = "INTERLOCK_TOKEN")]
    token: String,
    /// Delivery id. Repeating an id is a no-op that returns the first outcome.
    #[arg(long)]
    event_id: Option<String>,
}

#[derive(Subcommand)]
enum ResultCmd {
    Submit {
        #[command(flatten)]
        auth: Auth,
        #[arg(long)]
        epoch: u32,
        /// The git tree id of the output; `auto` records the attempt's worktree.
        /// interlock reads the changed files from this tree itself.
        #[arg(long)]
        tree: String,
        #[arg(long, default_value = "")]
        summary: String,
        #[arg(long = "question")]
        questions: Vec<String>,
    },
}

#[derive(Subcommand)]
enum NoteCmd {
    /// Keep a note on a task: a design, a review, an answer.
    Add {
        #[arg(long)]
        task: String,
        /// For example design, review or answer.
        #[arg(long)]
        kind: String,
        /// The note's text; `-` reads it from stdin.
        #[arg(long)]
        file: PathBuf,
    },
    /// A task's notes, oldest first.
    List {
        #[arg(long)]
        task: String,
    },
}

/// A move interlock refuses, reported with exit code 2 like the store's refusals.
#[derive(Debug)]
struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

#[derive(Subcommand)]
enum EvidenceCmd {
    Add {
        #[command(flatten)]
        auth: Auth,
        #[arg(long)]
        criterion: String,
        #[arg(long, value_enum)]
        strength: StrengthArg,
        /// The git tree id that was checked; `auto` computes the current worktree's.
        #[arg(long, env = "INTERLOCK_TREE")]
        tree: String,
        #[arg(long = "ref")]
        refs: Vec<String>,
        #[arg(long)]
        note: Option<String>,
        /// Where the check ran. Defaults to the task's environment.
        #[arg(long)]
        environment: Option<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum StrengthArg {
    Observed,
    Tested,
    Static,
    Blocked,
    Failed,
}

impl From<StrengthArg> for Strength {
    fn from(s: StrengthArg) -> Strength {
        match s {
            StrengthArg::Observed => Strength::Observed,
            StrengthArg::Tested => Strength::Tested,
            StrengthArg::Static => Strength::Static,
            StrengthArg::Blocked => Strength::Blocked,
            StrengthArg::Failed => Strength::Failed,
        }
    }
}

#[derive(Subcommand)]
enum IntegrateCmd {
    /// G5: write the operation before any call to the forge.
    Begin {
        task: String,
        #[arg(long)]
        pr: Option<u64>,
        #[arg(long)]
        base: Option<String>,
        /// Head to pin the merge to. Defaults to the verified tree.
        #[arg(long)]
        head: Option<String>,
    },
    /// G6, or R2 when the forge refused the pinned merge.
    Confirm {
        task: String,
        #[arg(long)]
        operation: String,
        #[arg(long, conflicts_with = "refused")]
        merged: Option<String>,
        #[arg(long)]
        refused: Option<String>,
    },
    /// G5, push, pull request, readiness, the merge pinned to the verified head, and G6.
    Run(forge::IntegrateRun),
    /// The task's forge operations, oldest first.
    Operations { task: String },
}

#[derive(Subcommand)]
enum GrantCmd {
    Create {
        #[arg(long)]
        principal: String,
        /// Task ids, or * for every task.
        #[arg(long, value_delimiter = ',', required = true)]
        tasks: Vec<String>,
        #[arg(long, value_delimiter = ',', value_enum, required = true)]
        classes: Vec<ClassArg>,
        #[arg(long, value_enum, default_value = "none")]
        landing: LandingArg,
        #[arg(long = "deny")]
        deny: Vec<String>,
        /// For example 8h or 30m.
        #[arg(long, conflicts_with = "expires_at")]
        expires_in: Option<String>,
        #[arg(long)]
        expires_at: Option<DateTime<Utc>>,
        /// Where the grant came from, such as the operator's own words.
        #[arg(long)]
        origin: String,
    },
    Revoke {
        grant: String,
    },
    List,
}

#[derive(Clone, Copy, ValueEnum)]
enum ClassArg {
    Read,
    LocalReversible,
    ExternalReversible,
    Landing,
    Irreversible,
}

impl From<ClassArg> for ActionClass {
    fn from(c: ClassArg) -> ActionClass {
        match c {
            ClassArg::Read => ActionClass::Read,
            ClassArg::LocalReversible => ActionClass::LocalReversible,
            ClassArg::ExternalReversible => ActionClass::ExternalReversible,
            ClassArg::Landing => ActionClass::Landing,
            ClassArg::Irreversible => ActionClass::Irreversible,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum LandingArg {
    None,
    Coordinator,
    Owner,
    Operator,
}

impl From<LandingArg> for LandingAuthority {
    fn from(l: LandingArg) -> LandingAuthority {
        match l {
            LandingArg::None => LandingAuthority::None,
            LandingArg::Coordinator => LandingAuthority::Coordinator,
            LandingArg::Owner => LandingAuthority::Owner,
            LandingArg::Operator => LandingAuthority::Operator,
        }
    }
}

#[derive(Subcommand)]
enum HostCmd {
    /// Report each host's version and capabilities.
    Inspect {
        /// A host name, or all.
        #[arg(long, default_value = "all")]
        host: String,
        /// Also run one small session to check the host can authenticate. Costs a model call.
        #[arg(long)]
        check_auth: bool,
        /// Save the report in the store for attempt start to use.
        #[arg(long)]
        save: bool,
    },
    /// Show an attempt's effective grant in a host's own permission patterns.
    Tools {
        task: String,
        #[arg(long, value_enum, default_value = "worker")]
        role: RoleArg,
        #[arg(long)]
        host: String,
    },
}

#[derive(Subcommand)]
enum PolicyCmd {
    /// Decide one action: allow, deny, or ask the operator.
    Check {
        #[arg(long, env = "INTERLOCK_ATTEMPT")]
        attempt: String,
        /// A shell command line.
        #[arg(long, conflicts_with = "tool")]
        shell: Option<String>,
        /// A host-neutral tool name such as edit or web.
        #[arg(long)]
        tool: Option<String>,
    },
}

#[derive(Subcommand)]
enum SchemaCmd {
    Show { kind: String },
    Validate { kind: String, file: PathBuf },
}

fn now() -> Result<DateTime<Utc>> {
    match std::env::var("INTERLOCK_NOW") {
        Ok(s) => Ok(DateTime::parse_from_rfc3339(&s).context("INTERLOCK_NOW")?.with_timezone(&Utc)),
        Err(_) => Ok(Utc::now()),
    }
}

fn default_db() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // An attempt's worktree under <repo>/.interlock/worktrees uses the repository's store.
    let worktrees = Path::new(".interlock").join("worktrees");
    if let Some(repo) = cwd.ancestors().find(|d| d.ends_with(&worktrees)).and_then(|d| d.parent()?.parent()) {
        return repo.join(".interlock").join("state.db");
    }
    let root = cwd.ancestors().find(|d| d.join(".git").exists()).unwrap_or(&cwd);
    root.join(".interlock").join("state.db")
}

fn absolute_db(cli: &Cli) -> Result<PathBuf> {
    let db = cli.db.clone().unwrap_or_else(default_db);
    Ok(if db.is_absolute() { db } else { std::env::current_dir()?.join(db) })
}

/// `interlock where`: the store the other commands would open from here, and whether it exists.
/// A skill asks this to decide whether the work is an interlock task; it must never create a store.
fn where_store(cli: &Cli) -> Result<ExitCode> {
    let store = absolute_db(cli)?;
    let exists = store.is_file();
    let repo = interlock_supervisor::guided::repo_of_store(&store).ok();
    println!("{}", serde_json::to_string_pretty(&json!({"store": store, "exists": exists, "repo": repo}))?);
    Ok(if exists { ExitCode::SUCCESS } else { ExitCode::from(3) })
}

fn open(cli: &Cli) -> Result<Store> {
    let path = cli.db.clone().unwrap_or_else(default_db);
    // Inside an attempt's worktree the store must already exist: never start a fresh one there.
    let cwd = std::env::current_dir().unwrap_or_default();
    let worktrees = Path::new(".interlock").join("worktrees");
    if cli.db.is_none() && !path.exists() && cwd.ancestors().any(|d| d.ends_with(&worktrees)) {
        bail!("no interlock store at {}; this directory is an attempt's worktree", path.display());
    }
    let mut store = Store::open(&path).with_context(|| format!("opening {}", path.display()))?;
    match std::env::var("INTERLOCK_FAULT").as_deref() {
        Ok("crash_before_commit") => store.set_fault(Some(Fault::CrashBeforeCommit)),
        Ok("error_before_commit") => store.set_fault(Some(Fault::ErrorBeforeCommit)),
        _ => {}
    }
    Ok(store)
}

/// After the operator ends a task's attempts, stops their sessions if no
/// supervisor is left to do it (a live supervisor's watcher already does),
/// and records why they ended. The move's JSON gains `stopped_sessions`.
fn with_stopped_sessions(
    cli: &Cli,
    store: &mut Store,
    task: &str,
    mv: &interlock_core::lifecycle::Move,
    context: &str,
) -> Result<Value> {
    let stopped = interlock_supervisor::sessions::end_unattended(store, &store_dir(cli), Some(task), context)?;
    let mut out = serde_json::to_value(mv)?;
    out["stopped_sessions"] = json!(stopped);
    Ok(out)
}

/// A warning when evidence is weaker than its criterion needs, so the agent
/// hears it while it can still look again.
fn strength_warning(store: &Store, attempt: &str, criterion: &str, strength: Strength) -> Option<String> {
    let task = store.task(&store.attempt(attempt).ok()?.task_id).ok()?;
    let c = task.criteria.iter().find(|c| c.id == criterion)?;
    if strength.pass_rank().is_none() || strength.satisfies(c.min_strength) {
        return None;
    }
    let name = |v: serde_json::Result<Value>| v.ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let (had, needs) = (name(serde_json::to_value(strength)), name(serde_json::to_value(c.min_strength)));
    Some(format!(
        "{criterion} needs {needs} or stronger ({}); {had} will not satisfy it",
        interlock_supervisor::prompts::strength_meaning(c.min_strength)
    ))
}

/// The directory beside the store: config, transcripts, worktrees, exports.
fn store_dir(cli: &Cli) -> PathBuf {
    let db = cli.db.clone().unwrap_or_else(default_db);
    db.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(".interlock"))
}

fn profile(cli: &Cli) -> Profile {
    match cli.profile {
        ProfileArg::Conservative => Profile::Conservative,
        ProfileArg::Permissive => Profile::Permissive,
    }
}

fn print(v: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn read_spec(path: &Path) -> Result<TaskSpec> {
    let text = if path == Path::new("-") {
        std::io::read_to_string(std::io::stdin())?
    } else {
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
    };
    let is_toml = path.extension().is_some_and(|e| e == "toml");
    if path == Path::new("-") {
        // From stdin, JSON or TOML, so a session can create a task without writing a file.
        return match serde_json::from_str(&text) {
            Ok(spec) => Ok(spec),
            Err(_) => Ok(toml::from_str(&text).context("reading the task from stdin as JSON or TOML")?),
        };
    }
    Ok(if is_toml { toml::from_str(&text)? } else { serde_json::from_str(&text)? })
}

fn parse_duration(s: &str) -> Result<Duration> {
    if let Some(secs) = s.strip_suffix('s') {
        return Ok(Duration::seconds(secs.parse().map_err(|_| anyhow!("bad duration {s}"))?));
    }
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: i64 = num.parse().map_err(|_| anyhow!("bad duration {s}; use forms like 30m or 8h"))?;
    Ok(match unit {
        "m" => Duration::minutes(n),
        "h" => Duration::hours(n),
        "d" => Duration::days(n),
        _ => bail!("bad duration {s}; use m, h or d"),
    })
}

fn host_by_name(name: &str) -> Result<Box<dyn Host>> {
    interlock_adapter::host(name).ok_or_else(|| {
        let names: Vec<&str> = interlock_adapter::hosts().iter().map(|h| h.name()).collect();
        anyhow!("unknown host {name}; known hosts: {}", names.join(", "))
    })
}

fn parse_capabilities(list: &[String]) -> Result<CapabilitySet> {
    list.iter()
        .map(|c| serde_json::from_value::<Capability>(json!(c.trim())).map_err(|_| anyhow!("unknown capability {c}")))
        .collect()
}

/// The capabilities and version for an attempt: declared, saved, or inspected now.
fn host_profile(store: &mut Store, host: &str, declared: Option<&[String]>) -> Result<(CapabilitySet, String)> {
    if let Some(list) = declared {
        return Ok((parse_capabilities(list)?, "declared".into()));
    }
    let report = match store.host_report(host)? {
        Some(v) => serde_json::from_value::<HostReport>(v)?,
        None => {
            // Saved only when it says something: never a probe that came back with no capabilities.
            let report = inspect_to_save(host)?;
            if report.installed {
                store.save_host_report(host, &serde_json::to_value(&report)?, now()?)?;
            }
            report
        }
    };
    if !report.installed {
        bail!("host {host} is not installed: {}", report.notes.join("; "));
    }
    Ok((report.capability_set(), report.version.unwrap_or_default()))
}

fn status(store: &Store, task_id: &str) -> Result<Value> {
    let (task, report) = store.evaluate(task_id)?;
    let attempts: Vec<Value> = store
        .attempts(task_id)?
        .iter()
        .map(|a| {
            json!({"id": a.id, "role": a.role, "epoch": a.epoch, "status": a.status, "host": a.host.host,
                   "binding": a.binding, "bound": binding_text(a)})
        })
        .collect();
    Ok(json!({
        "task": {
            "id": task.id,
            "state": task.state,
            "workflow": task.workflow,
            "intent": task.intent,
            "lease_epoch": task.lease_epoch,
            "attempts_used": task.attempts_used,
            "max_attempts": task.budget.max_attempts,
            "current_attempt": task.current_attempt,
            "current_tree": task.current_tree,
            "blocked_reason": task.blocked_reason,
            "integration_required": task.integration_required,
        },
        "evidence": report,
        "next_moves": next_moves(&task, &report),
        "attempts": attempts,
    }))
}

/// How an attempt is bound, in words, for status and briefs.
fn binding_text(a: &Attempt) -> String {
    let who = |b: &Binding| match (&b.host_session, &b.host_agent, &b.agent_type) {
        (Some(s), Some(agent), t) => format!("session {s}, agent {agent} ({})", t.as_deref().unwrap_or("type unknown")),
        (Some(s), None, _) => format!("session {s}"),
        _ => "no host session yet".into(),
    };
    match &a.binding {
        None => "not recorded".into(),
        Some(b) => match b.via {
            BoundVia::InterlockLaunched => "launched by interlock".into(),
            BoundVia::Subagent => format!("interlock's verifier subagent: {}", who(b)),
            BoundVia::Session => format!("bound to {}", who(b)),
            BoundVia::Unbound if a.role == Role::Worker => format!("unbound: {}", who(b)),
            BoundVia::Unbound => {
                format!("unbound ({}): its assessments count only where interlock ran the criterion's check", who(b))
            }
        },
    }
}

/// Once a task is done, failed or cancelled: ends the attempts it left open
/// and removes their worktrees. A done task's verified output is kept first,
/// as a ref the person can apply.
fn settle(cli: &Cli, store: &mut Store, task: &str) -> Result<Value> {
    let closed: Vec<String> = store.close_open_attempts(task, now()?)?.into_iter().map(|a| a.id).collect();
    let db = absolute_db(cli)?;
    // The task has already moved; a problem here is reported, not raised.
    let tidy = || -> Result<(Option<String>, Vec<PathBuf>)> {
        let Ok(repo) = interlock_supervisor::guided::repo_of_store(&db) else { return Ok((None, vec![])) };
        let kept = interlock_supervisor::guided::keep_output(store, &repo, task)?;
        Ok((kept, interlock_supervisor::guided::cleanup_worktrees(store, &repo, &db, task)?))
    };
    let (kept, removed, problem) = match tidy() {
        Ok((kept, removed)) => (kept, removed, None),
        Err(e) => (None, vec![], Some(format!("{e:#}"))),
    };
    let apply = kept.as_ref().map(|r| format!("git cherry-pick {r}"));
    Ok(json!({"closed_attempts": closed, "removed_worktrees": removed, "output_ref": kept, "apply_with": apply,
              "problem": problem}))
}

/// `--tree auto` means the tree of the attempt's own worktree, wherever the
/// command runs; without a recorded worktree, the current directory's.
fn tree_for(store: &Store, attempt_id: &str, arg: &str) -> Result<String> {
    if arg != "auto" {
        return Ok(arg.to_string());
    }
    let dir = match store.attempt(attempt_id)?.worktree {
        Some(w) => PathBuf::from(w),
        None => std::env::current_dir()?,
    };
    Ok(interlock_supervisor::git::worktree_tree(&dir)?)
}

fn run(cli: &Cli) -> Result<()> {
    let profile = profile(cli);
    let host_policy = HostPolicy::open();
    match &cli.command {
        Command::Init => {
            let path = cli.db.clone().unwrap_or_else(default_db);
            open(cli)?;
            if let Some(dir) = path.parent() {
                let ignore = dir.join(".gitignore");
                if !ignore.exists() {
                    std::fs::write(&ignore, "*\n")?;
                }
            }
            print(&json!({"store": path}))
        }
        Command::Workflows => print(&workflow::builtins()),
        Command::Schema(cmd) => match cmd {
            SchemaCmd::Show { kind } => {
                let kind = RecordKind::parse(kind).ok_or_else(|| anyhow!("unknown record kind {kind}"))?;
                println!("{}", kind.schema_source());
                Ok(())
            }
            SchemaCmd::Validate { kind, file } => {
                let kind = RecordKind::parse(kind).ok_or_else(|| anyhow!("unknown record kind {kind}"))?;
                let value: Value = serde_json::from_str(&std::fs::read_to_string(file)?)?;
                Validators::new()?.validate(kind, &value)?;
                print(&json!({"valid": true, "kind": kind.name()}))
            }
        },
        Command::Task(cmd) => {
            let mut store = open(cli)?;
            match cmd {
                TaskCmd::Create { file } => print(&store.create_task(read_spec(file)?, now()?)?),
                TaskCmd::Show { task } => print(&store.task(task)?),
                TaskCmd::List => print(
                    &store
                        .tasks()?
                        .iter()
                        .map(|t| json!({"id": t.id, "state": t.state, "workflow": t.workflow.name, "intent": t.intent}))
                        .collect::<Vec<_>>(),
                ),
                TaskCmd::Ready { task, base, repository, untracked_hash } => {
                    let repo = match repository {
                        Some(r) => r.clone(),
                        None => store.task(task)?.repository,
                    };
                    let criteria = store.task(task)?.criteria;
                    // Checks name files in the repository that owns the store, not the caller's.
                    let root = interlock_supervisor::guided::repo_of_store(&absolute_db(cli)?).ok();
                    let protected = root
                        .as_ref()
                        .map(|root| interlock_supervisor::checks::protected_paths(root, base, &criteria))
                        .unwrap_or_default();
                    // Untracked inputs are part of the snapshot unless the caller names them.
                    let untracked = match (untracked_hash, &root) {
                        (Some(h), _) => Some(h.clone()),
                        (None, Some(root)) => interlock_supervisor::git::untracked_tree(root)?,
                        (None, None) => None,
                    };
                    let snapshot = Snapshot {
                        repository: repo,
                        base_commit: base.clone(),
                        protected_paths: protected,
                        untracked_hash: untracked,
                    };
                    print(&store.ready(task, snapshot, now()?)?)
                }
                TaskCmd::Tree { task, tree } => {
                    forge::operator_only("task tree")?;
                    print(&store.record_new_tree(task, tree, now()?)?)
                }
                TaskCmd::Block { task, reason } => print(&store.block(task, reason, now()?)?),
                TaskCmd::Unblock { task } => {
                    forge::operator_only("task unblock")?;
                    print(&store.unblock(task, now()?)?)
                }
                TaskCmd::Retry { task, reason } => {
                    forge::operator_only("task retry")?;
                    let mv = store.retry(task, reason, now()?)?;
                    print(&with_stopped_sessions(cli, &mut store, task, &mv, "`interlock task retry`")?)
                }
                TaskCmd::Fail { task, reason } | TaskCmd::Cancel { task, reason } => {
                    // Stop any session its supervisor left behind, then close
                    // the attempts and tidy their worktrees.
                    let cancel = matches!(cmd, TaskCmd::Cancel { .. });
                    forge::operator_only(if cancel { "task cancel" } else { "task fail" })?;
                    let mv = store.stop(task, cancel, reason, now()?)?;
                    let context = if cancel { "`interlock task cancel`" } else { "`interlock task fail`" };
                    let mut moved = with_stopped_sessions(cli, &mut store, task, &mv, context)?;
                    moved["settled"] = settle(cli, &mut store, task)?;
                    print(&moved)
                }
                TaskCmd::Log { task } => print(&store.transitions(task)?),
                TaskCmd::Events { task } => print(&store.events(task)?),
                TaskCmd::Export { task } => {
                    let repo = interlock_supervisor::git::toplevel(&std::env::current_dir()?)?;
                    let dir = store_dir(cli);
                    print(&interlock_supervisor::export::export(&store, &repo, &dir, task, profile, now()?)?)
                }
                TaskCmd::Resume { task, from } => {
                    let repo = interlock_supervisor::git::toplevel(&std::env::current_dir()?)?;
                    let (commit, tree) =
                        interlock_supervisor::export::resume(&mut store, &repo, task, from.as_deref(), now()?)?;
                    print(
                        &json!({"task": task, "resumes_from": commit, "tree": tree, "state": store.task(task)?.state}),
                    )
                }
            }
        }
        Command::Attempt(cmd) => {
            let mut store = open(cli)?;
            match cmd {
                AttemptCmd::Start { task, role, host, mode, capabilities, agent, model, worktree } => {
                    let (caps, version) = host_profile(&mut store, host, capabilities.as_deref())?;
                    // The host side of the effective grant, from `[host_policy.<host>]`.
                    let host_policy = interlock_supervisor::config::Config::load(&store_dir(cli))
                        .map_err(|e| anyhow!(e))?
                        .host_policy(host);
                    let db = absolute_db(cli)?;
                    let interactive = matches!(mode, ModeArg::Interactive);
                    // In a guided session the hooks saw who ran this command; nobody else can say.
                    let opener = if interactive {
                        interlock_supervisor::guided::take_intent(&store, task, (*role).into(), now()?)?
                    } else {
                        None
                    };
                    if *role == RoleArg::Verifier
                        && let Some(who) = &opener
                    {
                        interlock_supervisor::guided::may_open_verifier(&store, who, Some(task)).map_err(Refused)?;
                    }
                    // `--worktree auto`: interlock makes a fresh worktree for this attempt.
                    let made = match worktree.as_deref() {
                        Some("auto") => {
                            let repo = interlock_supervisor::guided::repo_of_store(&db)?;
                            let dir = interlock_supervisor::guided::prepare_worktree(
                                &store,
                                &repo,
                                &db,
                                task,
                                (*role).into(),
                            )?;
                            Some((repo, dir))
                        }
                        _ => None,
                    };
                    let worktree = made.as_ref().map(|(_, dir)| dir.display().to_string()).or_else(|| worktree.clone());
                    let started = store.start_attempt(
                        StartAttempt {
                            task_id: task.clone(),
                            role: (*role).into(),
                            mode: match mode {
                                ModeArg::Interactive => Mode::Interactive,
                                ModeArg::Headless => Mode::Headless,
                            },
                            host: HostRef { host: host.clone(), version },
                            capabilities: caps,
                            profile,
                            host_policy,
                            agent: agent.clone(),
                            model: model.clone(),
                            worktree,
                        },
                        now()?,
                    );
                    let opened = matches!(started, Ok(Started::Yes { .. }));
                    if let (false, Some((repo, dir))) = (opened, &made) {
                        interlock_supervisor::guided::discard_worktree(repo, dir);
                    }
                    let mut started = started?;
                    if let Started::Yes { attempt, .. } = &mut started {
                        let binding =
                            interlock_supervisor::guided::binding_for((*role).into(), interactive, opener.as_ref());
                        *attempt = store.bind_attempt(&attempt.id, binding)?;
                    }
                    print(&started)
                }
                AttemptCmd::End { attempt, token, failed, note } => {
                    print(&store.end_attempt(attempt, token, *failed, note.clone(), now()?)?)
                }
                AttemptCmd::List { task } => print(&store.attempts(task)?),
            }
        }
        Command::Result(ResultCmd::Submit { auth, epoch, tree, summary, questions }) => {
            let mut store = open(cli)?;
            let task_id = store.attempt(&auth.attempt)?.task_id;
            // G3 reads the change set from the output tree, in the repository that owns
            // the store, against the task's input snapshot. When it cannot, it refuses.
            let db = absolute_db(cli)?;
            let read = || -> Result<(String, Vec<String>)> {
                let repo = interlock_supervisor::guided::repo_of_store(&db)?;
                let tree = tree_for(&store, &auth.attempt, tree)?;
                let changed = interlock_supervisor::guided::changed_paths(&store, &repo, &task_id, &tree)?;
                Ok((tree, changed))
            };
            let (tree, changed) = read().map_err(|e| {
                Refused(format!("interlock could not read what the result changes, so it refuses it: {e:#}"))
            })?;
            print(&store.submit_result(
                SubmitResult {
                    attempt_id: auth.attempt.clone(),
                    token: auth.token.clone(),
                    epoch: *epoch,
                    output_tree: tree,
                    changed_paths: changed,
                    summary: summary.clone(),
                    open_questions: questions.clone(),
                    event_id: auth.event_id.clone(),
                },
                now()?,
            )?)
        }
        Command::Claim(EvidenceCmd::Add { auth, criterion, strength, tree, refs, note, environment })
        | Command::Assess(EvidenceCmd::Add { auth, criterion, strength, tree, refs, note, environment }) => {
            let kind =
                if matches!(cli.command, Command::Claim(_)) { EvidenceKind::Claim } else { EvidenceKind::Assessment };
            let mut store = open(cli)?;
            let applied = store.add_evidence(
                kind,
                AddEvidence {
                    attempt_id: auth.attempt.clone(),
                    token: auth.token.clone(),
                    criterion_id: criterion.clone(),
                    strength: (*strength).into(),
                    tree: tree_for(&store, &auth.attempt, tree)?,
                    environment: environment.clone(),
                    evidence_refs: refs.clone(),
                    note: note.clone(),
                    event_id: auth.event_id.clone(),
                },
                now()?,
            )?;
            let mut out = serde_json::to_value(&applied)?;
            if let Some(w) = strength_warning(&store, &auth.attempt, criterion, (*strength).into()) {
                out["warning"] = json!(w);
            }
            print(&out)
        }
        Command::Status { task } => print(&status(&open(cli)?, task)?),
        Command::Advance { task } => {
            let mut store = open(cli)?;
            let moves = store.advance(task, now()?)?;
            let settled = settle(cli, &mut store, task)?;
            print(&json!({"moves": moves, "status": status(&store, task)?, "settled": settled}))
        }
        Command::Brief { task, role, format } => {
            let store = open(cli)?;
            let brief = store.brief(task, (*role).into(), profile, &host_policy, now()?)?;
            let attempts = store.attempts(task)?;
            let bound: Vec<(String, String)> = attempts
                .iter()
                .filter(|a| a.role != Role::Worker)
                .map(|a| (format!("{} ({:?}, {:?})", a.id, a.role, a.status).to_lowercase(), binding_text(a)))
                .collect();
            match format {
                Format::Json => {
                    let mut v = serde_json::to_value(&brief)?;
                    v["verifier_bindings"] =
                        json!(bound.iter().map(|(a, b)| json!({"attempt": a, "bound": b})).collect::<Vec<_>>());
                    print(&v)
                }
                Format::Markdown => {
                    print!("{}", brief.to_markdown());
                    if !bound.is_empty() {
                        println!("\n## Verifier attempts\n");
                        for (a, b) in &bound {
                            println!("- {a}: {b}");
                        }
                    }
                    Ok(())
                }
            }
        }
        Command::Integrate(cmd) => {
            if !matches!(cmd, IntegrateCmd::Operations { .. }) {
                forge::operator_only("integrate")?;
            }
            let mut store = open(cli)?;
            match cmd {
                IntegrateCmd::Begin { task, pr, base, head } => {
                    let intent = OperationIntent {
                        expected_head_sha: head.clone(),
                        base: base.clone(),
                        pull_request: *pr,
                        tree: None,
                    };
                    let (moved, operation) = store.begin_integration(task, OperationKind::Merge, intent, now()?)?;
                    print(&json!({"moved": moved, "operation": operation}))
                }
                IntegrateCmd::Confirm { task, operation, merged, refused } => {
                    let report = match (merged, refused) {
                        (Some(sha), None) => MergeReport::Merged { head_sha: sha.clone() },
                        (None, Some(reason)) => MergeReport::Refused { reason: reason.clone() },
                        _ => bail!("pass exactly one of --merged or --refused"),
                    };
                    print(&store.confirm_integration(task, operation, report, now()?)?)
                }
                IntegrateCmd::Operations { task } => print(&store.operations(task)?),
                IntegrateCmd::Run(_) => unreachable!("handled in main"),
            }
        }
        Command::Reconcile { task } => forge::reconcile(&cli.db.clone().unwrap_or_else(default_db), task.as_deref()),
        Command::Grant(cmd) => {
            if !matches!(cmd, GrantCmd::List) {
                forge::operator_only("grant")?;
            }
            let mut store = open(cli)?;
            match cmd {
                GrantCmd::Create { principal, tasks, classes, landing, deny, expires_in, expires_at, origin } => {
                    let at = now()?;
                    let expires_at = match (expires_in, expires_at) {
                        (Some(d), _) => Some(at + parse_duration(d)?),
                        (None, e) => *e,
                    };
                    print(&store.create_grant(
                        NewGrant {
                            principal: principal.clone(),
                            task_scope: tasks.clone(),
                            action_classes: classes.iter().map(|c| (*c).into()).collect(),
                            tools: ToolPolicy { allow: vec![], deny: deny.clone() },
                            landing_authority: (*landing).into(),
                            origin: origin.clone(),
                            expires_at,
                        },
                        at,
                    )?)
                }
                GrantCmd::Revoke { grant } => print(&store.revoke_grant(grant, now()?)?),
                GrantCmd::List => print(&store.grants()?),
            }
        }
        Command::Host(cmd) => match cmd {
            HostCmd::Inspect { host, check_auth, save } => {
                let probe = Probe::from_env();
                let selected: Vec<Box<dyn Host>> =
                    if host == "all" { interlock_adapter::hosts() } else { vec![host_by_name(host)?] };
                let mut reports = Vec::new();
                for h in selected {
                    let mut report = h.inspect(&probe);
                    if *check_auth && report.installed {
                        report.auth = h.check_auth(&probe);
                    }
                    reports.push(report);
                }
                if *save {
                    let mut store = open(cli)?;
                    // A probe that found the host but no capabilities (it may have timed out) is not saved.
                    for r in reports.iter().filter(|r| !r.installed || !r.capability_set().0.is_empty()) {
                        store.save_host_report(&r.host, &serde_json::to_value(r)?, now()?)?;
                    }
                }
                // Each report says whether the installed version matches .interlock/config.toml's pin.
                let config = interlock_supervisor::config::Config::load(&store_dir(cli)).map_err(|e| anyhow!(e))?;
                let mut out = Vec::new();
                for r in &reports {
                    let mut v = serde_json::to_value(r)?;
                    let pin = interlock_supervisor::config::PinStatus::of(config.pin(&r.host), r.version.as_deref());
                    v["pin"] = serde_json::to_value(pin)?;
                    out.push(v);
                }
                print(&out)
            }
            HostCmd::Tools { task, role, host } => {
                let store = open(cli)?;
                let task = store.task(task)?;
                let workflow = workflow::builtin(&task.workflow.name, task.workflow.version)
                    .ok_or_else(|| anyhow!("unknown workflow"))?;
                let grant = grants::effective(
                    &task.id,
                    profile,
                    &store.grants()?,
                    &host_policy,
                    workflow.role((*role).into()),
                    now()?,
                );
                let h = host_by_name(host)?;
                print(&json!({
                    "host": h.name(),
                    "action_classes": grant.action_classes,
                    "host_neutral": grant.tools,
                    "host_patterns": h.translate(&grant.tools),
                }))
            }
        },
        Command::Run { .. }
        | Command::Where
        | Command::Hook { .. }
        | Command::Skills(_)
        | Command::Setup { .. }
        | Command::Verify { .. } => {
            unreachable!("handled in main")
        }
        Command::Note(cmd) => {
            let store = open(cli)?;
            match cmd {
                NoteCmd::Add { task, kind, file } => {
                    let body = if file == Path::new("-") {
                        std::io::read_to_string(std::io::stdin())?
                    } else {
                        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?
                    };
                    print(&interlock_supervisor::guided::add_note(&store, task, kind, &body, now()?)?)
                }
                NoteCmd::List { task } => print(&interlock_supervisor::guided::notes(&store, task)?),
            }
        }
        Command::Check(CheckCmd::Run { criterion, target, task, operator, timeout, attempt, token }) => {
            let mut store = open(cli)?;
            let nonempty = |v: &Option<String>| v.clone().filter(|s| !s.is_empty());
            // An attempt's run checks the files in its own worktree, wherever the command runs.
            let (attempt, task_id, dir) = if *operator {
                forge::operator_only("check run --operator")?;
                let task = task.clone().ok_or_else(|| anyhow!("--operator needs --task"))?;
                (None, task, std::env::current_dir()?)
            } else {
                let (Some(a), Some(t)) = (nonempty(attempt), nonempty(token)) else {
                    bail!("outside an attempt, pass --operator and --task");
                };
                let rec = store.attempt(&a)?;
                let dir = match rec.worktree {
                    Some(w) => PathBuf::from(w),
                    None => std::env::current_dir()?,
                };
                (Some((a, t)), rec.task_id, dir)
            };
            let db = cli.db.clone().unwrap_or_else(default_db);
            let scratch = db.parent().map(|d| d.join("scratch")).unwrap_or_else(|| PathBuf::from(".interlock/scratch"));
            let run = interlock_supervisor::checks::run_check(
                &mut store,
                interlock_supervisor::checks::CheckRequest {
                    task_id: &task_id,
                    criterion_id: criterion,
                    target: match target {
                        TargetArg::Output => RunTarget::Output,
                        TargetArg::Base => RunTarget::Base,
                    },
                    attempt,
                    dir: &dir,
                    scratch: &scratch,
                    timeout: parse_duration(timeout)?.to_std()?,
                },
            )?;
            print(&json!({
                "passed": run.passed(),
                "failed": run.failed(),
                "checked_nothing": run.vacuous,
                "run": run,
            }))
        }
        Command::Policy(PolicyCmd::Check { attempt, shell, tool }) => {
            let store = open(cli)?;
            let attempt = store.attempt(attempt)?;
            let (class, tool_name) = match (shell, tool) {
                (Some(line), None) => (classify_shell(line), format!("shell:{}", line.trim())),
                (None, Some(t)) => {
                    let class = match t.as_str() {
                        "read" | "web" => ActionClass::Read,
                        "edit" | "agent" => ActionClass::LocalReversible,
                        _ => ActionClass::ExternalReversible,
                    };
                    (class, t.clone())
                }
                _ => bail!("pass one of --shell or --tool"),
            };
            let verdict = grants::decide(&attempt.effective_grant, class, &tool_name);
            let (decision, reason) = match &verdict {
                grants::Verdict::Allow => {
                    ("allow", format!("{} is covered by the effective grant", grants::class_name(class)))
                }
                grants::Verdict::Deny(r) => ("deny", r.clone()),
                grants::Verdict::Ask(r) => ("ask", r.clone()),
            };
            print(&json!({"decision": decision, "class": class, "tool": tool_name, "reason": reason}))
        }
    }
}

/// Runs one hook: payload on stdin, the response on stdout and stderr, and an
/// exit code both hosts understand.
fn hook(cli: &Cli) -> ExitCode {
    let mut raw = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut raw);
    let ctx = interlock_supervisor::hook::HookContext::from_env(cli.db.clone().unwrap_or_else(default_db));
    // A guided session names no attempt in its environment; the hooks learn the caller from the payload.
    let guided = ctx.attempt_id.is_none() && !ctx.headless;
    let Ok(payload) = serde_json::from_str::<Value>(&raw) else {
        // Fail closed: inside an attempt, a tool call we cannot read is denied.
        let governed =
            ctx.attempt_id.is_some() || (guided && interlock_supervisor::guided::guided_attempt_open(&ctx.db));
        if governed && matches!(cli.command, Command::Hook { event: HookArg::PreToolUse }) {
            eprintln!("interlock could not read the hook payload");
            return ExitCode::from(2);
        }
        return ExitCode::SUCCESS;
    };
    let response = if guided {
        interlock_supervisor::guided::handle(&ctx, &payload, Utc::now())
    } else {
        interlock_supervisor::hook::handle(&ctx, &payload, Utc::now())
    };
    if !response.stdout.is_empty() {
        println!("{}", response.stdout);
    }
    if !response.stderr.is_empty() {
        eprintln!("{}", response.stderr);
    }
    ExitCode::from(response.exit_code as u8)
}

#[allow(clippy::too_many_arguments)]
fn run_task(
    cli: &Cli,
    task: &str,
    host: &str,
    model: &Option<String>,
    timeout: &str,
    max_turns: u32,
    max_sessions: u32,
    keep_worktrees: bool,
    capabilities: &Option<Vec<String>>,
    effort: &Option<String>,
    skills: &Option<PathBuf>,
) -> Result<ExitCode> {
    let cwd = std::env::current_dir()?;
    let repo = interlock_supervisor::git::toplevel(&cwd)?;
    let db = cli.db.clone().unwrap_or_else(|| repo.join(".interlock").join("state.db"));
    let db = if db.is_absolute() { db } else { cwd.join(db) };
    let cfg = interlock_supervisor::RunConfig {
        model: model.clone(),
        timeout: parse_duration(timeout)?.to_std()?,
        max_turns: Some(max_turns),
        keep_worktrees,
        max_sessions,
        profile: profile(cli),
        interlock_bin: std::env::current_exe()?,
        capabilities: capabilities.as_deref().map(parse_capabilities).transpose()?,
        effort: effort.clone(),
        skills: skills_plugin(skills)?,
    };
    // Settle what a previous controller left open at the forge before anything else.
    let reconciled = forge::reconcile_on_start(&db, &repo)?;
    let mut supervisor = interlock_supervisor::Supervisor::new(repo, db, host_by_name(host)?, Probe::from_env(), cfg)?;
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use std::sync::atomic::{AtomicUsize, Ordering};
    // SIGINT, SIGTERM or SIGHUP stops the running session, cancels its attempt
    // and leaves the task to resume. Further signals do nothing more: the host
    // runs in its own process group, so exiting early would leave it running
    // unseen, and stopping it takes at most a few seconds. The report names
    // the first signal that arrived.
    let first = std::sync::Arc::new(AtomicUsize::new(0));
    let mut signals = signal_hook::iterator::Signals::new([SIGINT, SIGTERM, SIGHUP])?;
    let (cancel, seen) = (supervisor.cancel.clone(), first.clone());
    std::thread::spawn(move || {
        for sig in signals.forever() {
            let _ = seen.compare_exchange(0, sig as usize, Ordering::SeqCst, Ordering::SeqCst);
            cancel.store(true, Ordering::SeqCst);
        }
    });
    let mut report = supervisor.run(task)?;
    report.reconciled.extend(reconciled);
    print(&report)?;
    if report.interrupted {
        let name = match first.load(Ordering::SeqCst) as i32 {
            SIGINT => "SIGINT",
            SIGTERM => "SIGTERM",
            SIGHUP => "SIGHUP",
            _ => "a signal",
        };
        let resume = format!("interlock run {task} --host {host}");
        eprintln!("{}", json!({"interrupted": name, "final_state": report.final_state, "resume": resume}));
        return Ok(ExitCode::from(6));
    }
    Ok(if report.final_state == State::Done { ExitCode::SUCCESS } else { ExitCode::from(5) })
}

/// A host report fit to save: re-probed once if the first probe came back
/// without capabilities (a probe that timed out reads as none), refused if
/// the second did too.
fn inspect_to_save(host: &str) -> Result<HostReport> {
    let h = host_by_name(host)?;
    let probe = Probe::from_env();
    let report = h.inspect(&probe);
    if !report.installed || !report.capability_set().0.is_empty() {
        return Ok(report);
    }
    let again = h.inspect(&probe);
    if again.capability_set().0.is_empty() {
        bail!(
            "{host} is installed but probing it reported no capabilities twice (a probe may have timed out); \
             nothing was saved. Check that `{host} --help` runs, then try again"
        );
    }
    Ok(again)
}

/// A skills plugin for `--skills`: a directory holding `.claude-plugin/plugin.json`,
/// as `interlock skills generate --target claude-code` writes it. Made absolute,
/// since sessions run in their own worktrees.
fn skills_plugin(dir: &Option<PathBuf>) -> Result<Option<PathBuf>> {
    let Some(dir) = dir else { return Ok(None) };
    let dir = std::fs::canonicalize(dir).with_context(|| format!("--skills {}", dir.display()))?;
    if !dir.join(".claude-plugin").join("plugin.json").is_file() {
        bail!("--skills {} is not a plugin: it has no .claude-plugin/plugin.json", dir.display());
    }
    Ok(Some(dir))
}

/// `interlock verify`: one verifier session launched by interlock.
#[allow(clippy::too_many_arguments)]
fn verify_task(
    cli: &Cli,
    task: &str,
    host: &str,
    model: &Option<String>,
    timeout: &str,
    max_turns: u32,
    effort: &Option<String>,
    skills: &Option<PathBuf>,
) -> Result<ExitCode> {
    let db = absolute_db(cli)?;
    let repo = interlock_supervisor::guided::repo_of_store(&db)?;
    let cfg = interlock_supervisor::RunConfig {
        model: model.clone(),
        timeout: parse_duration(timeout)?.to_std()?,
        max_turns: Some(max_turns),
        keep_worktrees: false,
        max_sessions: 1,
        profile: profile(cli),
        interlock_bin: std::env::current_exe()?,
        capabilities: None,
        effort: effort.clone(),
        skills: skills_plugin(skills)?,
    };
    let mut supervisor = interlock_supervisor::Supervisor::new(repo, db, host_by_name(host)?, Probe::from_env(), cfg)?;
    let report = supervisor.verify(task)?;
    print(&report)?;
    let mut store = open(cli)?;
    settle(cli, &mut store, task)?;
    Ok(if matches!(report.final_state, State::Verified | State::Done) { ExitCode::SUCCESS } else { ExitCode::from(5) })
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match &cli.command {
        Command::Hook { .. } => return hook(&cli),
        Command::Where => where_store(&cli),
        Command::Integrate(IntegrateCmd::Run(args)) => {
            forge::integrate_run(&cli.db.clone().unwrap_or_else(default_db), args)
        }
        Command::Run {
            task,
            host,
            model,
            timeout,
            max_turns,
            max_sessions,
            keep_worktrees,
            capabilities,
            effort,
            skills,
        } => run_task(
            &cli,
            task,
            host,
            model,
            timeout,
            *max_turns,
            *max_sessions,
            *keep_worktrees,
            capabilities,
            effort,
            skills,
        ),
        Command::Skills(cmd) => skills_cmd::skills(cmd),
        Command::Setup { host, from, force } => absolute_db(&cli).and_then(|db| {
            // The store must live in the repository setup writes to; nothing is created otherwise.
            let repo = interlock_supervisor::guided::repo_of_store(&db)?;
            // Inspect the host once now, so a guided session's attempts read the saved report.
            let report = inspect_to_save(host)?;
            let mut store = open(&cli)?;
            if report.installed {
                store.save_host_report(host, &serde_json::to_value(&report)?, now()?)?;
            }
            skills_cmd::setup(host, &repo, &db, from.as_deref(), &report, *force)
        }),
        Command::Verify { task, host, model, timeout, max_turns, effort, skills } => {
            verify_task(&cli, task, host, model, timeout, *max_turns, effort, skills)
        }
        _ => run(&cli).map(|()| ExitCode::SUCCESS),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            // Store errors may arrive directly or wrapped by the supervisor.
            let store_err = err.downcast_ref::<StoreError>().or_else(|| match err.downcast_ref() {
                Some(interlock_supervisor::RunError::Store(e)) => Some(e),
                _ => None,
            });
            let (code, body) = match store_err {
                _ if err.downcast_ref::<Refused>().is_some() => {
                    (2, json!({"error": "refused", "message": err.to_string()}))
                }
                Some(StoreError::Refused(r)) => (2, json!({"error": "refused", "refusal": r})),
                Some(StoreError::NotFound(what)) => {
                    (3, json!({"error": "not_found", "message": format!("not found: {what}")}))
                }
                Some(StoreError::BadToken(_)) => (4, json!({"error": "bad_token", "message": err.to_string()})),
                Some(StoreError::NetworkFilesystem { .. }) => {
                    (2, json!({"error": "network_filesystem", "message": format!("{err:#}")}))
                }
                _ => (1, json!({"error": "failed", "message": format!("{err:#}")})),
            };
            eprintln!("{}", serde_json::to_string_pretty(&body).unwrap_or_default());
            ExitCode::from(code)
        }
    }
}
