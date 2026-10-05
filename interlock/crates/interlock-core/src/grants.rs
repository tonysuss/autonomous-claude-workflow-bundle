//! Authorization. Effective grant = user grants ∩ host policy ∩ task needs.
//! The irreversible class can never be granted.

use std::collections::BTreeSet;

use interlock_schema::{ActionClass, EffectiveGrant, Grant, LandingAuthority, Timestamp, ToolPolicy};
use serde::{Deserialize, Serialize};

use crate::tools;
use crate::workflow::RoleSpec;

/// What the operator allows without an explicit grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    /// Read and local reversible actions within task scope. The default.
    Conservative,
    /// Adds external reversible actions such as PR comments.
    Permissive,
}

impl Profile {
    pub fn baseline(self) -> &'static [ActionClass] {
        match self {
            Profile::Conservative => &[ActionClass::Read, ActionClass::LocalReversible],
            Profile::Permissive => &[ActionClass::Read, ActionClass::LocalReversible, ActionClass::ExternalReversible],
        }
    }
}

/// What the host configuration permits at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostPolicy {
    pub action_classes: Vec<ActionClass>,
    pub tools: ToolPolicy,
}

impl HostPolicy {
    /// Permits every grantable class and every tool family.
    pub fn open() -> Self {
        HostPolicy {
            action_classes: vec![
                ActionClass::Read,
                ActionClass::LocalReversible,
                ActionClass::ExternalReversible,
                ActionClass::Landing,
            ],
            tools: ToolPolicy {
                allow: [tools::READ, tools::EDIT, tools::SHELL, tools::WEB, tools::AGENT, "mcp"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                deny: vec![],
            },
        }
    }
}

/// Checks a grant before it is stored.
pub fn validate(grant: &Grant) -> Result<(), String> {
    if grant.action_classes.contains(&ActionClass::Irreversible) {
        return Err("no grant can cover the irreversible class".into());
    }
    if grant.task_scope.is_empty() {
        return Err("a grant needs a task scope; use [\"*\"] for every task".into());
    }
    let lands = grant.action_classes.contains(&ActionClass::Landing);
    match (lands, grant.landing_authority) {
        (true, LandingAuthority::None) => Err("the landing class needs a landing authority".into()),
        (false, a) if a != LandingAuthority::None => Err("a landing authority needs the landing class".into()),
        _ => Ok(()),
    }
}

pub fn is_active(grant: &Grant, task_id: &str, now: Timestamp) -> bool {
    grant.revoked_at.is_none_or(|t| t > now)
        && grant.expires_at.is_none_or(|t| t > now)
        && grant.task_scope.iter().any(|s| s == "*" || s == task_id)
}

pub fn effective(
    task_id: &str,
    profile: Profile,
    grants: &[Grant],
    host: &HostPolicy,
    needs: &RoleSpec,
    now: Timestamp,
) -> EffectiveGrant {
    let active: Vec<&Grant> = grants.iter().filter(|g| is_active(g, task_id, now) && validate(g).is_ok()).collect();

    let mut user: BTreeSet<ActionClass> = profile.baseline().iter().copied().collect();
    for g in &active {
        user.extend(g.action_classes.iter().copied());
    }
    user.remove(&ActionClass::Irreversible);
    let host_classes: BTreeSet<ActionClass> = host.action_classes.iter().copied().collect();
    let need_classes: BTreeSet<ActionClass> = needs.action_classes.iter().copied().collect();
    let action_classes: Vec<ActionClass> =
        user.intersection(&host_classes).filter(|c| need_classes.contains(c)).copied().collect();

    let allow: Vec<String> = needs
        .tools
        .allow
        .iter()
        .filter(|t| host.tools.allow.iter().any(|h| tools::pattern_matches(h, t)))
        .cloned()
        .collect();
    let mut deny: Vec<String> = Vec::new();
    for d in needs.tools.deny.iter().chain(&host.tools.deny).chain(active.iter().flat_map(|g| &g.tools.deny)) {
        if !deny.contains(d) {
            deny.push(d.clone());
        }
    }

    EffectiveGrant {
        landing_authority: if action_classes.contains(&ActionClass::Landing) {
            landing_authority(task_id, grants, now)
        } else {
            LandingAuthority::None
        },
        action_classes,
        tools: ToolPolicy { allow, deny },
        grant_ids: active.iter().map(|g| g.id.clone()).collect(),
    }
}

/// The landing authority from the most recent active grant that covers landing.
pub fn landing_authority(task_id: &str, grants: &[Grant], now: Timestamp) -> LandingAuthority {
    grants
        .iter()
        .filter(|g| is_active(g, task_id, now) && validate(g).is_ok())
        .filter(|g| g.action_classes.contains(&ActionClass::Landing))
        .max_by_key(|g| g.created_at)
        .map_or(LandingAuthority::None, |g| g.landing_authority)
}

pub fn missing_classes(grant: &EffectiveGrant, required: &[ActionClass]) -> Vec<ActionClass> {
    required.iter().filter(|c| !grant.action_classes.contains(c)).copied().collect()
}

/// Whether the effective grant allows one action, given its class and tool.
pub fn allows(grant: &EffectiveGrant, class: ActionClass, tool: &str) -> bool {
    class != ActionClass::Irreversible && grant.action_classes.contains(&class) && tools::permits(&grant.tools, tool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow;
    use chrono::{Duration, TimeZone, Utc};

    fn now() -> Timestamp {
        Utc.with_ymd_and_hms(2026, 10, 5, 22, 0, 0).unwrap()
    }

    fn grant(classes: &[ActionClass], authority: LandingAuthority, expires_in_h: Option<i64>) -> Grant {
        Grant {
            id: "g".into(),
            principal: "operator".into(),
            task_scope: vec!["t1".into()],
            action_classes: classes.to_vec(),
            tools: ToolPolicy::default(),
            landing_authority: authority,
            origin: "going to bed, keep going".into(),
            created_at: now() - Duration::hours(1),
            expires_at: expires_in_h.map(|h| now() + Duration::hours(h)),
            revoked_at: None,
        }
    }

    #[test]
    fn irreversible_is_never_grantable() {
        assert!(validate(&grant(&[ActionClass::Irreversible], LandingAuthority::None, None)).is_err());
        let needs = RoleSpec {
            action_classes: vec![ActionClass::Read, ActionClass::Irreversible],
            ..workflow::builtin("bug-fix", 1).unwrap().worker
        };
        let mut g = grant(&[ActionClass::Read], LandingAuthority::None, None);
        g.action_classes.push(ActionClass::Irreversible);
        let eg = effective("t1", Profile::Permissive, &[g], &HostPolicy::open(), &needs, now());
        assert!(!eg.action_classes.contains(&ActionClass::Irreversible));
    }

    #[test]
    fn grants_expire_and_are_scoped() {
        let g = grant(&[ActionClass::ExternalReversible], LandingAuthority::None, Some(8));
        assert!(is_active(&g, "t1", now()));
        assert!(!is_active(&g, "t2", now()));
        assert!(!is_active(&g, "t1", now() + Duration::hours(9)));
    }

    #[test]
    fn effective_grant_is_an_intersection() {
        let mut needs = workflow::builtin("bug-fix", 1).unwrap().worker;
        needs.action_classes.push(ActionClass::ExternalReversible);
        let host = HostPolicy::open();
        // Conservative and no grant: external is needed but not granted.
        let eg = effective("t1", Profile::Conservative, &[], &host, &needs, now());
        assert_eq!(missing_classes(&eg, &needs.action_classes), vec![ActionClass::ExternalReversible]);
        // A scoped grant fills the gap.
        let g = grant(&[ActionClass::ExternalReversible], LandingAuthority::None, Some(8));
        let eg = effective("t1", Profile::Conservative, &[g], &host, &needs, now());
        assert!(missing_classes(&eg, &needs.action_classes).is_empty());
        // Granted but not needed: not included.
        let g = grant(&[ActionClass::Landing], LandingAuthority::Operator, None);
        let eg = effective("t1", Profile::Conservative, &[g], &host, &needs, now());
        assert!(!eg.action_classes.contains(&ActionClass::Landing));
        assert_eq!(eg.landing_authority, LandingAuthority::None);
    }

    #[test]
    fn host_deny_and_grant_deny_both_apply() {
        let needs = workflow::builtin("bug-fix", 1).unwrap().worker;
        let mut host = HostPolicy::open();
        host.tools.deny.push("shell:curl".into());
        let mut g = grant(&[ActionClass::Read], LandingAuthority::None, None);
        g.tools.deny.push("shell:npm publish".into());
        let eg = effective("t1", Profile::Conservative, &[g], &host, &needs, now());
        assert!(allows(&eg, ActionClass::LocalReversible, "shell:cargo test"));
        assert!(!allows(&eg, ActionClass::LocalReversible, "shell:curl example.com"));
        assert!(!allows(&eg, ActionClass::LocalReversible, "shell:npm publish"));
        assert!(!allows(&eg, ActionClass::ExternalReversible, "shell:git push"));
    }
}
