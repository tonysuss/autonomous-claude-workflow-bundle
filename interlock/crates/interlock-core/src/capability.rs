//! Versioned host capabilities. Adapters advertise what they offer; the core
//! decides what to do when something is missing.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub const CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Start a headless session from a brief.
    SessionStart,
    /// Collect a finished session's final output.
    SessionCollect,
    /// Cancel a running session.
    SessionCancel,
    /// Stream lifecycle events while a session runs.
    EventStream,
    /// Restrict the tools a session can use, enforced by the host.
    ToolRestriction,
    /// Consult interlock before each tool call (host hooks).
    PerCallPolicy,
    /// Stop an agent from finishing until interlock agrees (host hooks).
    StopGuard,
    /// Choose the model per session.
    ModelSelection,
    /// Define isolated custom agents or sub-agents per attempt.
    CustomAgents,
    /// Run several sessions at once.
    Parallel,
    /// Choose the reasoning effort per session.
    EffortSelection,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilitySet(pub BTreeSet<Capability>);

impl CapabilitySet {
    pub fn has(&self, c: Capability) -> bool {
        self.0.contains(&c)
    }
}

impl FromIterator<Capability> for CapabilitySet {
    fn from_iter<I: IntoIterator<Item = Capability>>(iter: I) -> Self {
        CapabilitySet(iter.into_iter().collect())
    }
}

/// What to do instead when a requirement is not met.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fallback {
    /// Run steps one after another.
    Sequential,
    /// Use and record the session's current model.
    CurrentModel,
}

/// Met by any one of `any_of`. With no fallback, a missing requirement blocks the step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    pub any_of: Vec<Capability>,
    pub fallback: Option<Fallback>,
    pub why: String,
}

impl Requirement {
    pub fn new(any_of: &[Capability], fallback: Option<Fallback>, why: &str) -> Self {
        Requirement { any_of: any_of.to_vec(), fallback, why: why.to_string() }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityCheck {
    pub fallbacks: Vec<(Fallback, String)>,
    pub missing: Vec<String>,
}

impl CapabilityCheck {
    pub fn ok(&self) -> bool {
        self.missing.is_empty()
    }
}

pub fn check(requirements: &[Requirement], available: &CapabilitySet) -> CapabilityCheck {
    let mut out = CapabilityCheck::default();
    for req in requirements {
        if req.any_of.iter().any(|c| available.has(*c)) {
            continue;
        }
        match req.fallback {
            Some(f) => out.fallbacks.push((f, req.why.clone())),
            None => out.missing.push(req.why.clone()),
        }
    }
    out
}
