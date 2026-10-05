//! Built-in v1 workflows: investigation, bug fix, feature, refactor.

use interlock_schema::{ActionClass, Role, ToolPolicy};
use serde::{Deserialize, Serialize};

use crate::capability::{Capability as C, Fallback, Requirement};
use crate::tools;

/// Interactive: the host agent drives and calls interlock at each step.
/// Headless: the supervisor starts sessions through an adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Interactive,
    Headless,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Integration {
    /// The workflow never delivers to a forge (investigations).
    Never,
    /// The task decides at creation.
    Optional,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleSpec {
    pub action_classes: Vec<ActionClass>,
    pub tools: ToolPolicy,
    pub requires: Vec<Requirement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workflow {
    pub name: String,
    pub version: u32,
    pub integration: Integration,
    pub worker: RoleSpec,
    pub verifier: RoleSpec,
}

impl Workflow {
    pub fn role(&self, role: Role) -> &RoleSpec {
        match role {
            Role::Worker => &self.worker,
            Role::Verifier | Role::Reviewer => &self.verifier,
        }
    }

    /// The capabilities a role needs in a given mode.
    pub fn requirements(&self, role: Role, mode: Mode) -> Vec<Requirement> {
        let mut reqs = self.role(role).requires.clone();
        match (mode, role) {
            (Mode::Headless, _) => {
                reqs.push(Requirement::new(&[C::SessionStart], None, "start a headless session"));
                reqs.push(Requirement::new(&[C::SessionCollect], None, "collect the session's result"));
                reqs.push(Requirement::new(&[C::SessionCancel], None, "cancel the session"));
            }
            (Mode::Interactive, Role::Verifier | Role::Reviewer) => {
                reqs.push(Requirement::new(
                    &[C::CustomAgents],
                    None,
                    "an isolated custom agent to verify independently",
                ));
            }
            (Mode::Interactive, Role::Worker) => {}
        }
        reqs
    }
}

fn policy(allow: &[&str], deny: &[&str]) -> ToolPolicy {
    ToolPolicy {
        allow: allow.iter().map(|s| s.to_string()).collect(),
        deny: deny.iter().map(|s| s.to_string()).collect(),
    }
}

fn scoped_tools() -> Requirement {
    Requirement::new(&[C::ToolRestriction, C::PerCallPolicy], None, "tool restriction enforced by the host")
}

fn optional_extras() -> Vec<Requirement> {
    vec![
        Requirement::new(&[C::Parallel], Some(Fallback::Sequential), "parallel sessions"),
        Requirement::new(&[C::ModelSelection], Some(Fallback::CurrentModel), "model selection"),
    ]
}

fn verifier() -> RoleSpec {
    let mut requires = vec![scoped_tools()];
    requires.extend(optional_extras());
    RoleSpec {
        action_classes: vec![ActionClass::Read, ActionClass::LocalReversible],
        tools: policy(&[tools::READ, tools::SHELL], &[tools::EDIT, "shell:git push", "shell:git commit"]),
        requires,
    }
}

fn changing_worker() -> RoleSpec {
    let mut requires = vec![scoped_tools()];
    requires.extend(optional_extras());
    RoleSpec {
        action_classes: vec![ActionClass::Read, ActionClass::LocalReversible],
        tools: policy(&[tools::READ, tools::EDIT, tools::SHELL], &["shell:git push"]),
        requires,
    }
}

pub fn builtins() -> Vec<Workflow> {
    let investigation_worker = RoleSpec {
        tools: policy(&[tools::READ, tools::SHELL, tools::WEB], &[tools::EDIT, "shell:git push", "shell:git commit"]),
        ..changing_worker()
    };
    vec![
        Workflow {
            name: "investigation".into(),
            version: 1,
            integration: Integration::Never,
            worker: investigation_worker,
            verifier: verifier(),
        },
        Workflow {
            name: "bug-fix".into(),
            version: 1,
            integration: Integration::Optional,
            worker: changing_worker(),
            verifier: verifier(),
        },
        Workflow {
            name: "feature".into(),
            version: 1,
            integration: Integration::Optional,
            worker: changing_worker(),
            verifier: verifier(),
        },
        Workflow {
            name: "refactor".into(),
            version: 1,
            integration: Integration::Optional,
            worker: changing_worker(),
            verifier: verifier(),
        },
    ]
}

pub fn builtin(name: &str, version: u32) -> Option<Workflow> {
    builtins().into_iter().find(|w| w.name == name && w.version == version)
}

pub fn latest(name: &str) -> Option<Workflow> {
    builtins().into_iter().filter(|w| w.name == name).max_by_key(|w| w.version)
}
