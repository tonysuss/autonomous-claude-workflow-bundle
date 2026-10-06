//! Briefs for starting or resuming work, built only from durable records.
//! A chat transcript is never the authority for resuming.

use interlock_schema::{Baseline, CheckRun, EffectiveGrant, Role, RunTarget, Task, TaskResult};
use serde::Serialize;

use crate::evidence::{CriterionVerdict, EvidenceReport};
use crate::lifecycle::{NextMove, next_moves};

#[derive(Debug, Clone, Serialize)]
pub struct Brief {
    pub task_id: String,
    pub role: Role,
    pub goal: String,
    pub workflow: String,
    pub state: String,
    pub scope: Vec<String>,
    /// Files the checks run; a result that changes them is rejected.
    pub protected: Vec<String>,
    pub base_commit: Option<String>,
    pub current_tree: Option<String>,
    pub criteria: Vec<BriefCriterion>,
    pub previous_result: Option<PreviousResult>,
    /// What the tasks this one depends on produced, from their records.
    pub upstream: Vec<UpstreamResult>,
    pub failures_to_fix: Vec<String>,
    pub grant: Option<EffectiveGrant>,
    pub next_moves: Vec<NextMove>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BriefCriterion {
    pub id: String,
    pub statement: String,
    pub check: Option<String>,
    /// What the check must do on the input snapshot, in words.
    pub baseline: Option<String>,
    pub needs: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreviousResult {
    pub attempt_id: String,
    pub output_tree: String,
    pub summary: String,
    pub changed_paths: Vec<String>,
    pub open_questions: Vec<String>,
}

/// An upstream (dependency) task's state and its last accepted result.
#[derive(Debug, Clone, Serialize)]
pub struct UpstreamResult {
    pub task_id: String,
    pub state: String,
    pub output_tree: Option<String>,
    pub summary: Option<String>,
    pub changed_paths: Vec<String>,
}

pub fn build(
    task: &Task,
    role: Role,
    report: &EvidenceReport,
    last_accepted: Option<&TaskResult>,
    last_rejected: Option<&TaskResult>,
    runs: &[CheckRun],
    grant: Option<&EffectiveGrant>,
) -> Brief {
    let criteria = task
        .criteria
        .iter()
        .map(|c| {
            let status = report
                .criteria
                .iter()
                .find(|r| r.criterion_id == c.id)
                .map(|r| match &r.verdict {
                    CriterionVerdict::Pass { strength, .. } => format!("passes ({strength:?})"),
                    CriterionVerdict::Fail { note, .. } => {
                        format!(
                            "failed an independent check{}",
                            note.as_deref().map(|n| format!(": {n}")).unwrap_or_default()
                        )
                    }
                    CriterionVerdict::NotYet { reason } => format!("not yet: {reason}"),
                })
                .unwrap_or_default();
            BriefCriterion {
                id: c.id.clone(),
                statement: c.statement.clone(),
                check: c.check.clone(),
                baseline: {
                    let promise = match c.baseline {
                        Baseline::Fails => Some("it must fail on the input snapshot, reproducing the problem"),
                        Baseline::Passes => Some("it must pass on the input snapshot, as a regression guard"),
                        Baseline::Any => None,
                    };
                    let seen =
                        runs.iter().rev().find(|r| r.criterion_id == c.id && r.target == RunTarget::Base).map(|r| {
                            let what = if r.vacuous.is_some() {
                                "ran nothing"
                            } else if r.passed() {
                                "pass"
                            } else if r.failed() {
                                "fail"
                            } else {
                                "not finish"
                            };
                            format!("interlock saw it {what} there (run {})", r.id)
                        });
                    promise.map(|p| match seen {
                        Some(s) => format!("{p}; {s}"),
                        None => p.to_string(),
                    })
                },
                needs: format!(
                    "{} from {}",
                    serde_json::to_value(c.min_strength).unwrap().as_str().unwrap_or("?"),
                    serde_json::to_value(c.producer).unwrap().as_str().unwrap_or("?")
                ),
                status,
            }
        })
        .collect();
    let mut failures_to_fix: Vec<String> = report
        .criteria
        .iter()
        .filter_map(|r| match &r.verdict {
            CriterionVerdict::Fail { note, evidence_id, .. } => Some(format!(
                "{}: {}",
                r.criterion_id,
                note.clone().unwrap_or_else(|| format!("see assessment {evidence_id}"))
            )),
            _ => None,
        })
        .collect();
    // A rejection newer than the last accepted result is what the next attempt must avoid.
    if let Some(r) = last_rejected.filter(|r| last_accepted.is_none_or(|a| r.received_at > a.received_at)) {
        failures_to_fix.push(format!(
            "attempt {}'s result was rejected: {}",
            r.attempt_id,
            r.superseded_reason.as_deref().unwrap_or("no reason recorded")
        ));
    }
    Brief {
        task_id: task.id.clone(),
        role,
        goal: task.intent.clone(),
        workflow: format!("{} v{}", task.workflow.name, task.workflow.version),
        state: task.state.to_string(),
        scope: task.scope.paths.clone(),
        protected: task.input_snapshot.as_ref().map(|s| s.protected_paths.clone()).unwrap_or_default(),
        base_commit: task.input_snapshot.as_ref().map(|s| s.base_commit.clone()),
        current_tree: task.current_tree.clone(),
        criteria,
        previous_result: last_accepted.map(|r| PreviousResult {
            attempt_id: r.attempt_id.clone(),
            output_tree: r.output_tree.clone(),
            summary: r.summary.clone(),
            changed_paths: r.changed_paths.clone(),
            open_questions: r.open_questions.clone(),
        }),
        upstream: vec![],
        failures_to_fix,
        grant: grant.cloned(),
        next_moves: next_moves(task, report),
    }
}

impl Brief {
    pub fn to_markdown(&self) -> String {
        let mut s = String::new();
        let mut line = |l: String| {
            s.push_str(&l);
            s.push('\n');
        };
        line(format!("# Task {} ({:?} brief)", self.task_id, self.role));
        line(String::new());
        line(format!("Goal: {}", self.goal));
        line(format!("Workflow: {}. State: {}.", self.workflow, self.state));
        if !self.scope.is_empty() {
            line(format!("Scope: {}. A result that changes other files is rejected.", self.scope.join(", ")));
        }
        if !self.protected.is_empty() {
            line(format!("Do not change the checks themselves: {}.", self.protected.join(", ")));
        }
        if let Some(base) = &self.base_commit {
            line(format!("Base commit: {base}"));
        }
        if let Some(tree) = &self.current_tree {
            let accepted = self.previous_result.as_ref().is_some_and(|r| &r.output_tree == tree);
            let judged = matches!(self.state.as_str(), "awaiting_verification" | "verified" | "integrating");
            line(match (accepted, judged) {
                (true, true) => format!("Tree under verification: {tree}"),
                (true, false) => format!("Starting tree, the last accepted result: {tree}"),
                (false, _) => {
                    format!("Starting tree, exported work in progress that no result has been accepted for: {tree}")
                }
            });
        }
        line(String::new());
        line("## Criteria".into());
        for c in &self.criteria {
            let check = c
                .check
                .as_deref()
                .map(|ch| format!(" Check: `{ch}`; run it with `interlock check run --criterion {}`.", c.id))
                .unwrap_or_default();
            let baseline = c.baseline.as_deref().map(|b| format!(" On its own, {b}.")).unwrap_or_default();
            let statement = c.statement.trim_end();
            let stop = if statement.ends_with(['.', '!', '?']) { "" } else { "." };
            line(format!(
                "- **{}**: {statement}{stop} Needs {}.{check}{baseline} Status: {}.",
                c.id, c.needs, c.status
            ));
        }
        if !self.failures_to_fix.is_empty() {
            line(String::new());
            line("## Fix first".into());
            for f in &self.failures_to_fix {
                line(format!("- {f}"));
            }
        }
        if !self.upstream.is_empty() {
            line(String::new());
            line("## Upstream results".into());
            for u in &self.upstream {
                let what = match (&u.summary, &u.output_tree) {
                    (Some(s), Some(t)) => format!("{s} (tree {t})"),
                    _ => "no accepted result yet".into(),
                };
                line(format!("- {} ({}): {what}", u.task_id, u.state));
                if !u.changed_paths.is_empty() {
                    line(format!("  Changed: {}", u.changed_paths.join(", ")));
                }
            }
        }
        if let Some(r) = &self.previous_result {
            line(String::new());
            line(format!("## Previous accepted result (attempt {})", r.attempt_id));
            line(r.summary.clone());
            if !r.changed_paths.is_empty() {
                line(format!("Changed: {}", r.changed_paths.join(", ")));
            }
            for q in &r.open_questions {
                line(format!("- Open question: {q}"));
            }
        }
        if let Some(g) = &self.grant {
            line(String::new());
            line("## Authority".into());
            line(format!(
                "Action classes: {}. Tools allowed: {}. Denied: {}. Landing: {:?}.",
                g.action_classes.iter().map(|c| format!("{c:?}")).collect::<Vec<_>>().join(", "),
                g.tools.allow.join(", "),
                if g.tools.deny.is_empty() { "none".into() } else { g.tools.deny.join(", ") },
                g.landing_authority
            ));
        }
        line(String::new());
        line("## Report back through interlock".into());
        line("Submit results, claims and assessments with `interlock` using your attempt id and token. Do not edit interlock's store.".into());
        s
    }
}
