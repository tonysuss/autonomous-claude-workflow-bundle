//! Evidence policy v2. Worker claims, verifier assessments and interlock's
//! own check runs are kept apart; the policy reads all three and decides, in
//! a fixed order:
//!
//! 1. Keep only current evidence: its currency key matches the task.
//! 2. Any current independent failure fails the criterion, whatever else
//!    says pass. A check run a verifier asked for that fails counts as one.
//! 3. Required strength from an allowed producer passes it. When the
//!    criterion names a check, interlock must also have run that check on
//!    this tree and seen it pass, and, if the criterion sets a baseline, seen
//!    the check behave as promised on the task's input snapshot.
//! 4. Otherwise, not yet.
//!
//! A check run that ran nothing (no tests found) never passes and never fails.

use interlock_schema::{
    Baseline, CheckRun, Criterion, Currency, Evidence, Id, Producer, RunProducer, RunTarget, Strength, Task,
};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Claim,
    Assessment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum CriterionVerdict {
    Pass { strength: Strength, source: Source, evidence_id: Id },
    Fail { evidence_id: Id, attempt_id: Id, note: Option<String> },
    NotYet { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CriterionReport {
    pub criterion_id: Id,
    #[serde(flatten)]
    pub verdict: CriterionVerdict,
    pub current_claims: usize,
    pub current_assessments: usize,
    pub current_runs: usize,
    pub stale: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceReport {
    pub criteria: Vec<CriterionReport>,
    pub all_pass: bool,
    pub any_fail: bool,
}

impl EvidenceReport {
    /// One line per criterion that does not pass yet.
    pub fn missing(&self) -> Vec<String> {
        self.criteria
            .iter()
            .filter_map(|c| match &c.verdict {
                CriterionVerdict::Pass { .. } => None,
                CriterionVerdict::Fail { evidence_id, .. } => {
                    Some(format!("{}: failed independent check ({evidence_id})", c.criterion_id))
                }
                CriterionVerdict::NotYet { reason } => Some(format!("{}: {reason}", c.criterion_id)),
            })
            .collect()
    }
}

/// Everything recorded about a task's evidence.
#[derive(Debug, Clone, Copy, Default)]
pub struct Records<'a> {
    pub claims: &'a [Evidence],
    pub assessments: &'a [Evidence],
    pub runs: &'a [CheckRun],
}

/// The key evidence for this criterion must carry to count. `None` until the
/// task has an output tree.
pub fn currency_for(task: &Task, criterion: &Criterion) -> Option<Currency> {
    Some(Currency {
        tree: task.current_tree.clone()?,
        check_version: criterion.check_version.clone(),
        environment: task.environment.clone(),
        policy_digest: task.policy_digest.clone(),
    })
}

fn is_current(e: &Evidence, criterion: &Criterion, key: &Currency) -> bool {
    e.criterion_id == criterion.id && same_currency(&e.currency, key)
}

/// Trees compare by prefix so abbreviated object ids match full ones.
pub fn same_tree(a: &str, b: &str) -> bool {
    a.len() >= 7 && b.len() >= 7 && (a.starts_with(b) || b.starts_with(a))
}

pub fn same_currency(a: &Currency, b: &Currency) -> bool {
    same_tree(&a.tree, &b.tree)
        && a.check_version == b.check_version
        && a.environment == b.environment
        && a.policy_digest == b.policy_digest
}

/// Runs of this criterion's current check definition, in this environment.
fn runs_for<'a>(task: &'a Task, criterion: &'a Criterion, runs: &'a [CheckRun]) -> impl Iterator<Item = &'a CheckRun> {
    runs.iter().filter(move |r| {
        r.criterion_id == criterion.id
            && r.check_version == criterion.check_version
            && r.environment == task.environment
    })
}

fn producer_allowed(criterion: &Criterion, producer: RunProducer) -> bool {
    match criterion.producer {
        Producer::SelfReport => true,
        Producer::Independent => matches!(producer, RunProducer::Verifier | RunProducer::Operator),
    }
}

/// Why a criterion's baseline promise does not hold on the input snapshot,
/// once interlock has run its check there. Such a criterion can never pass:
/// a reproduction that already passes, or a regression guard that already
/// fails or tests nothing, proves nothing about the change.
pub fn baseline_problem(task: &Task, criterion: &Criterion, runs: &[CheckRun]) -> Option<String> {
    let base: Vec<&CheckRun> = runs_for(task, criterion, runs).filter(|r| r.target == RunTarget::Base).collect();
    if base.is_empty() || criterion.baseline == Baseline::Any {
        return None;
    }
    let vacuous = base.iter().find_map(|r| r.vacuous.clone());
    match criterion.baseline {
        Baseline::Fails if !base.iter().any(|r| r.failed()) => Some(match vacuous {
            Some(v) => format!("the check ran nothing on the input snapshot ({v}), so it cannot reproduce the problem"),
            None => {
                "the check already passes on the input snapshot, so it cannot show the change fixed anything".into()
            }
        }),
        Baseline::Passes if !base.iter().any(|r| r.passed()) => Some(match vacuous {
            Some(v) => format!("the check ran nothing on the input snapshot ({v}), so it guards nothing"),
            None => "the check already fails on the input snapshot, so it cannot guard against regressions".into(),
        }),
        _ => None,
    }
}

pub fn decide(task: &Task, criterion: &Criterion, records: Records<'_>) -> CriterionReport {
    let for_criterion = |e: &&Evidence| e.criterion_id == criterion.id;
    let total =
        records.claims.iter().filter(for_criterion).count() + records.assessments.iter().filter(for_criterion).count();
    let report = |verdict, claims: usize, assessments: usize, runs: usize, stale: usize| CriterionReport {
        criterion_id: criterion.id.clone(),
        verdict,
        current_claims: claims,
        current_assessments: assessments,
        current_runs: runs,
        stale,
    };

    let Some(key) = currency_for(task, criterion) else {
        let verdict = CriterionVerdict::NotYet { reason: "no output tree to check yet".into() };
        return report(verdict, 0, 0, 0, total);
    };

    let cur_claims: Vec<&Evidence> = records.claims.iter().filter(|e| is_current(e, criterion, &key)).collect();
    let cur_assess: Vec<&Evidence> = records.assessments.iter().filter(|e| is_current(e, criterion, &key)).collect();
    let cur_runs: Vec<&CheckRun> = runs_for(task, criterion, records.runs)
        .filter(|r| r.target == RunTarget::Output && same_tree(&r.tree, &key.tree))
        .collect();
    let stale = total - cur_claims.len() - cur_assess.len();
    let (nc, na, nr) = (cur_claims.len(), cur_assess.len(), cur_runs.len());

    // 2. Independent failures.
    if let Some(f) = cur_assess.iter().find(|e| e.strength == Strength::Failed) {
        let verdict = CriterionVerdict::Fail {
            evidence_id: f.id.clone(),
            attempt_id: f.attempt_id.clone(),
            note: f.note.clone(),
        };
        return report(verdict, nc, na, nr, stale);
    }
    if let Some(r) = cur_runs.iter().find(|r| r.producer != RunProducer::Worker && r.failed()) {
        let verdict = CriterionVerdict::Fail {
            evidence_id: r.id.clone(),
            attempt_id: r.attempt_id.clone().unwrap_or_default(),
            note: Some(format!("`{}` exited {}: {}", r.command, r.exit_code.unwrap_or(-1), tail(&r.output_tail))),
        };
        return report(verdict, nc, na, nr, stale);
    }

    // 3. Passes.
    let mut missing = Vec::new();
    if criterion.check.is_some() {
        if !cur_runs.iter().any(|r| r.passed() && producer_allowed(criterion, r.producer)) {
            missing.push(match criterion.producer {
                Producer::Independent => format!(
                    "a passing run of its check on this tree, made by interlock for a verifier \
                     (`interlock check run --criterion {}`)",
                    criterion.id
                ),
                Producer::SelfReport => format!(
                    "a passing run of its check on this tree, made by interlock (`interlock check run --criterion {}`)",
                    criterion.id
                ),
            });
            if let Some(v) = cur_runs.iter().find_map(|r| r.vacuous.as_deref()) {
                missing.push(format!("a run on this tree checked nothing ({v})"));
            }
        }
        if let Some(problem) = baseline_problem(task, criterion, records.runs) {
            missing.push(problem);
        } else if criterion.baseline != Baseline::Any
            && !runs_for(task, criterion, records.runs).any(|r| r.target == RunTarget::Base)
        {
            missing.push("a run of its check on the input snapshot".into());
        }
    }
    let allowed = cur_assess
        .iter()
        .map(|e| (Source::Assessment, *e))
        .chain(cur_claims.iter().filter(|_| criterion.producer == Producer::SelfReport).map(|e| (Source::Claim, *e)));
    let best = allowed
        .filter(|(_, e)| e.strength.satisfies(criterion.min_strength))
        .max_by_key(|(src, e)| (e.strength.pass_rank(), *src == Source::Assessment));
    let verdict = match best {
        Some((source, e)) if missing.is_empty() => {
            CriterionVerdict::Pass { strength: e.strength, source, evidence_id: e.id.clone() }
        }
        Some(_) => CriterionVerdict::NotYet { reason: format!("needs {}", missing.join("; and ")) },
        None => {
            let mut reason = not_yet_reason(criterion, &cur_claims, &cur_assess, stale);
            if !missing.is_empty() {
                reason = format!("{reason}; and {}", missing.join("; and "));
            }
            CriterionVerdict::NotYet { reason }
        }
    };
    report(verdict, nc, na, nr, stale)
}

fn tail(s: &str) -> String {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(3)..].join(" | ")
}

fn not_yet_reason(criterion: &Criterion, claims: &[&Evidence], assessments: &[&Evidence], stale: usize) -> String {
    let need = format!(
        "needs {} or stronger from {}",
        serde_json::to_value(criterion.min_strength).unwrap().as_str().unwrap_or("?"),
        match criterion.producer {
            Producer::Independent => "an independent verifier",
            Producer::SelfReport => "the worker or a verifier",
        }
    );
    let mut notes = Vec::new();
    if assessments.iter().any(|e| e.strength == Strength::Blocked) {
        notes.push("the verifier could not run the check".to_string());
    }
    if criterion.producer == Producer::Independent && !claims.is_empty() {
        notes.push(format!("{} worker claim(s) do not count here", claims.len()));
    }
    if stale > 0 {
        notes.push(format!("{stale} stale record(s) ignored"));
    }
    if notes.is_empty() { need } else { format!("{need}; {}", notes.join("; ")) }
}

pub fn evaluate(task: &Task, records: Records<'_>) -> EvidenceReport {
    let criteria: Vec<CriterionReport> = task.criteria.iter().map(|c| decide(task, c, records)).collect();
    let any_fail = criteria.iter().any(|c| matches!(c.verdict, CriterionVerdict::Fail { .. }));
    // No criteria means nothing was checked, which never counts as a pass.
    let all_pass = !criteria.is_empty() && criteria.iter().all(|c| matches!(c.verdict, CriterionVerdict::Pass { .. }));
    EvidenceReport { criteria, all_pass, any_fail }
}

/// Baseline problems across all criteria, for blocking a task before any
/// worker spends effort on it.
pub fn baseline_problems(task: &Task, runs: &[CheckRun]) -> Vec<(Id, String)> {
    task.criteria.iter().filter_map(|c| baseline_problem(task, c, runs).map(|p| (c.id.clone(), p))).collect()
}
