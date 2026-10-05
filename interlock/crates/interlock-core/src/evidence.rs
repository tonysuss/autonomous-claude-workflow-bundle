//! Evidence policy v1. Worker claims and verifier assessments are kept apart;
//! the policy reads both and decides, in a fixed order:
//!
//! 1. Keep only current evidence: its currency key matches the task.
//! 2. Any current independent failure fails the criterion, whatever else says pass.
//! 3. Required strength from an allowed producer passes it.
//! 4. Otherwise, not yet.

use interlock_schema::{Criterion, Currency, Evidence, Id, Producer, Strength, Task};
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
pub fn same_currency(a: &Currency, b: &Currency) -> bool {
    let trees_match =
        a.tree.len() >= 7 && b.tree.len() >= 7 && (a.tree.starts_with(&b.tree) || b.tree.starts_with(&a.tree));
    trees_match
        && a.check_version == b.check_version
        && a.environment == b.environment
        && a.policy_digest == b.policy_digest
}

pub fn decide(task: &Task, criterion: &Criterion, claims: &[Evidence], assessments: &[Evidence]) -> CriterionReport {
    let for_criterion = |e: &&Evidence| e.criterion_id == criterion.id;
    let total = claims.iter().filter(for_criterion).count() + assessments.iter().filter(for_criterion).count();

    let Some(key) = currency_for(task, criterion) else {
        return CriterionReport {
            criterion_id: criterion.id.clone(),
            verdict: CriterionVerdict::NotYet { reason: "no output tree to check yet".into() },
            current_claims: 0,
            current_assessments: 0,
            stale: total,
        };
    };

    let cur_claims: Vec<&Evidence> = claims.iter().filter(|e| is_current(e, criterion, &key)).collect();
    let cur_assess: Vec<&Evidence> = assessments.iter().filter(|e| is_current(e, criterion, &key)).collect();
    let stale = total - cur_claims.len() - cur_assess.len();

    let verdict = if let Some(f) = cur_assess.iter().find(|e| e.strength == Strength::Failed) {
        CriterionVerdict::Fail { evidence_id: f.id.clone(), attempt_id: f.attempt_id.clone(), note: f.note.clone() }
    } else {
        let allowed = cur_assess.iter().map(|e| (Source::Assessment, *e)).chain(
            cur_claims.iter().filter(|_| criterion.producer == Producer::SelfReport).map(|e| (Source::Claim, *e)),
        );
        let best = allowed
            .filter(|(_, e)| e.strength.satisfies(criterion.min_strength))
            .max_by_key(|(src, e)| (e.strength.pass_rank(), *src == Source::Assessment));
        match best {
            Some((source, e)) => CriterionVerdict::Pass { strength: e.strength, source, evidence_id: e.id.clone() },
            None => CriterionVerdict::NotYet { reason: not_yet_reason(criterion, &cur_claims, &cur_assess, stale) },
        }
    };

    CriterionReport {
        criterion_id: criterion.id.clone(),
        verdict,
        current_claims: cur_claims.len(),
        current_assessments: cur_assess.len(),
        stale,
    }
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

pub fn evaluate(task: &Task, claims: &[Evidence], assessments: &[Evidence]) -> EvidenceReport {
    let criteria: Vec<CriterionReport> = task.criteria.iter().map(|c| decide(task, c, claims, assessments)).collect();
    let any_fail = criteria.iter().any(|c| matches!(c.verdict, CriterionVerdict::Fail { .. }));
    // No criteria means nothing was checked, which never counts as a pass.
    let all_pass = !criteria.is_empty() && criteria.iter().all(|c| matches!(c.verdict, CriterionVerdict::Pass { .. }));
    EvidenceReport { criteria, all_pass, any_fail }
}
