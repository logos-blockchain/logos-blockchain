use super::{
    CucumberWorld, Duration, HashMap, NodesToStartUnordered, NonZero, Step, StepResult, TARGET,
    WalletStartInfo, ZONE_SECURITY_PARAM, ZoneNodeResourcesRow, keygen,
    set_deployment_config_override, start_node, start_nodes_order_respecting_dependencies, warn,
};

pub(in super::super) fn register_zone_sequencers_with_shared_key(
    world: &mut CucumberWorld,
    source_alias: &str,
    aliases: Vec<String>,
) -> StepResult {
    let signing_key = world.zone.sequencer_signing_key(source_alias)?.clone();

    for alias in aliases {
        world.zone.register_sequencer(alias, signing_key.clone());
    }

    Ok(())
}

pub(in super::super) async fn start_nodes_with_zone_resources(
    world: &mut CucumberWorld,
    step: &Step,
    rows: Vec<ZoneNodeResourcesRow>,
) -> StepResult {
    apply_zone_timing_defaults(world, &step.value)?;

    let nodes = collect_zone_nodes_to_start(&rows);
    let nodes = start_nodes_order_respecting_dependencies(
        nodes,
        world.nodes_info.keys().cloned().collect(),
    )
    .inspect_err(|error| {
        warn!(target: TARGET, "Step `{}` error: {error}", step.value);
    })?;

    for (node_name, wallet_start_info, mut initial_peers) in nodes {
        initial_peers.sort();
        initial_peers.dedup();

        start_node(
            world,
            &step.value,
            &node_name,
            &wallet_start_info,
            &initial_peers,
            false,
            &[],
        )
        .await?;
    }

    register_zone_resources(world, rows)
}

fn apply_zone_timing_defaults(world: &mut CucumberWorld, step: &str) -> StepResult {
    world.set_cryptarchia_security_param(
        NonZero::new(ZONE_SECURITY_PARAM).expect("zone security parameter is non-zero"),
    );
    world.set_prolonged_bootstrap_period(Duration::ZERO);

    set_deployment_config_override(world, step, "time.slot_duration", "seconds(1)")?;
    set_deployment_config_override(
        world,
        step,
        "cryptarchia.slot_activation_coeff.numerator",
        "1",
    )?;
    set_deployment_config_override(
        world,
        step,
        "cryptarchia.slot_activation_coeff.denominator",
        "2",
    )
}

fn collect_zone_nodes_to_start(rows: &[ZoneNodeResourcesRow]) -> NodesToStartUnordered {
    let mut nodes = HashMap::new();

    for row in rows {
        let entry = nodes
            .entry(row.node_name.clone())
            .or_insert_with(|| (Vec::new(), Vec::new()));

        entry.0.push(WalletStartInfo {
            wallet_name: row.wallet_name.clone(),
            account_index: row.account_index,
        });

        if let Some(peer) = &row.connected_to {
            entry.1.push(peer.clone());
        }
    }

    nodes
}

fn register_zone_resources(
    world: &mut CucumberWorld,
    rows: Vec<ZoneNodeResourcesRow>,
) -> StepResult {
    for row in rows {
        for alias in row.sequencers {
            if !world.zone.has_sequencer(&alias) {
                world.zone.register_sequencer(alias.clone(), keygen());
            }

            world.zone.attach_sequencer_resources(
                &alias,
                row.node_name.clone(),
                row.wallet_name.clone(),
            )?;
        }
    }

    Ok(())
}
