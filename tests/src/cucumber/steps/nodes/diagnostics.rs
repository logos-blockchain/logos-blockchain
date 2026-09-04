use std::{
    collections::{BTreeSet, HashSet},
    fs,
    io::Write as _,
    net::SocketAddr,
    num::{NonZero, NonZeroU64},
    time::Duration,
};

use cucumber::{gherkin::Step, given, when};
use futures::future::join_all;
use hex::ToHex as _;
use lb_chain_service::ChainServiceInfo;
use lb_core::sdp::{ProviderId, ServiceType};
use lb_http_api_common::TimeInfo;
use lb_node::config::DeploymentSettings;
use lb_testing_framework::{NodeHttpClient, USER_CONFIG_FILE, configs::deployment::TopologyConfig};
use time::OffsetDateTime;
use tokio::time::{Instant, sleep, timeout};
use tracing::{info, warn};

use crate::cucumber::{
    TARGET,
    error::{StepError, StepResult},
    steps::nodes::config_override::set_deployment_config_override,
    utils::{peer_id_from_node_yaml, user_config_from_node_yaml},
    world::{BlendDiagnosticPhase, CucumberWorld},
};

const DIAGNOSTIC: &str = "blend_tsi_outage";
const TIMELINE_FILE: &str = "blend_diagnostic_timeline.ndjson";
const DIAGNOSTIC_QUERY_TIMEOUT: Duration = Duration::from_millis(1_500);

fn append_timeline_record(world: &CucumberWorld, record: &serde_json::Value) {
    let path = world.lifecycle.scenario_base_dir.join(TIMELINE_FILE);
    let result = (|| -> std::io::Result<()> {
        let mut header_written = world
            .blend_diagnostics
            .timeline_header_written
            .lock()
            .map_err(|_| std::io::Error::other("timeline header lock poisoned"))?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        if !*header_written {
            if file.metadata()?.len() != 0 {
                file.write_all(b"\n")?;
            }
            let timestamp = OffsetDateTime::now_utc();
            let header = serde_json::json!({
                "event": "blend_diagnostic_timeline_header",
                "date": timestamp.date().to_string(),
                "time": timestamp.time().to_string(),
                "scenario": world
                    .lifecycle
                    .scenario_name
                    .as_deref()
                    .unwrap_or("<unknown>"),
            });
            serde_json::to_writer(&mut file, &header).map_err(std::io::Error::other)?;
            file.write_all(b"\n\n")?;
            *header_written = true;
        }
        serde_json::to_writer(&mut file, record).map_err(std::io::Error::other)?;
        file.write_all(b"\n")?;
        file.flush()?;
        drop(header_written);
        Ok(())
    })();

    if let Err(error) = result {
        warn!(
            target: TARGET,
            diagnostic = DIAGNOSTIC,
            event = "timeline_write_failure",
            path = %path.display(),
            error = %error,
            "Could not persist Blend diagnostic timeline record"
        );
    }
}

pub fn log_blend_relay_event(
    world: &CucumberWorld,
    event: &str,
    node_name: &str,
    declared_addr: SocketAddr,
    backend_addr: SocketAddr,
    phase: &str,
) {
    info!(
        target: TARGET,
        diagnostic = DIAGNOSTIC,
        event,
        node = node_name,
        declared_addr = %declared_addr,
        backend_addr = %backend_addr,
        phase,
        "Blend provider relay event"
    );
    append_timeline_record(
        world,
        &serde_json::json!({
            "event": event,
            "timestamp": OffsetDateTime::now_utc().to_string(),
            "node": node_name,
            "declared_addr": declared_addr.to_string(),
            "backend_addr": backend_addr.to_string(),
            "phase": phase,
        }),
    );
}

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DiagnosticGeometry {
    base_period_length: u64,
    slots_per_epoch: u64,
    snapshot_close_offset: u64,
    finalization_length: u64,
    finalization_midpoint_offset: u64,
    pre_boundary_offset: u64,
}

impl DiagnosticGeometry {
    fn from_settings(settings: &DeploymentSettings) -> Self {
        let epoch_config = settings.cryptarchia.epoch_config;
        let base_period_length = settings
            .cryptarchia
            .consensus_config()
            .base_period_length()
            .get();
        let snapshot_close_offset = base_period_length.saturating_mul(
            NonZeroU64::from(epoch_config.epoch_stake_distribution_stabilization)
                .get()
                .saturating_add(NonZeroU64::from(epoch_config.epoch_period_nonce_buffer).get()),
        );
        let finalization_length = base_period_length
            .saturating_mul(NonZeroU64::from(epoch_config.epoch_period_nonce_stabilization).get());
        let slots_per_epoch = settings.cryptarchia.slots_per_epoch();

        Self {
            base_period_length,
            slots_per_epoch,
            snapshot_close_offset,
            finalization_length,
            finalization_midpoint_offset: snapshot_close_offset
                .saturating_add(finalization_length / 2),
            pre_boundary_offset: slots_per_epoch.saturating_sub(1),
        }
    }

    fn checkpoint_slot(self, epoch: u32, checkpoint_kind: CheckpointKind) -> u64 {
        let epoch_start = boundary_slot_for_length(self.slots_per_epoch, epoch);
        epoch_start.saturating_add(match checkpoint_kind {
            CheckpointKind::SnapshotClose => self.snapshot_close_offset,
            CheckpointKind::FinalizationMidpoint => self.finalization_midpoint_offset,
            CheckpointKind::PreBoundary => self.pre_boundary_offset,
            CheckpointKind::EpochBoundary => 0,
        })
    }

    fn snapshot_close_slot(self, checkpoint_epoch: u32, checkpoint_kind: CheckpointKind) -> u64 {
        let snapshot_epoch = match checkpoint_kind {
            CheckpointKind::EpochBoundary => checkpoint_epoch,
            _ => checkpoint_epoch.saturating_add(1),
        };
        boundary_slot_for_length(self.slots_per_epoch, snapshot_epoch.saturating_sub(1))
            .saturating_add(self.snapshot_close_offset)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum CheckpointKind {
    SnapshotClose,
    FinalizationMidpoint,
    PreBoundary,
    EpochBoundary,
}

impl CheckpointKind {
    const NON_BOUNDARY: [Self; 3] = [
        Self::SnapshotClose,
        Self::FinalizationMidpoint,
        Self::PreBoundary,
    ];

    const fn as_str(self) -> &'static str {
        match self {
            Self::SnapshotClose => "snapshot_close",
            Self::FinalizationMidpoint => "finalization_midpoint",
            Self::PreBoundary => "pre_boundary",
            Self::EpochBoundary => "epoch_boundary",
        }
    }
}

fn boundary_slot_for_length(slots_per_epoch: u64, epoch: u32) -> u64 {
    u64::from(epoch).saturating_mul(slots_per_epoch)
}

#[expect(
    clippy::too_many_lines,
    reason = "Diagnostic setup keeps all named parameter overrides together"
)]
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
    let geometry = DiagnosticGeometry::from_settings(&settings);
    let slots_per_epoch = geometry.slots_per_epoch;
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
        base_period_length = geometry.base_period_length,
        snapshot_close_offset = geometry.snapshot_close_offset,
        finalization_length = geometry.finalization_length,
        finalization_midpoint_offset = geometry.finalization_midpoint_offset,
        pre_boundary_offset = geometry.pre_boundary_offset,
        slot_activation_coeff_numerator = settings.cryptarchia.slot_activation_coeff.numerator,
        slot_activation_coeff_denominator = settings
            .cryptarchia
            .slot_activation_coeff
            .denominator
            .get(),
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

    append_timeline_record(
        world,
        &serde_json::json!({
            "event": "blend_diagnostic_parameter_set",
            "timestamp": OffsetDateTime::now_utc().to_string(),
            "parameter_set": parameter_set.name,
            "security_parameter": settings.cryptarchia.security_param.get(),
            "slot_duration_secs": settings.time.slot_duration.as_secs(),
            "slot_duration_ms": settings.time.slot_duration.as_millis(),
            "epoch_stake_distribution_stabilization": settings
                .cryptarchia
                .epoch_config
                .epoch_stake_distribution_stabilization
                .get(),
            "epoch_period_nonce_buffer": settings
                .cryptarchia
                .epoch_config
                .epoch_period_nonce_buffer
                .get(),
            "epoch_period_nonce_stabilization": settings
                .cryptarchia
                .epoch_config
                .epoch_period_nonce_stabilization
                .get(),
            "slot_activation_coeff_numerator": settings
                .cryptarchia
                .slot_activation_coeff
                .numerator,
            "slot_activation_coeff_denominator": settings
                .cryptarchia
                .slot_activation_coeff
                .denominator
                .get(),
            "base_period_length": geometry.base_period_length,
            "slots_per_epoch": geometry.slots_per_epoch,
            "snapshot_close_offset": geometry.snapshot_close_offset,
            "finalization_length": geometry.finalization_length,
            "finalization_midpoint_offset": geometry.finalization_midpoint_offset,
            "pre_boundary_offset": geometry.pre_boundary_offset,
        }),
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
    chain_tip_epoch: u32,
    observation: &'a NodeDiagnosticObservation,
}

struct NodeDiagnosticObservation {
    time_info: TimeInfo,
    consensus: ChainServiceInfo,
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

#[expect(
    clippy::too_many_lines,
    reason = "Diagnostic setup and the first observation share one scenario step"
)]
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
    world.blend_diagnostics.reference_node = Some(node_name.to_owned());
    let settings = deployment_settings(world, node_name)?;
    let client = world.resolve_node_http_client(node_name)?;
    let epoch_length = settings.cryptarchia.slots_per_epoch();
    let geometry = DiagnosticGeometry::from_settings(&settings);
    let timeout_secs = observation_timeout_secs(&settings, transition_count);
    let poll_interval = observation_poll_interval(&settings);

    let initial_deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let initial_observation = loop {
        if Instant::now() >= initial_deadline {
            return Err(StepError::Timeout {
                message: format!(
                    "Step `{step}` timed out querying the Time service on `{node_name}` before observing epoch transitions"
                ),
            });
        }
        if let Some(observation) =
            query_observation_for_observation(&client, world, phase, node_name).await
        {
            break observation;
        }

        sleep(poll_interval).await;
    };
    let start_epoch = initial_observation.time_info.current_epoch;
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
        start_clock_epoch = initial_observation.time_info.current_epoch,
        start_clock_slot = initial_observation.time_info.current_slot,
        start_chain_tip_slot = u64::from(initial_observation.consensus.cryptarchia_info.slot),
        start_chain_tip_lag_slots = initial_observation
            .time_info
            .current_slot
            .saturating_sub(u64::from(initial_observation.consensus.cryptarchia_info.slot)),
        requested_transitions = transition_count,
        epoch_length_slots = epoch_length,
        snapshot_close_offset = geometry.snapshot_close_offset,
        finalization_length = geometry.finalization_length,
        timeout_secs,
        "Starting Time-service diagnostic epoch observation"
    );
    append_timeline_record(
        world,
        &serde_json::json!({
            "event": "epoch_observation_start",
            "timestamp": OffsetDateTime::now_utc().to_string(),
            "phase": phase.as_str(),
            "reference_node": node_name,
            "start_epoch": start_epoch,
            "target_epoch": target_epoch,
            "start_clock_epoch": initial_observation.time_info.current_epoch,
            "start_clock_slot": initial_observation.time_info.current_slot,
            "start_chain_tip_slot": initial_observation.consensus.cryptarchia_info.slot,
            "start_chain_tip_lag_slots": initial_observation
                .time_info
                .current_slot
                .saturating_sub(u64::from(initial_observation.consensus.cryptarchia_info.slot)),
            "requested_transitions": transition_count,
            "epoch_length_slots": epoch_length,
            "snapshot_close_offset": geometry.snapshot_close_offset,
            "finalization_length": geometry.finalization_length,
            "timeout_secs": timeout_secs,
        }),
    );

    wait_for_epoch_transitions(
        world,
        &client,
        &settings,
        initial_observation,
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

#[expect(
    clippy::too_many_lines,
    reason = "Diagnostic observation keeps transition logging and snapshot timing together"
)]
async fn wait_for_epoch_transitions(
    world: &mut CucumberWorld,
    client: &NodeHttpClient,
    settings: &DeploymentSettings,
    initial_observation: NodeDiagnosticObservation,
    observation: EpochObservationConfig<'_>,
) -> StepResult {
    let deadline = Instant::now() + Duration::from_secs(observation.timeout_secs);
    let start_epoch = observation.start_epoch;
    let mut current_observed_epoch = start_epoch;
    let mut last_clock_slot = initial_observation.time_info.current_slot;
    let geometry = DiagnosticGeometry::from_settings(settings);
    let mut completed_checkpoints = BTreeSet::new();
    loop {
        if Instant::now() >= deadline {
            return Err(StepError::Timeout {
                message: format!(
                    "Step `{step}` timed out waiting for epoch {target_epoch} on `{node_name}`; start epoch={start_epoch}, last epoch={current_observed_epoch}, last clock slot={last_clock_slot}",
                    step = observation.step,
                    node_name = observation.node_name,
                    target_epoch = observation.target_epoch,
                    start_epoch = start_epoch,
                    current_observed_epoch = current_observed_epoch,
                ),
            });
        }

        let Some(current_observation) = query_observation_for_observation(
            client,
            world,
            observation.phase,
            observation.node_name,
        )
        .await
        else {
            sleep(observation.poll_interval).await;
            continue;
        };

        last_clock_slot = current_observation.time_info.current_slot;
        let clock_epoch = current_observation.time_info.current_epoch;
        log_due_checkpoints(
            world,
            observation.phase,
            observation.node_name,
            settings,
            geometry,
            start_epoch,
            clock_epoch,
            last_clock_slot,
            &current_observation,
            &mut completed_checkpoints,
        )
        .await;

        if clock_epoch <= current_observed_epoch {
            sleep(observation.poll_interval).await;
            continue;
        }

        let previous_observed_epoch = current_observed_epoch;
        current_observed_epoch = clock_epoch;
        let epochs_crossed = current_observed_epoch.saturating_sub(previous_observed_epoch);
        let expected_boundary_epoch = previous_observed_epoch.saturating_add(1);
        let expected_boundary_slot =
            geometry.checkpoint_slot(expected_boundary_epoch, CheckpointKind::EpochBoundary);
        let observed_slot = current_observation.time_info.current_slot;
        let boundary_overshoot_slots = observed_slot.saturating_sub(expected_boundary_slot);
        let transition_index = current_observed_epoch.saturating_sub(start_epoch);
        let chain_tip_slot = u64::from(current_observation.consensus.cryptarchia_info.slot);
        let chain_tip_epoch = epoch_for_slot(settings, chain_tip_slot);
        append_timeline_record(
            world,
            &serde_json::json!({
                "event": "epoch_transition",
                "timestamp": OffsetDateTime::now_utc().to_string(),
                "phase": observation.phase.as_str(),
                "reference_node": observation.node_name,
                "previous_observed_epoch": previous_observed_epoch,
                "current_observed_epoch": current_observed_epoch,
                "clock_epoch": current_observed_epoch,
                "clock_slot": observed_slot,
                "epochs_crossed": epochs_crossed,
                "expected_boundary_slot": expected_boundary_slot,
                "boundary_overshoot_slots": boundary_overshoot_slots,
                "transition_index": transition_index,
                "chain_tip_epoch": chain_tip_epoch,
                "chain_tip_slot": chain_tip_slot,
                "chain_tip_lag_slots": observed_slot.saturating_sub(chain_tip_slot),
                "chain_tip_height": current_observation.consensus.cryptarchia_info.height,
                "chain_tip_id": current_observation
                    .consensus
                    .cryptarchia_info
                    .tip
                    .encode_hex::<String>(),
                "chain_lib_slot": u64::from(
                    current_observation.consensus.cryptarchia_info.lib_slot,
                ),
                "chain_lib_id": current_observation
                    .consensus
                    .cryptarchia_info
                    .lib
                    .encode_hex::<String>(),
                "chain_state": format!(
                    "{:?}",
                    current_observation.consensus.cryptarchia_info.state
                ),
            }),
        );
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
            chain_tip_epoch,
            observation: &current_observation,
        });
        for boundary_epoch in expected_boundary_epoch..=current_observed_epoch {
            let key = (boundary_epoch, CheckpointKind::EpochBoundary);
            if completed_checkpoints.insert(key) {
                log_epoch_checkpoint(
                    world,
                    observation.phase,
                    observation.node_name,
                    settings,
                    geometry,
                    boundary_epoch,
                    CheckpointKind::EpochBoundary,
                    &current_observation,
                )
                .await;
            }
        }
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

async fn query_node_observation(
    client: &NodeHttpClient,
) -> Result<NodeDiagnosticObservation, String> {
    let (time_info, consensus) = timeout(DIAGNOSTIC_QUERY_TIMEOUT, async {
        tokio::join!(client.time_info(), client.consensus_info())
    })
    .await
    .map_err(|_| {
        format!(
            "diagnostic query timed out after {}ms",
            DIAGNOSTIC_QUERY_TIMEOUT.as_millis()
        )
    })?;

    Ok(NodeDiagnosticObservation {
        time_info: time_info.map_err(|error| format!("time info query failed: {error}"))?,
        consensus: consensus.map_err(|error| format!("consensus info query failed: {error}"))?,
    })
}

async fn query_observation_for_observation(
    client: &NodeHttpClient,
    world: &CucumberWorld,
    phase: BlendDiagnosticPhase,
    node_name: &str,
) -> Option<NodeDiagnosticObservation> {
    match query_node_observation(client).await {
        Ok(observation) => Some(observation),
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
            append_timeline_record(
                world,
                &serde_json::json!({
                    "event": "epoch_observation_query_failure",
                    "timestamp": OffsetDateTime::now_utc().to_string(),
                    "phase": phase.as_str(),
                    "node": node_name,
                    "clock_epoch": serde_json::Value::Null,
                    "clock_slot": serde_json::Value::Null,
                    "error": error,
                }),
            );
            None
        }
    }
}

fn log_epoch_transition(observation: &EpochTransitionLog<'_>) {
    let info = &observation.observation.consensus.cryptarchia_info;
    let chain_tip_slot = u64::from(info.slot);
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
        clock_epoch = observation.observation.time_info.current_epoch,
        clock_slot = observation.observation.time_info.current_slot,
        chain_tip_epoch = observation.chain_tip_epoch,
        chain_tip_slot,
        chain_tip_lag_slots = observation
            .observation
            .time_info
            .current_slot
            .saturating_sub(chain_tip_slot),
        chain_tip_height = info.height,
        chain_tip_id = %info.tip.encode_hex::<String>(),
        chain_lib_slot = u64::from(info.lib_slot),
        chain_lib_id = %info.lib.encode_hex::<String>(),
        chain_state = ?info.state,
        transition_index = observation.transition_index,
                "Observed Time-service epoch transition"
    );
}

#[expect(
    clippy::too_many_arguments,
    reason = "Checkpoint scheduling keeps the clock, geometry, and node context explicit"
)]
async fn log_due_checkpoints(
    world: &CucumberWorld,
    phase: BlendDiagnosticPhase,
    reference_node: &str,
    settings: &DeploymentSettings,
    geometry: DiagnosticGeometry,
    start_epoch: u32,
    clock_epoch: u32,
    clock_slot: u64,
    reference_observation: &NodeDiagnosticObservation,
    completed_checkpoints: &mut BTreeSet<(u32, CheckpointKind)>,
) {
    for checkpoint_epoch in start_epoch..=clock_epoch {
        for checkpoint_kind in CheckpointKind::NON_BOUNDARY {
            let target_slot = geometry.checkpoint_slot(checkpoint_epoch, checkpoint_kind);
            if target_slot > clock_slot
                || !completed_checkpoints.insert((checkpoint_epoch, checkpoint_kind))
            {
                continue;
            }

            log_epoch_checkpoint(
                world,
                phase,
                reference_node,
                settings,
                geometry,
                checkpoint_epoch,
                checkpoint_kind,
                reference_observation,
            )
            .await;
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "Checkpoint persistence keeps all cross-node chain fields in one diagnostic record"
)]
async fn log_epoch_checkpoint(
    world: &CucumberWorld,
    phase: BlendDiagnosticPhase,
    reference_node: &str,
    settings: &DeploymentSettings,
    geometry: DiagnosticGeometry,
    checkpoint_epoch: u32,
    checkpoint_kind: CheckpointKind,
    reference_observation: &NodeDiagnosticObservation,
) {
    let checkpoint_slot = geometry.checkpoint_slot(checkpoint_epoch, checkpoint_kind);
    let snapshot_close_slot = geometry.snapshot_close_slot(checkpoint_epoch, checkpoint_kind);
    let requested_epoch = match checkpoint_kind {
        CheckpointKind::EpochBoundary => checkpoint_epoch,
        _ => checkpoint_epoch.saturating_add(1),
    };
    let reference_clock_epoch = reference_observation.time_info.current_epoch;
    let reference_clock_slot = reference_observation.time_info.current_slot;

    let mut nodes = world
        .nodes_info
        .iter()
        .filter(|(node_name, _)| !world.blend_diagnostics.stopped_nodes.contains(*node_name))
        .map(|(node_name, node_info)| (node_name.clone(), node_info.started_node.client.clone()))
        .collect::<Vec<_>>();
    nodes.sort_by(|left, right| left.0.cmp(&right.0));

    let snapshots = join_all(
        nodes
            .into_iter()
            .map(async |(node_name, client)| (node_name, query_node_observation(&client).await)),
    )
    .await;

    for (node_name, result) in snapshots {
        match result {
            Ok(observation) => {
                let info = observation.consensus.cryptarchia_info;
                let chain_tip_slot = u64::from(info.slot);
                let chain_tip_lag_slots = observation
                    .time_info
                    .current_slot
                    .saturating_sub(chain_tip_slot);
                let chain_tip_epoch = epoch_for_slot(settings, chain_tip_slot);
                let source_tip_before_snapshot_close = chain_tip_slot < snapshot_close_slot;
                info!(
                    target: TARGET,
                    diagnostic = DIAGNOSTIC,
                    event = "epoch_chain_checkpoint",
                    phase = phase.as_str(),
                    reference_node,
                    checkpoint_kind = checkpoint_kind.as_str(),
                    checkpoint_epoch,
                    checkpoint_slot,
                    requested_epoch,
                    snapshot_close_slot,
                    reference_clock_epoch,
                    reference_clock_slot,
                    node = node_name,
                    clock_epoch = observation.time_info.current_epoch,
                    clock_slot = observation.time_info.current_slot,
                    chain_tip_epoch,
                    chain_tip_slot,
                    chain_tip_lag_slots,
                    chain_tip_height = info.height,
                    chain_tip_id = %info.tip.encode_hex::<String>(),
                    chain_lib_slot = u64::from(info.lib_slot),
                    chain_lib_id = %info.lib.encode_hex::<String>(),
                    chain_state = ?info.state,
                    source_tip_before_snapshot_close,
                    timestamp = %OffsetDateTime::now_utc(),
                    "Near-simultaneous chain snapshot at diagnostic checkpoint"
                );
                append_timeline_record(
                    world,
                    &serde_json::json!({
                        "event": "epoch_chain_checkpoint",
                        "timestamp": OffsetDateTime::now_utc().to_string(),
                        "phase": phase.as_str(),
                        "reference_node": reference_node,
                        "checkpoint_kind": checkpoint_kind.as_str(),
                        "checkpoint_epoch": checkpoint_epoch,
                        "checkpoint_slot": checkpoint_slot,
                        "requested_epoch": requested_epoch,
                        "snapshot_close_slot": snapshot_close_slot,
                        "reference_clock_epoch": reference_clock_epoch,
                        "reference_clock_slot": reference_clock_slot,
                        "node": node_name,
                        "clock_epoch": observation.time_info.current_epoch,
                        "clock_slot": observation.time_info.current_slot,
                        "slot_duration_ms": observation.time_info.slot_duration_ms,
                        "genesis_time_unix_ms": observation.time_info.genesis_time_unix_ms,
                        "chain_tip_epoch": chain_tip_epoch,
                        "chain_tip_slot": chain_tip_slot,
                        "chain_tip_lag_slots": chain_tip_lag_slots,
                        "chain_tip_height": info.height,
                        "chain_tip_id": info.tip.encode_hex::<String>(),
                        "chain_lib_slot": u64::from(info.lib_slot),
                        "chain_lib_id": info.lib.encode_hex::<String>(),
                        "chain_state": format!("{:?}", info.state),
                        "source_tip_before_snapshot_close": source_tip_before_snapshot_close,
                    }),
                );
            }
            Err(error) => {
                warn!(
                    target: TARGET,
                    diagnostic = DIAGNOSTIC,
                    event = "epoch_chain_checkpoint_query_failure",
                    phase = phase.as_str(),
                    reference_node,
                    checkpoint_kind = checkpoint_kind.as_str(),
                    checkpoint_epoch,
                    checkpoint_slot,
                    requested_epoch,
                    reference_clock_epoch,
                    reference_clock_slot,
                    node = node_name,
                    error = %error,
                    "Could not query running node for diagnostic epoch checkpoint"
                );
                append_timeline_record(
                    world,
                    &serde_json::json!({
                        "event": "epoch_chain_checkpoint_query_failure",
                        "timestamp": OffsetDateTime::now_utc().to_string(),
                        "phase": phase.as_str(),
                        "reference_node": reference_node,
                        "checkpoint_kind": checkpoint_kind.as_str(),
                        "checkpoint_epoch": checkpoint_epoch,
                        "checkpoint_slot": checkpoint_slot,
                        "requested_epoch": requested_epoch,
                        "reference_clock_epoch": reference_clock_epoch,
                        "reference_clock_slot": reference_clock_slot,
                        "node": node_name,
                        "error": error,
                    }),
                );
            }
        }
    }
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
    let expected_provider_count = world.cluster.blend_core_nodes.unwrap_or(node_names.len());
    let expected_provider_names = (1..=expected_provider_count)
        .map(|number| format!("NODE_{number}"))
        .collect::<BTreeSet<_>>();
    let reference_node = node_names
        .first()
        .ok_or_else(|| diagnostic_error("No running nodes are available for identity mapping"))?;
    let deployment = deployment_settings(world, reference_node)?;
    let declared_provider_ids = deployment
        .cryptarchia
        .genesis_block
        .genesis_tx()
        .sdp_declarations()
        .filter(|declaration| declaration.operation().service_type == ServiceType::BlendNetwork)
        .map(|declaration| declaration.operation().provider_id)
        .collect::<HashSet<ProviderId>>();
    let mut actual_provider_names = BTreeSet::new();

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
        let actual_provider = declared_provider_ids.contains(&blend_provider_id);
        if actual_provider {
            actual_provider_names.insert(node_name.clone());
        }

        info!(
            target: TARGET,
            diagnostic = DIAGNOSTIC,
            event = "diagnostic_identity",
            node = node_name,
            runtime_node = node_info.started_node.name.as_str(),
            peer_id = %peer_id,
            blend_provider_id = ?blend_provider_id,
            expected_provider = expected_provider_names.contains(&node_name),
            actual_provider,
            "Diagnostic node identity mapping"
        );
        append_timeline_record(
            world,
            &serde_json::json!({
                "event": "diagnostic_identity",
                "timestamp": OffsetDateTime::now_utc().to_string(),
                "node": node_name,
                "runtime_node": node_info.started_node.name,
                "peer_id": peer_id.to_string(),
                "blend_provider_id": format!("{blend_provider_id:?}"),
                "expected_provider": expected_provider_names.contains(&node_name),
                "actual_provider": actual_provider,
            }),
        );
    }

    append_timeline_record(
        world,
        &serde_json::json!({
            "event": "diagnostic_provider_mapping",
            "timestamp": OffsetDateTime::now_utc().to_string(),
            "expected_provider_nodes": expected_provider_names,
            "actual_provider_nodes": actual_provider_names,
        }),
    );

    if expected_provider_names != actual_provider_names {
        return Err(diagnostic_error(format!(
            "Blend provider mapping mismatch: expected {expected_provider_names:?}, actual {actual_provider_names:?}"
        )));
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
    let time_info = lifecycle_reference_time(world, event, node_name)
        .await
        .map(|(_, time_info)| time_info);
    let reference_node = world.blend_diagnostics.reference_node.as_deref();
    let clock_epoch = time_info.as_ref().map(|time_info| time_info.current_epoch);
    let clock_slot = time_info.as_ref().map(|time_info| time_info.current_slot);
    let timestamp = OffsetDateTime::now_utc();
    info!(
        target: TARGET,
        diagnostic = DIAGNOSTIC,
        event,
        stage,
        node = node_name,
        reference_node = ?reference_node,
        phase = phase.as_str(),
        timestamp = %timestamp,
        clock_epoch = ?clock_epoch,
        clock_slot = ?clock_slot,
        "Diagnostic node lifecycle marker"
    );
    append_timeline_record(
        world,
        &serde_json::json!({
            "event": event,
            "stage": stage,
            "node": node_name,
            "reference_node": reference_node,
            "phase": phase.as_str(),
            "timestamp": timestamp.to_string(),
            "clock_epoch": clock_epoch,
            "clock_slot": clock_slot,
        }),
    );
}

async fn lifecycle_reference_time<'a>(
    world: &'a CucumberWorld,
    event: &str,
    node_name: &str,
) -> Option<(&'a str, TimeInfo)> {
    let Some(reference_node) = world.blend_diagnostics.reference_node.as_deref() else {
        warn!(
            target: TARGET,
            diagnostic = DIAGNOSTIC,
            event,
            node = node_name,
            "Could not resolve diagnostic reference node for lifecycle marker"
        );
        return None;
    };
    let Ok(client) = world.resolve_node_http_client(reference_node) else {
        warn!(
            target: TARGET,
            diagnostic = DIAGNOSTIC,
            event,
            node = node_name,
            reference_node,
            "Could not resolve diagnostic reference node for lifecycle marker"
        );
        log_reference_query_failure(
            world,
            event,
            node_name,
            reference_node,
            "could not resolve diagnostic reference node",
        );
        return None;
    };
    match timeout(DIAGNOSTIC_QUERY_TIMEOUT, client.time_info()).await {
        Ok(Ok(time_info)) => Some((reference_node, time_info)),
        Ok(Err(error)) => {
            let error = error.to_string();
            log_reference_query_failure(world, event, node_name, reference_node, &error);
            None
        }
        Err(_) => {
            let error = format!(
                "diagnostic time query timed out after {}ms",
                DIAGNOSTIC_QUERY_TIMEOUT.as_millis()
            );
            log_reference_query_failure(world, event, node_name, reference_node, &error);
            None
        }
    }
}

fn log_reference_query_failure(
    world: &CucumberWorld,
    event: &str,
    node_name: &str,
    reference_node: &str,
    error: &str,
) {
    warn!(
        target: TARGET,
        diagnostic = DIAGNOSTIC,
        event = "epoch_observation_query_failure",
        observation_event = event,
        node = node_name,
        reference_node,
        error = %error,
        "Could not query diagnostic reference clock"
    );
    append_timeline_record(
        world,
        &serde_json::json!({
            "event": "epoch_observation_query_failure",
            "observation_event": event,
            "timestamp": OffsetDateTime::now_utc().to_string(),
            "node": node_name,
            "reference_node": reference_node,
            "error": error,
        }),
    );
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
    let mut blend_unreachable_nodes: Vec<_> = world
        .blend_diagnostics
        .blend_unreachable_nodes
        .iter()
        .cloned()
        .collect();
    blend_unreachable_nodes.sort_by(|left, right| {
        diagnostic_node_sort_number(left)
            .cmp(&diagnostic_node_sort_number(right))
            .then_with(|| left.cmp(right))
    });
    let reference_node = world.blend_diagnostics.reference_node.as_deref();
    let time_info = if let Some(reference_node) = reference_node {
        lifecycle_reference_time(world, "node_stop_summary", reference_node)
            .await
            .map(|(_, time_info)| time_info)
    } else {
        None
    };
    let clock_epoch = time_info.as_ref().map(|time_info| time_info.current_epoch);
    let clock_slot = time_info.as_ref().map(|time_info| time_info.current_slot);
    let timestamp = OffsetDateTime::now_utc();
    info!(
        target: TARGET,
        diagnostic = DIAGNOSTIC,
        event = "node_stop_summary",
        phase = "outage",
        reference_node = ?reference_node,
        timestamp = %timestamp,
        clock_epoch = ?clock_epoch,
        clock_slot = ?clock_slot,
        stopped_nodes = ?stopped_nodes,
        blend_unreachable_nodes = ?blend_unreachable_nodes,
        "Diagnostic majority Blend provider outage"
    );
    append_timeline_record(
        world,
        &serde_json::json!({
            "event": "node_stop_summary",
            "phase": "outage",
            "reference_node": reference_node,
            "timestamp": timestamp.to_string(),
            "clock_epoch": clock_epoch,
            "clock_slot": clock_slot,
            "stopped_nodes": stopped_nodes,
            "blend_unreachable_nodes": blend_unreachable_nodes,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_parameter_sets_apply_expected_values_and_derive_epoch_length() {
        for (name, security_parameter, phases, expected_geometry) in [
            (
                "clean_control",
                10,
                (1, 1, 1),
                (100, 300, 200, 100, 250, 299),
            ),
            (
                "testnet_representative",
                5,
                (3, 3, 4),
                (50, 500, 300, 200, 400, 499),
            ),
            ("fast_repro", 3, (1, 1, 1), (30, 90, 60, 30, 75, 89)),
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
            let geometry = DiagnosticGeometry::from_settings(&settings);
            assert_eq!(
                (
                    geometry.base_period_length,
                    geometry.slots_per_epoch,
                    geometry.snapshot_close_offset,
                    geometry.finalization_length,
                    geometry.finalization_midpoint_offset,
                    geometry.pre_boundary_offset,
                ),
                expected_geometry
            );
            assert_eq!(
                geometry.checkpoint_slot(4, CheckpointKind::SnapshotClose),
                4 * expected_geometry.1 + expected_geometry.2
            );
            assert_eq!(
                geometry.checkpoint_slot(4, CheckpointKind::FinalizationMidpoint),
                4 * expected_geometry.1 + expected_geometry.4
            );
            assert_eq!(
                geometry.checkpoint_slot(4, CheckpointKind::PreBoundary),
                4 * expected_geometry.1 + expected_geometry.5
            );
            assert_eq!(
                geometry.checkpoint_slot(4, CheckpointKind::EpochBoundary),
                4 * expected_geometry.1
            );
            assert_eq!(
                geometry.snapshot_close_slot(4, CheckpointKind::EpochBoundary),
                3 * expected_geometry.1 + expected_geometry.2
            );
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

    #[test]
    fn timeline_has_header_and_blank_lines_between_records() {
        let temp_dir = tempfile::tempdir().expect("temporary directory should be created");
        let mut world = CucumberWorld::default();
        world.lifecycle.scenario_base_dir = temp_dir.path().to_owned();
        world.set_scenario_name("timeline header scenario");

        append_timeline_record(&world, &serde_json::json!({"event": "first"}));
        append_timeline_record(&world, &serde_json::json!({"event": "second"}));

        let timeline = fs::read_to_string(temp_dir.path().join(TIMELINE_FILE))
            .expect("timeline should be readable");
        let lines: Vec<_> = timeline.lines().collect();
        assert_eq!(lines[1], "");
        assert_eq!(lines[3], "{\"event\":\"second\"}");

        let header: serde_json::Value =
            serde_json::from_str(lines[0]).expect("header should be valid JSON");
        assert_eq!(header["event"], "blend_diagnostic_timeline_header");
        assert_eq!(header["scenario"], "timeline header scenario");
        assert!(header["date"].is_string());
        assert!(header["time"].is_string());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(lines[2])
                .expect("first record should be valid JSON")["event"],
            "first"
        );
        let mut next_run = CucumberWorld::default();
        next_run.lifecycle.scenario_base_dir = temp_dir.path().to_owned();
        next_run.set_scenario_name("next timeline run");
        append_timeline_record(&next_run, &serde_json::json!({"event": "third"}));

        let timeline = fs::read_to_string(temp_dir.path().join(TIMELINE_FILE))
            .expect("timeline should be readable");
        let lines: Vec<_> = timeline.lines().collect();
        assert_eq!(lines[4], "");
        let next_header: serde_json::Value =
            serde_json::from_str(lines[5]).expect("next header should be valid JSON");
        assert_eq!(next_header["scenario"], "next timeline run");
        assert_eq!(lines[6], "");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(lines[7])
                .expect("third record should be valid JSON")["event"],
            "third"
        );
    }
}
