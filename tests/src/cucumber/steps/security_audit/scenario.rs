use super::{
    BTreeMap, CucumberWorld, HashSet, SecurityAuditParticipants, SecurityAuditWorkload,
    SecurityAuditWorkloadPolicy, Step, StepError, StepResult, given,
};

#[given("the security audit participants are:")]
fn step_declare_audit_participants(world: &mut CucumberWorld, step: &Step) -> StepResult {
    if world.security_audit_participants.is_some() {
        return Err(StepError::InvalidArgument {
            message: "security audit participants are already declared".to_owned(),
        });
    }
    let table = step.table.as_ref().ok_or(StepError::MissingTable)?;
    world.security_audit_participants = Some(
        parse_audit_participants(&table.rows)
            .map_err(|message| StepError::InvalidArgument { message })?,
    );
    Ok(())
}

pub(super) fn parse_audit_participants(
    rows: &[Vec<String>],
) -> Result<SecurityAuditParticipants, String> {
    if rows.first().map(Vec::as_slice)
        != Some(&[
            "node".to_owned(),
            "role".to_owned(),
            "canonical_reference".to_owned(),
        ])
    {
        return Err(
            "participant table requires node, role and canonical_reference columns".to_owned(),
        );
    }
    let mut seen = HashSet::new();
    let mut producer = None;
    let mut honest_nodes = Vec::new();
    let mut canonical_reference = None;
    for row in &rows[1..] {
        let [name, role, reference] = row.as_slice() else {
            return Err(
                "each participant requires a node, role and canonical reference".to_owned(),
            );
        };
        if name.trim().is_empty() || !seen.insert(name.clone()) {
            return Err(format!("participant {name} is empty or duplicated"));
        }
        match (role.as_str(), reference.as_str()) {
            ("producer", "no") if producer.is_none() => producer = Some(name.clone()),
            ("honest", "no") => honest_nodes.push(name.clone()),
            ("honest", "yes") if canonical_reference.is_none() => {
                honest_nodes.push(name.clone());
                canonical_reference = Some(name.clone());
            }
            _ => {
                return Err(format!(
                    "invalid or conflicting participant role for {name}"
                ));
            }
        }
    }
    if honest_nodes.is_empty() {
        return Err("at least one honest node is required".to_owned());
    }
    Ok(SecurityAuditParticipants {
        producer: producer.ok_or_else(|| "exactly one producer is required".to_owned())?,
        honest_nodes,
        canonical_reference: canonical_reference
            .ok_or_else(|| "one honest canonical reference is required".to_owned())?,
    })
}

pub(super) fn validate_audit_participants(
    participants: &SecurityAuditParticipants,
    available_nodes: &HashSet<String>,
) -> Result<(), String> {
    let declared = std::iter::once(&participants.producer)
        .chain(&participants.honest_nodes)
        .cloned()
        .collect::<HashSet<_>>();
    if &declared != available_nodes {
        return Err(format!(
            "declared audit participants {declared:?} do not match started nodes {available_nodes:?}"
        ));
    }
    Ok(())
}

pub(super) fn audit_participants(
    world: &CucumberWorld,
) -> Result<&SecurityAuditParticipants, String> {
    let participants = world
        .security_audit_participants
        .as_ref()
        .ok_or_else(|| "security audit participants were not declared".to_owned())?;
    let started = world.nodes_info.keys().cloned().collect();
    validate_audit_participants(participants, &started)?;
    Ok(participants)
}

const SIBLING_CONFIG_PATH: &str = "cryptarchia.leader.security_audit.sibling_blocks_per_leadership";

pub(super) fn startup_sibling_count(
    overrides: &[crate::cucumber::world::ConfigOverride],
) -> Result<usize, String> {
    let setting = overrides
        .iter()
        .find(|item| item.path == SIBLING_CONFIG_PATH)
        .ok_or_else(|| format!("missing startup override {SIBLING_CONFIG_PATH}"))?;
    setting
        .value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| format!("invalid startup override {SIBLING_CONFIG_PATH}"))
}

pub(super) fn audit_sibling_count(world: &CucumberWorld) -> Result<usize, String> {
    let participants = audit_participants(world)?;
    let node = world
        .nodes_info
        .get(&participants.producer)
        .ok_or_else(|| "audit producer was not started".to_owned())?;
    startup_sibling_count(&node.startup_user_config_overrides)
}

#[given("the audit transaction workload is:")]
fn step_audit_workload_configuration(world: &mut CucumberWorld, step: &Step) -> StepResult {
    if world.security_audit_workload.is_some() {
        return Err(StepError::InvalidArgument {
            message: "audit workload was already declared".to_owned(),
        });
    }
    let table = step.table.as_ref().ok_or(StepError::MissingTable)?;
    world.security_audit_workload = Some(
        parse_audit_workload(&table.rows)
            .map_err(|message| StepError::InvalidArgument { message })?,
    );
    Ok(())
}

pub(super) fn parse_audit_workload(rows: &[Vec<String>]) -> Result<SecurityAuditWorkload, String> {
    let mut values = BTreeMap::new();
    for row in rows {
        let [key, value] = row.as_slice() else {
            return Err("workload entries require a key and value".to_owned());
        };
        if values.insert(key.as_str(), value.as_str()).is_some() {
            return Err(format!("duplicate workload setting {key}"));
        }
    }
    let get = |name: &str| {
        values
            .get(name)
            .copied()
            .ok_or_else(|| format!("missing workload setting {name}"))
    };
    let number = |name: &str| {
        get(name)?
            .parse::<u64>()
            .map_err(|_| format!("invalid workload setting {name}"))
    };
    let sender = get("sender_wallet")?.to_owned();
    let receiver = get("receiver_wallet")?.to_owned();
    let amount = number("amount_lgo")?;
    let budget = usize::try_from(number("maximum_transactions")?)
        .map_err(|_| "invalid transaction budget".to_owned())?;
    if sender.is_empty() || receiver.is_empty() || sender == receiver || amount == 0 || budget == 0
    {
        return Err("workload requires distinct wallets and positive amount and budget".to_owned());
    }
    let policy = if values.contains_key("pending_sample_seconds") && values.len() == 5 {
        let pending_seconds = number("pending_sample_seconds")?;
        if pending_seconds == 0 {
            return Err("pending sample duration must be positive".to_owned());
        }
        SecurityAuditWorkloadPolicy::Nonempty {
            budget,
            pending_seconds,
        }
    } else if values.contains_key("initial_pending_transactions")
        && values.contains_key("submission_interval_seconds")
        && values.len() == 6
    {
        let initial_pending = usize::try_from(number("initial_pending_transactions")?)
            .map_err(|_| "invalid initial transaction count".to_owned())?;
        let interval_seconds = number("submission_interval_seconds")?;
        if initial_pending > budget || interval_seconds == 0 {
            return Err("invalid sustained workload scheduling".to_owned());
        }
        SecurityAuditWorkloadPolicy::Sustained {
            budget,
            initial_pending,
            interval_seconds,
        }
    } else {
        return Err(
            "workload must specify either pending sampling or sustained submission scheduling"
                .to_owned(),
        );
    };
    Ok(SecurityAuditWorkload {
        sender,
        receiver,
        amount,
        policy,
    })
}

pub(super) fn audit_workload(world: &CucumberWorld) -> Result<&SecurityAuditWorkload, String> {
    world
        .security_audit_workload
        .as_ref()
        .ok_or_else(|| "audit workload was not declared".to_owned())
}

#[cfg(test)]
mod workload_tests {
    use super::*;

    fn rows(entries: &[(&str, &str)]) -> Vec<Vec<String>> {
        entries
            .iter()
            .map(|(key, value)| vec![(*key).into(), (*value).into()])
            .collect()
    }

    #[test]
    fn validates_both_workload_policies_and_rejects_invalid_settings() {
        let common = [
            ("sender_wallet", "sender"),
            ("receiver_wallet", "receiver"),
            ("amount_lgo", "10"),
            ("maximum_transactions", "80"),
        ];
        let mut entries = common.to_vec();
        entries.push(("pending_sample_seconds", "30"));
        assert!(matches!(
            parse_audit_workload(&rows(&entries)).unwrap().policy,
            SecurityAuditWorkloadPolicy::Nonempty {
                budget: 80,
                pending_seconds: 30
            }
        ));
        entries.push(("pending_sample_seconds", "30"));
        assert!(parse_audit_workload(&rows(&entries)).is_err());
        let mut entries = common.to_vec();
        entries.extend([
            ("initial_pending_transactions", "8"),
            ("submission_interval_seconds", "25"),
        ]);
        assert!(matches!(
            parse_audit_workload(&rows(&entries)).unwrap().policy,
            SecurityAuditWorkloadPolicy::Sustained {
                budget: 80,
                initial_pending: 8,
                interval_seconds: 25
            }
        ));
        entries[4].1 = "81";
        assert!(parse_audit_workload(&rows(&entries)).is_err());
        assert!(parse_audit_workload(&rows(&common)).is_err());
    }
}
