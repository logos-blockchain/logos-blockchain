use std::{fs, num::NonZero, time::Duration};

use cucumber::{gherkin::Step, given, when};
use hex::ToHex as _;
use lb_chain_service::ChainServiceInfo;
use lb_node::config::DeploymentSettings;
use lb_testing_framework::{NodeHttpClient, USER_CONFIG_FILE, configs::deployment::TopologyConfig};
use time::OffsetDateTime;
use tokio::time::{Instant, sleep};
use tracing::{info, warn};

use crate::cucumber::{
    TARGET,
    error::{StepError, StepResult},
    steps::manual_nodes::config_override::set_deployment_config_override,
    utils::{peer_id_from_node_yaml, user_config_from_node_yaml},
    world::{BlendDiagnosticPhase, CucumberWorld},
};

const DIAGNOSTIC: &str = "blend_tsi_outage";

fn diagnostic_node_sort_number(node_name: &str) -> Option<u32> {
    let number = node_name.strip_prefix("NODE_")?;
    number.parse::<u32>().ok()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BlendDiagnosticParameterSet {
    name: &'static str,
    security_parameter: u32,
    slot_duration_secs: u64,
    epoch_stake_distribution_stabilization: u8,
    epoch_period_nonce_buffer: u8,
    epoch_period_nonce_stabilization: u8,
}

impl BlendDiagnosticParameterSet {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "clean_control" => Some(Self {
                name: "clean_control",
                security_parameter: 10,
                slot_duration_secs: 1,
                epoch_stake_distribution_stabilization: 1,
                epoch_period_nonce_buffer: 1,
                epoch_period_nonce_stabilization: 1,
            }),
            "testnet_representative" => Some(Self {
                name: "testnet_representative",
                security_parameter: 5,
                slot_duration_secs: 1,
                epoch_stake_distribution_stabilization: 3,
                epoch_period_nonce_buffer: 3,
                epoch_period_nonce_stabilization: 4,
            }),
            "fast_repro" => Some(Self {
                name: "fast_repro",
                security_parameter: 3,
                slot_duration_secs: 1,
                epoch_stake_distribution_stabilization: 1,
                epoch_period_nonce_buffer: 1,
                epoch_period_nonce_stabilization: 1,
            }),
            _ => None,
        }
    }

    const fn apply_to(&self, settings: &mut DeploymentSettings) {
        settings.cryptarchia.security_param = NonZero::new(self.security_parameter)
            .expect("named Blend diagnostic security parameter must be non-zero");
        settings.time.slot_duration = Duration::from_secs(self.slot_duration_secs);
        settings
            .cryptarchia
            .epoch_config
            .epoch_stake_distribution_stabilization =
            NonZero::new(self.epoch_stake_distribution_stabilization)
                .expect("named Blend diagnostic phase must be non-zero");
        settings.cryptarchia.epoch_config.epoch_period_nonce_buffer =
            NonZero::new(self.epoch_period_nonce_buffer)
                .expect("named Blend diagnostic phase must be non-zero");
        settings
            .cryptarchia
            .epoch_config
            .epoch_period_nonce_stabilization = NonZero::new(self.epoch_period_nonce_stabilization)
            .expect("named Blend diagnostic phase must be non-zero");
    }

    fn effective_deployment_settings(self) -> DeploymentSettings {
        let mut settings = DeploymentSettings::default();
        settings.cryptarchia.slot_activation_coeff = TopologyConfig::default().active_slot_coeff;
        self.apply_to(&mut settings);
        settings
    }
}

pub fn set_blend_diagnostic_parameter_set(
    world: &mut CucumberWorld,
    step: &str,
    parameter_set_name: &str,
) -> StepResult {
    let parameter_set = BlendDiagnosticParameterSet::from_name(parameter_set_name).ok_or_else(|| {
        StepError::InvalidArgument {
            message: format!(
                "unknown Blend diagnostic parameter set `{parameter_set_name}`; expected `clean_control`, `testnet_representative`, or `fast_repro`"
            ),
        }
    })?;

    let security_parameter = NonZero::new(parameter_set.security_parameter).ok_or_else(|| {
        StepError::InvalidArgument {
            message: format!(
                "Blend diagnostic parameter set `{parameter_set_name}` has an invalid security parameter"
            ),
        }
    })?;
    world.set_cryptarchia_security_param(security_parameter);
    set_deployment_config_override(
        world,
        step,
        "time.slot_duration",
        &format!("seconds({})", parameter_set.slot_duration_secs),
    )?;
    set_deployment_config_override(
        world,
        step,
        "cryptarchia.epoch_config.epoch_stake_distribution_stabilization",
        &parameter_set
            .epoch_stake_distribution_stabilization
            .to_string(),
    )?;
    set_deployment_config_override(
        world,
        step,
        "cryptarchia.epoch_config.epoch_period_nonce_buffer",
        &parameter_set.epoch_period_nonce_buffer.to_string(),
    )?;
    set_deployment_config_override(
        world,
        step,
        "cryptarchia.epoch_config.epoch_period_nonce_stabilization",
        &parameter_set.epoch_period_nonce_stabilization.to_string(),
    )?;

    let settings = parameter_set.effective_deployment_settings();
    let slots_per_epoch = settings.cryptarchia.slots_per_epoch();
    info!(
        target: TARGET,
        diagnostic = DIAGNOSTIC,
        event = "blend_diagnostic_parameter_set",
        parameter_set = parameter_set.name,
        security_parameter = settings.cryptarchia.security_param.get(),
        slot_duration_secs = settings.time.slot_duration.as_secs(),
        epoch_stake_distribution_stabilization = settings
            .cryptarchia
            .epoch_config
            .epoch_stake_distribution_stabilization
            .get(),
        epoch_period_nonce_buffer = settings
            .cryptarchia
            .epoch_config
            .epoch_period_nonce_buffer
            .get(),
        epoch_period_nonce_stabilization = settings
            .cryptarchia
            .epoch_config
            .epoch_period_nonce_stabilization
            .get(),
        slots_per_epoch,
        "Blend diagnostic parameters: {}: k={}, phases={}/{}/{}, slots_per_epoch={}",
        parameter_set.name,
        settings.cryptarchia.security_param.get(),
        settings
            .cryptarchia
            .epoch_config
            .epoch_stake_distribution_stabilization
            .get(),
        settings
            .cryptarchia
            .epoch_config
            .epoch_period_nonce_buffer
            .get(),
        settings
            .cryptarchia
            .epoch_config
            .epoch_period_nonce_stabilization
            .get(),
        slots_per_epoch,
    );

    Ok(())
}

fn diagnostic_error(message: impl Into<String>) -> StepError {
    StepError::LogicalError {
        message: message.into(),
    }
}

fn deployment_settings(
    world: &CucumberWorld,
    node_name: &str,
) -> Result<DeploymentSettings, StepError> {
    let node_info = world
        .nodes_info
        .get(node_name)
        .ok_or_else(|| diagnostic_error(format!("Node info for `{node_name}` is not available")))?;
    let path = node_info.runtime_dir.join("deployment.yaml");
    let contents = fs::read_to_string(&path).map_err(|error| {
        diagnostic_error(format!(
            "Failed to read deployment config `{}`: {error}",
            path.display()
        ))
    })?;
    serde_yaml::from_str(&contents).map_err(|error| {
        diagnostic_error(format!(
            "Failed to parse deployment config `{}`: {error}",
            path.display()
        ))
    })
}

#[must_use]
fn epoch_for_slot(settings: &DeploymentSettings, slot: u64) -> u32 {
    (slot / settings.cryptarchia.slots_per_epoch())
        .try_into()
        .unwrap_or(u32::MAX)
}

#[must_use]
fn boundary_slot(settings: &DeploymentSettings, epoch: u32) -> u64 {
    u64::from(epoch).saturating_mul(settings.cryptarchia.slots_per_epoch())
}

fn error_message(error: impl std::fmt::Display) -> StepError {
    StepError::StepFail {
        message: error.to_string(),
    }
}

struct EpochObservationConfig<'a> {
    phase: BlendDiagnosticPhase,
    node_name: &'a str,
    step: &'a str,
    start_epoch: u32,
    target_epoch: u32,
    poll_interval: Duration,
    timeout_secs: u64,
}

struct EpochTransitionLog<'a> {
    phase: BlendDiagnosticPhase,
    node_name: &'a str,
    previous_observed_epoch: u32,
    current_observed_epoch: u32,
    epochs_crossed: u32,
    expected_boundary_slot: u64,
    observed_slot: u64,
    boundary_overshoot_slots: u64,
    transition_index: u32,
    cryptarchia_info: &'a lb_chain_service::CryptarchiaInfo,
}

#[when(expr = "I observe {int} epoch transitions on node {string}")]
async fn observe_epoch_transitions_step(
    world: &mut CucumberWorld,
    step: &Step,
    transition_count: usize,
    node_name: String,
) -> StepResult {
    observe_epoch_transitions(world, &step.value, &node_name, transition_count).await
}

pub async fn observe_epoch_transitions(
    world: &mut CucumberWorld,
    step: &str,
    node_name: &str,
    transition_count: usize,
) -> StepResult {
    if transition_count == 0 {
        return Err(diagnostic_error(format!(
            "Step `{step}` requires at least one epoch transition"
        )));
    }

    if world.blend_diagnostics.phase.is_none() {
        world.blend_diagnostics.phase = Some(BlendDiagnosticPhase::Baseline);
    }
    let phase = world
        .blend_diagnostics
        .phase
        .expect("diagnostic phase was initialized above");
    let settings = deployment_settings(world, node_name)?;
    let client = world.resolve_node_http_client(node_name)?;
    let epoch_length = settings.cryptarchia.slots_per_epoch();
    let timeout_secs = observation_timeout_secs(&settings, transition_count);
    let poll_interval = observation_poll_interval(&settings);

    let initial_info = client.consensus_info().await.map_err(|error| {
        error_message(format!(
            "Step `{step}` could not query `{node_name}`: {error}"
        ))
    })?;
    let start_epoch = epoch_for_slot(&settings, initial_info.cryptarchia_info.slot.into());
    let target_epoch = start_epoch.saturating_add(
        u32::try_from(transition_count)
            .map_err(|_| diagnostic_error(format!("Step `{step}` requested too many epochs")))?,
    );
    info!(
        target: TARGET,
        diagnostic = DIAGNOSTIC,
        event = "epoch_observation_start",
        phase = phase.as_str(),
        node = node_name,
        start_epoch,
        target_epoch,
        start_slot = u64::from(initial_info.cryptarchia_info.slot),
        requested_transitions = transition_count,
        epoch_length_slots = epoch_length,
        timeout_secs,
        "Starting diagnostic epoch observation"
    );

    wait_for_epoch_transitions(
        world,
        &client,
        &settings,
        initial_info,
        EpochObservationConfig {
            phase,
            node_name,
            step,
            start_epoch,
            target_epoch,
            poll_interval,
            timeout_secs,
        },
    )
    .await
}

fn observation_timeout_secs(settings: &DeploymentSettings, transition_count: usize) -> u64 {
    settings
        .time
        .slot_duration
        .as_secs()
        .saturating_mul(settings.cryptarchia.slots_per_epoch())
        .max(1)
        .saturating_mul(
            u64::try_from(transition_count)
                .unwrap_or(u64::MAX)
                .saturating_add(2),
        )
}

fn observation_poll_interval(settings: &DeploymentSettings) -> Duration {
    Duration::from_millis(
        u64::try_from((settings.time.slot_duration.as_millis() / 4).clamp(100, 1_000))
            .unwrap_or(1_000),
    )
}

async fn wait_for_epoch_transitions(
    world: &mut CucumberWorld,
    client: &NodeHttpClient,
    settings: &DeploymentSettings,
    initial_info: ChainServiceInfo,
    observation: EpochObservationConfig<'_>,
) -> StepResult {
    let deadline = Instant::now() + Duration::from_secs(observation.timeout_secs);
    let start_epoch = observation.start_epoch;
    let mut current_observed_epoch = start_epoch;
    let mut last_slot = u64::from(initial_info.cryptarchia_info.slot);
    loop {
        if Instant::now() >= deadline {
            return Err(StepError::Timeout {
                message: format!(
                    "Step `{step}` timed out waiting for epoch {target_epoch} on `{node_name}`; start epoch={start_epoch}, last epoch={current_observed_epoch}, last slot={last_slot}",
                    step = observation.step,
                    node_name = observation.node_name,
                    target_epoch = observation.target_epoch,
                    start_epoch = start_epoch,
                    current_observed_epoch = current_observed_epoch,
                ),
            });
        }

        let Some(consensus) =
            query_consensus_for_observation(client, observation.phase, observation.node_name).await
        else {
            sleep(observation.poll_interval).await;
            continue;
        };
        let cryptarchia_info = consensus.cryptarchia_info;
        last_slot = u64::from(cryptarchia_info.slot);
        let epoch = epoch_for_slot(settings, cryptarchia_info.slot.into());
        if epoch <= current_observed_epoch {
            sleep(observation.poll_interval).await;
            continue;
        }

        let previous_observed_epoch = current_observed_epoch;
        current_observed_epoch = epoch;
        let epochs_crossed = current_observed_epoch.saturating_sub(previous_observed_epoch);
        let expected_boundary_epoch = previous_observed_epoch.saturating_add(1);
        let expected_boundary_slot = boundary_slot(settings, expected_boundary_epoch);
        let observed_slot = u64::from(cryptarchia_info.slot);
        let boundary_overshoot_slots = observed_slot.saturating_sub(expected_boundary_slot);
        let transition_index = current_observed_epoch.saturating_sub(start_epoch);
        log_epoch_transition(&EpochTransitionLog {
            phase: observation.phase,
            node_name: observation.node_name,
            previous_observed_epoch,
            current_observed_epoch,
            epochs_crossed,
            expected_boundary_slot,
            observed_slot,
            boundary_overshoot_slots,
            transition_index,
            cryptarchia_info: &cryptarchia_info,
        });
        if epochs_crossed > 1 {
            let missing_epochs: Vec<_> =
                (previous_observed_epoch.saturating_add(1)..current_observed_epoch).collect();
            warn!(
                target: TARGET,
                diagnostic = DIAGNOSTIC,
                event = "epoch_transition_gap",
                phase = observation.phase.as_str(),
                node = observation.node_name,
                previous_observed_epoch,
                current_observed_epoch,
                missing_epochs = ?missing_epochs,
                "Diagnostic epoch observation crossed epochs without an exact poll"
            );
        }
        if current_observed_epoch >= observation.target_epoch {
            world.blend_diagnostics.observation_count =
                world.blend_diagnostics.observation_count.saturating_add(1);
            return Ok(());
        }
        sleep(observation.poll_interval).await;
    }
}

async fn query_consensus_for_observation(
    client: &NodeHttpClient,
    phase: BlendDiagnosticPhase,
    node_name: &str,
) -> Option<ChainServiceInfo> {
    match client.consensus_info().await {
        Ok(consensus) => Some(consensus),
        Err(error) => {
            warn!(
                target: TARGET,
                diagnostic = DIAGNOSTIC,
                event = "epoch_observation_query_failure",
                phase = phase.as_str(),
                node = node_name,
                error = %error,
                "Diagnostic epoch observation query failed; retrying"
            );
            None
        }
    }
}

fn log_epoch_transition(observation: &EpochTransitionLog<'_>) {
    info!(
        target: TARGET,
        diagnostic = DIAGNOSTIC,
        event = "epoch_transition",
        phase = observation.phase.as_str(),
        node = observation.node_name,
        timestamp = %OffsetDateTime::now_utc(),
        previous_observed_epoch = observation.previous_observed_epoch,
        current_observed_epoch = observation.current_observed_epoch,
        epochs_crossed = observation.epochs_crossed,
        expected_boundary_slot = observation.expected_boundary_slot,
        observed_slot = observation.observed_slot,
        boundary_overshoot_slots = observation.boundary_overshoot_slots,
        epoch = observation.current_observed_epoch,
        boundary_slot = observation.expected_boundary_slot,
        slot = observation.observed_slot,
        tip_height = observation.cryptarchia_info.height,
        tip_id = %observation.cryptarchia_info.tip.encode_hex::<String>(),
        lib_slot = u64::from(observation.cryptarchia_info.lib_slot),
        lib_id = %observation.cryptarchia_info.lib.encode_hex::<String>(),
        transition_index = observation.transition_index,
        "Observed Cryptarchia epoch transition"
    );
}

#[given("I log diagnostic identities")]
#[when("I log diagnostic identities")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require a mutable world reference"
)]
fn log_diagnostic_identities_step(world: &mut CucumberWorld) -> StepResult {
    log_diagnostic_identities(world)
}

fn log_diagnostic_identities(world: &CucumberWorld) -> StepResult {
    let mut node_names: Vec<_> = world.nodes_info.keys().cloned().collect();
    node_names.sort();

    for node_name in node_names {
        let node_info = world
            .nodes_info
            .get(&node_name)
            .ok_or_else(|| diagnostic_error(format!("Node `{node_name}` is not available")))?;
        let user_config_path = node_info.runtime_dir.join(USER_CONFIG_FILE);
        let user_config = user_config_from_node_yaml(&user_config_path)?;
        let peer_id = world
            .cluster
            .node_peer_ids
            .get(&node_name)
            .copied()
            .unwrap_or(peer_id_from_node_yaml(&user_config_path)?);
        let blend_provider_id = user_config.blend_provider_id().map_err(|error| {
            diagnostic_error(format!(
                "Could not derive Blend provider identity for `{node_name}`: {error}"
            ))
        })?;

        info!(
            target: TARGET,
            diagnostic = DIAGNOSTIC,
            event = "diagnostic_identity",
            node = node_name,
            runtime_node = node_info.started_node.name.as_str(),
            peer_id = %peer_id,
            blend_provider_id = ?blend_provider_id,
            "Diagnostic node identity mapping"
        );
    }

    Ok(())
}

#[given("I log diagnostic outage summary")]
#[when("I log diagnostic outage summary")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require a mutable world reference"
)]
async fn log_diagnostic_outage_summary_step(world: &mut CucumberWorld) -> StepResult {
    log_majority_outage_summary(world).await;
    Ok(())
}

pub async fn log_node_lifecycle_marker(
    world: &CucumberWorld,
    event: &str,
    node_name: &str,
    stage: &str,
) {
    let Some(phase) = world.blend_diagnostics.phase else {
        return;
    };
    let Some((settings, consensus)) = lifecycle_reference_consensus(world, event, node_name).await
    else {
        return;
    };
    let slot = consensus.cryptarchia_info.slot;
    let epoch = epoch_for_slot(&settings, slot.into());
    info!(
        target: TARGET,
        diagnostic = DIAGNOSTIC,
        event,
        stage,
        node = node_name,
        reference_node = "NODE_2",
        phase = phase.as_str(),
        timestamp = %OffsetDateTime::now_utc(),
        epoch,
        slot = u64::from(slot),
        "Diagnostic node lifecycle marker"
    );
}

async fn lifecycle_reference_consensus(
    world: &CucumberWorld,
    event: &str,
    node_name: &str,
) -> Option<(DeploymentSettings, ChainServiceInfo)> {
    let Ok(settings) = deployment_settings(world, "NODE_2") else {
        warn!(target: TARGET, diagnostic = DIAGNOSTIC, event, node = node_name, "Could not load diagnostic deployment config for lifecycle marker");
        return None;
    };
    let Ok(client) = world.resolve_node_http_client("NODE_2") else {
        warn!(target: TARGET, diagnostic = DIAGNOSTIC, event, node = node_name, "Could not resolve diagnostic reference node for lifecycle marker");
        return None;
    };
    let Ok(consensus) = client.consensus_info().await else {
        warn!(target: TARGET, diagnostic = DIAGNOSTIC, event, node = node_name, "Could not query diagnostic reference node for lifecycle marker");
        return None;
    };
    Some((settings, consensus))
}

pub async fn log_majority_outage_summary(world: &CucumberWorld) {
    let Some(BlendDiagnosticPhase::Outage) = world.blend_diagnostics.phase else {
        return;
    };
    let mut stopped_nodes: Vec<_> = world
        .blend_diagnostics
        .stopped_nodes
        .iter()
        .cloned()
        .collect();
    stopped_nodes.sort_by(|left, right| {
        diagnostic_node_sort_number(left)
            .cmp(&diagnostic_node_sort_number(right))
            .then_with(|| left.cmp(right))
    });
    let Ok(settings) = deployment_settings(world, "NODE_2") else {
        return;
    };
    let Ok(client) = world.resolve_node_http_client("NODE_2") else {
        return;
    };
    let Ok(consensus) = client.consensus_info().await else {
        return;
    };
    let slot = consensus.cryptarchia_info.slot;
    let epoch = epoch_for_slot(&settings, slot.into());
    info!(
        target: TARGET,
        diagnostic = DIAGNOSTIC,
        event = "node_stop_summary",
        phase = "outage",
        reference_node = "NODE_2",
        timestamp = %OffsetDateTime::now_utc(),
        epoch,
        slot = u64::from(slot),
        stopped_nodes = ?stopped_nodes,
        "Diagnostic majority Blend provider outage"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_parameter_sets_apply_expected_values_and_derive_epoch_length() {
        for (name, security_parameter, phases) in [
            ("clean_control", 10, (1, 1, 1)),
            ("testnet_representative", 5, (3, 3, 4)),
            ("fast_repro", 3, (1, 1, 1)),
        ] {
            let parameter_set =
                BlendDiagnosticParameterSet::from_name(name).expect("profile should be known");
            let settings = parameter_set.effective_deployment_settings();

            assert_eq!(
                settings.cryptarchia.security_param.get(),
                security_parameter
            );
            assert_eq!(settings.time.slot_duration, Duration::from_secs(1));
            assert_eq!(
                (
                    settings
                        .cryptarchia
                        .epoch_config
                        .epoch_stake_distribution_stabilization
                        .get(),
                    settings
                        .cryptarchia
                        .epoch_config
                        .epoch_period_nonce_buffer
                        .get(),
                    settings
                        .cryptarchia
                        .epoch_config
                        .epoch_period_nonce_stabilization
                        .get(),
                ),
                phases
            );
            assert!(settings.cryptarchia.slots_per_epoch() > 0);
            let mut deployment_settings = DeploymentSettings::default();
            deployment_settings.cryptarchia.slot_activation_coeff =
                TopologyConfig::default().active_slot_coeff;
            parameter_set.apply_to(&mut deployment_settings);
            assert_eq!(
                settings.cryptarchia.slots_per_epoch(),
                deployment_settings.cryptarchia.slots_per_epoch()
            );
        }
    }

    #[test]
    fn unknown_named_parameter_set_is_rejected() {
        assert!(BlendDiagnosticParameterSet::from_name("accelerated").is_none());
        assert!(BlendDiagnosticParameterSet::from_name("unknown").is_none());
    }
}
