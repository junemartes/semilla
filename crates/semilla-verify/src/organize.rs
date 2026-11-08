//! The group and subgroup rules (design sections 6.2 and 6.3), and the subgroup suggestions.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::ops::{self, OpLevel};
use semilla_core::{ChangeSet, Class, ClaimResult, GroupKind, Report, Role, StepReport, Subgroup};

use crate::Options;

/// The suggested role of a claim, from the rule table and the computed class.
pub fn suggested_role(set: &ChangeSet, claim_id: &str, result: Option<&ClaimResult>, test_items: &BTreeSet<String>) -> Role {
    let Some((_, claim)) = set.claim(claim_id) else { return Role::Core };
    let info = ops::op(&claim.op);
    if claim.at.as_ref().is_some_and(|at| test_items.contains(&at.item)) {
        return Role::Tests;
    }
    if info.is_some_and(|info| info.level == OpLevel::File) {
        return Role::Config;
    }
    let role = info.map(|info| info.role).unwrap_or(Role::Core);
    let neutral = result.is_some_and(|result| result.class == Some(Class::Neutral) && result.status.is_verified());
    if role == Role::Core && neutral { Role::Mechanical } else { role }
}

fn role_title(role: Role) -> &'static str {
    match role {
        Role::Interface => "Interface",
        Role::Core => "Core logic",
        Role::Tests => "Tests",
        Role::Config => "Config",
        Role::Propagation => "Propagation",
        Role::Mechanical => "Mechanical",
        Role::Custom => "Custom",
    }
}

/// Suggests the subgroups of a group, by role.
pub fn suggest(set: &ChangeSet, group_id: &str, steps: &[StepReport], test_items: &BTreeSet<String>) -> Vec<Subgroup> {
    let Some(group) = set.groups.iter().find(|group| group.id == group_id) else { return Vec::new() };
    let results: BTreeMap<&str, &ClaimResult> = steps.iter().flat_map(|step| step.claims.iter()).map(|result| (result.claim.as_str(), result)).collect();
    let mut by_role: BTreeMap<Role, Vec<String>> = BTreeMap::new();
    for step in set.steps.iter().filter(|step| group.steps.contains(&step.index)) {
        for claim in &step.claims {
            let role = suggested_role(set, &claim.id, results.get(claim.id.as_str()).copied(), test_items);
            by_role.entry(role).or_default().push(claim.id.clone());
        }
    }
    let mut subgroups: Vec<Subgroup> = by_role
        .into_iter()
        .map(|(role, claims)| Subgroup { group: group_id.to_string(), role, title: role_title(role).to_string(), why: String::new(), claims, edited: false })
        .collect();
    subgroups.sort_by_key(|subgroup| subgroup.role.order());
    subgroups
}

/// Checks the groups and the subgroups. Errors go to `report.organization`.
pub fn check(set: &ChangeSet, steps: &[StepReport], report: &mut Report, options: Options) {
    let results: BTreeMap<&str, &ClaimResult> = steps.iter().flat_map(|step| step.claims.iter()).map(|result| (result.claim.as_str(), result)).collect();

    if set.groups.is_empty() {
        if options.require_groups && !set.steps.is_empty() {
            report.organization.push("the change set has no groups".into());
        } else if !set.steps.is_empty() {
            report.warnings.push("the change set has no groups yet".into());
        }
    }

    // Each step is in exactly one group.
    if !set.groups.is_empty() {
        for step in &set.steps {
            let count = set.groups.iter().filter(|group| group.steps.contains(&step.index)).count();
            if count != 1 {
                report.organization.push(format!("step {} is in {count} groups; it must be in exactly one", step.index));
            }
        }
    }

    // The kind rules.
    for group in &set.groups {
        let claims: Vec<(&str, &str)> = set
            .steps
            .iter()
            .filter(|step| group.steps.contains(&step.index))
            .flat_map(|step| step.claims.iter().map(|claim| (claim.id.as_str(), claim.op.as_str())))
            .collect();
        for (id, op) in &claims {
            let info = ops::op(op);
            let result = results.get(id);
            let problem = match group.kind {
                GroupKind::Refactor => (!result.is_some_and(|r| r.class == Some(Class::Neutral))).then(|| "a refactor group can contain only neutral claims"),
                GroupKind::Rename | GroupKind::Move => (!matches!(*op, "rename" | "move-item" | "rename-local")).then(|| "a rename or move group can contain only identity claims"),
                GroupKind::Api => (!info.is_some_and(|i| i.level == OpLevel::Item || i.role == Role::Propagation)).then(|| "an api group can contain only signature, type, and call site claims"),
                GroupKind::Deps => (!info.is_some_and(|i| i.level == OpLevel::File || i.role == Role::Propagation)).then(|| "a deps group can contain only dependency and call site claims"),
                GroupKind::Tests => None, // Checked below with the test items.
                GroupKind::Fix | GroupKind::Feature | GroupKind::Behavior => None,
            };
            if let Some(problem) = problem {
                report.organization.push(format!("group {}: claim {id} (`{op}`): {problem}", group.id));
            }
        }
    }

    // The subgroup rules.
    for subgroup in &set.subgroups {
        let Some(group) = set.groups.iter().find(|group| group.id == subgroup.group) else {
            report.organization.push(format!("subgroup `{}` names the missing group `{}`", subgroup.title, subgroup.group));
            continue;
        };
        for claim in &subgroup.claims {
            let Some((step, found)) = set.claim(claim) else {
                report.organization.push(format!("subgroup `{}` names the missing claim {claim}", subgroup.title));
                continue;
            };
            if !group.steps.contains(&step.index) {
                report.organization.push(format!("subgroup `{}`: claim {claim} is not in a step of group {}", subgroup.title, group.id));
            }
            let result = results.get(claim.as_str());
            let neutral_proved = result.is_some_and(|r| r.class == Some(Class::Neutral) && r.status.is_verified());
            let info = ops::op(&found.op);
            let problem = match subgroup.role {
                Role::Mechanical => (!neutral_proved).then(|| "a Mechanical subgroup can contain only proved neutral claims"),
                Role::Propagation => (!info.is_some_and(|i| i.role == Role::Propagation)).then(|| "a Propagation subgroup can contain only call site and rewrite claims"),
                Role::Interface => (!info.is_some_and(|i| i.level == OpLevel::Item)).then(|| "an Interface subgroup can contain only item claims"),
                Role::Config => (!info.is_some_and(|i| i.level == OpLevel::File)).then(|| "a Config subgroup can contain only file claims"),
                Role::Tests | Role::Core | Role::Custom => None,
            };
            if let Some(problem) = problem {
                report.organization.push(format!("subgroup `{}`: claim {claim} (`{}`): {problem}", subgroup.title, found.op));
            }
        }
    }
    // Each claim is in exactly one subgroup of its group (when the agent set subgroups).
    for group in &set.groups {
        let subgroups: Vec<&Subgroup> = set.subgroups.iter().filter(|s| s.group == group.id).collect();
        if subgroups.is_empty() {
            continue;
        }
        for step in set.steps.iter().filter(|step| group.steps.contains(&step.index)) {
            for claim in &step.claims {
                let count = subgroups.iter().filter(|s| s.claims.contains(&claim.id)).count();
                if count != 1 {
                    report.organization.push(format!("claim {} is in {count} subgroups of group {}; it must be in exactly one", claim.id, group.id));
                }
            }
        }
    }
}

/// The open or folded state of a subgroup: its contents decide, not its label.
pub fn is_open(subgroup: &Subgroup, steps: &[StepReport]) -> bool {
    let results: BTreeMap<&str, &ClaimResult> = steps.iter().flat_map(|step| step.claims.iter()).map(|result| (result.claim.as_str(), result)).collect();
    subgroup.claims.iter().any(|claim| !results.get(claim.as_str()).is_some_and(|r| r.class == Some(Class::Neutral) && r.status.is_verified()))
}

/// Checks the test rule of `tests` groups: all claims are on test items.
pub fn check_test_groups(set: &ChangeSet, test_items: &BTreeSet<String>, report: &mut Report) {
    for group in set.groups.iter().filter(|g| g.kind == GroupKind::Tests) {
        for step in set.steps.iter().filter(|s| group.steps.contains(&s.index)) {
            for claim in &step.claims {
                let on_test = claim.at.as_ref().is_some_and(|at| test_items.contains(&at.item));
                if !on_test {
                    report.organization.push(format!("group {}: claim {} is not on test code; a tests group can contain only test code", group.id, claim.id));
                }
            }
        }
    }
}
