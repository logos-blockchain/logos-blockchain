use std::{
    collections::{BTreeMap, HashMap, HashSet},
    error::Error,
    num::{NonZero, NonZeroU32},
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

use lb_chain_service::Epoch;
pub use lb_config::consensus::SdpFundingConfig;
use lb_config::preparation::prepare_network;
use lb_core::{block::genesis::GenesisBlock, mantle::GenesisTime};
use lb_node::config::{
    RunConfig,
    deployment::{DeploymentSettings, EraDeclarations, era::ruleset::EraRuleset},
};
use lb_utils::math::NonNegativeRatio;
use rand::{Rng, SeedableRng as _};
use testing_framework_core::topology::{DeploymentProvider, DeploymentSeed, DynTopologyError};
use thiserror::Error;

use super::{
    Libp2pNetworkLayout, NetworkParams,
    wallet::{WalletConfig, WalletConfigError},
};
use crate::{DeploymentPlan, config::PreparedDeployment, get_reserved_available_udp_port};

pub type DynError = Box<dyn Error + Send + Sync + 'static>;
const DEFAULT_SLOT_TIME_IN_SECS: u64 = 1;
const DEFAULT_ACTIVE_SLOT_COEFF: NonNegativeRatio =
    NonNegativeRatio::new(1, NonZeroU32::new(10).unwrap());
const DEFAULT_SECURITY_PARAM: NonZeroU32 = NonZeroU32::new(20).unwrap();

fn constrain_default_uncle_reference_window(
    cryptarchia: &mut lb_node::config::deployment::era::ruleset::v1::cryptarchia::Settings,
) {
    cryptarchia.uncle_reference_window_in_block = cryptarchia
        .uncle_reference_window_in_block
        .min(cryptarchia.widest_uncle_reference_window());
}

static RESERVED_AUTOMATIC_GENESIS_TIMES: LazyLock<Mutex<HashSet<GenesisTime>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Reserves a near-current genesis second for a new automatically configured
/// deployment in this process.
#[must_use]
pub fn resolve_automatic_genesis_time() -> GenesisTime {
    let requested = time::OffsetDateTime::now_utc()
        .try_into()
        .expect("current time should fit in GenesisTime");
    let mut reserved = RESERVED_AUTOMATIC_GENESIS_TIMES
        .lock()
        .expect("automatic genesis time reservation lock should not be poisoned");

    let mut candidate = requested;
    while !reserved.insert(candidate) {
        let next = time::OffsetDateTime::from(candidate) + time::Duration::seconds(1);
        candidate = next
            .try_into()
            .expect("automatic genesis time should fit in GenesisTime");
    }

    candidate
}

#[derive(Debug, Error)]
pub enum TopologyBuildError {
    #[error("internal config vector mismatch for {label} (expected {expected}, got {actual})")]
    VectorLenMismatch {
        label: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("failed to allocate blend UDP ports for topology")]
    BlendPortAllocation,
    #[error(transparent)]
    InvalidWallet(#[from] WalletConfigError),
}

/// Shared Cryptarchia test parameters used by Cucumber integration scenarios.
///
/// Uncle-reference windows are deliberately derived from the normal deployment
/// defaults when this profile is applied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CucumberParameterSet {
    name: &'static str,
    security_parameter: u32,
    slot_duration_secs: u64,
    epoch_stake_distribution_stabilization: u8,
    epoch_period_nonce_buffer: u8,
    epoch_period_nonce_stabilization: u8,
    slot_activation_coeff_numerator: u32,
    slot_activation_coeff_denominator: u32,
}

impl CucumberParameterSet {
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "cucumber_fixture" => Some(Self {
                name: "cucumber_fixture",
                security_parameter: 8,
                slot_duration_secs: 1,
                epoch_stake_distribution_stabilization: 3,
                epoch_period_nonce_buffer: 3,
                epoch_period_nonce_stabilization: 4,
                slot_activation_coeff_numerator: 1,
                slot_activation_coeff_denominator: 2,
            }),
            "fast_repro" => Some(Self {
                name: "fast_repro",
                security_parameter: 3,
                slot_duration_secs: 1,
                epoch_stake_distribution_stabilization: 1,
                epoch_period_nonce_buffer: 1,
                epoch_period_nonce_stabilization: 1,
                slot_activation_coeff_numerator: 1,
                slot_activation_coeff_denominator: 10,
            }),
            "testnet_representative" => Some(Self {
                name: "testnet_representative",
                security_parameter: 10,
                slot_duration_secs: 1,
                epoch_stake_distribution_stabilization: 3,
                epoch_period_nonce_buffer: 3,
                epoch_period_nonce_stabilization: 4,
                slot_activation_coeff_numerator: 1,
                slot_activation_coeff_denominator: 30,
            }),
            _ => None,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    #[must_use]
    pub const fn security_param(self) -> NonZeroU32 {
        NonZeroU32::new(self.security_parameter)
            .expect("named Cucumber security parameter must be non-zero")
    }

    #[must_use]
    pub const fn slot_duration(self) -> Duration {
        Duration::from_secs(self.slot_duration_secs)
    }

    #[must_use]
    pub const fn phases(self) -> (u8, u8, u8) {
        (
            self.epoch_stake_distribution_stabilization,
            self.epoch_period_nonce_buffer,
            self.epoch_period_nonce_stabilization,
        )
    }

    #[must_use]
    pub const fn slot_activation_coeff(self) -> NonNegativeRatio {
        NonNegativeRatio::new(
            self.slot_activation_coeff_numerator,
            NonZeroU32::new(self.slot_activation_coeff_denominator)
                .expect("named Cucumber coefficient denominator must be non-zero"),
        )
    }

    /// Applies the profile to every deployment era before an era schedule is
    /// constructed, deriving the default uncle window from the configured `k`.
    pub fn apply_to(self, settings: &mut DeploymentSettings) {
        settings
            .update_genesis_era_parameters(|parameters| {
                let cryptarchia = &mut parameters.cryptarchia;
                cryptarchia.security_param = self.security_param();
                cryptarchia.slot_activation_coeff = self
                    .slot_activation_coeff()
                    .try_into()
                    .expect("named Cucumber coefficient must be valid");
                let epoch_config = &mut cryptarchia.epoch_config;
                epoch_config.epoch_stake_distribution_stabilization =
                    NonZero::new(self.epoch_stake_distribution_stabilization)
                        .expect("named Cucumber phase must be non-zero");
                epoch_config.epoch_period_nonce_buffer =
                    NonZero::new(self.epoch_period_nonce_buffer)
                        .expect("named Cucumber phase must be non-zero");
                epoch_config.epoch_period_nonce_stabilization =
                    NonZero::new(self.epoch_period_nonce_stabilization)
                        .expect("named Cucumber phase must be non-zero");
                parameters.time.slot_duration_in_seconds = NonZero::new(self.slot_duration_secs)
                    .expect("named Cucumber slot duration must be non-zero");
                constrain_default_uncle_reference_window(cryptarchia);
            })
            .expect("The selected Cucumber parameter set must be valid.");
    }

    #[must_use]
    pub fn effective_deployment_settings(self) -> DeploymentSettings {
        let mut settings = DeploymentSettings::default();
        self.apply_to(&mut settings);
        settings
    }
}

/// Consecutive era transition used by the real-node happy-path fixture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EraBoundarySchedule {
    previous_era: u32,
    target_era: u32,
    activation_epoch: u32,
}

impl EraBoundarySchedule {
    pub fn new(
        previous_era: u32,
        target_era: u32,
        activation_epoch: u32,
    ) -> Result<Self, &'static str> {
        if previous_era.checked_add(1) != Some(target_era) {
            return Err("The target era must immediately follow the previous era");
        }
        if activation_epoch <= previous_era {
            return Err("The activation epoch must leave room for all preceding era declarations");
        }
        Ok(Self {
            previous_era,
            target_era,
            activation_epoch,
        })
    }

    #[must_use]
    pub const fn previous_era(self) -> u32 {
        self.previous_era
    }

    #[must_use]
    pub const fn target_era(self) -> u32 {
        self.target_era
    }

    #[must_use]
    pub const fn activation_epoch(self) -> u32 {
        self.activation_epoch
    }
}

/// Defines the profile of the node binary to be used in the deployment.
#[derive(Debug, Default, Clone, Eq, PartialEq)]
pub enum NodeBinaryProfile {
    #[default]
    Normal,
    TokioConsole,
}

/// Environment variable name for the node binary profile.
pub const NODE_BINARY_PROFILE: &str = "NODE_BINARY_PROFILE";
///  Environment variable value for the normal node binary profile.
const NODE_BINARY_PROFILE_NORMAL: &str = "normal";
/// Environment variable value for the tokio-console node binary profile.
const NODE_BINARY_PROFILE_TOKIO_CONSOLE: &str = "tokio-console";

impl NodeBinaryProfile {
    #[must_use]
    pub fn from_string(s: &str) -> Self {
        match s {
            NODE_BINARY_PROFILE_TOKIO_CONSOLE => Self::TokioConsole,
            _ => Self::Normal,
        }
    }

    #[must_use]
    pub const fn to_string(&self) -> &'static str {
        match self {
            Self::Normal => NODE_BINARY_PROFILE_NORMAL,
            Self::TokioConsole => NODE_BINARY_PROFILE_TOKIO_CONSOLE,
        }
    }
}

/// High-level topology settings used to generate node configs for a scenario.
#[derive(Clone)]
pub struct TopologyConfig {
    pub n_nodes: usize,
    pub blend_core_nodes: usize,
    pub sdp_funding_config: SdpFundingConfig,
    pub network_params: Arc<NetworkParams>,
    pub wallet_config: WalletConfig,
    pub scenario_base_dir: PathBuf,
    pub genesis_block: Option<GenesisBlock>,
    requested_genesis_time: Option<GenesisTime>,
    genesis_time: Option<GenesisTime>,
    pub slot_duration: Option<Duration>,
    pub active_slot_coeff: NonNegativeRatio,
    pub security_param: NonZeroU32,
    cucumber_parameter_set: Option<CucumberParameterSet>,
    era_boundary_schedule: Option<EraBoundarySchedule>,
    node_config_overrides: HashMap<usize, RunConfig>,
    allow_multiple_genesis_tokens: bool,
    allow_zero_value_genesis_tokens: bool,
    pub test_context: Option<String>,
    pub(super) node_binary_profile: NodeBinaryProfile,
}

impl TopologyConfig {
    fn with_node_count(nodes: usize) -> Self {
        Self {
            n_nodes: nodes,
            blend_core_nodes: nodes,
            ..Self::default()
        }
    }

    #[must_use]
    pub const fn with_allow_multiple_genesis_tokens(mut self, allow_multiple: bool) -> Self {
        self.allow_multiple_genesis_tokens = allow_multiple;
        self
    }

    #[must_use]
    pub const fn with_allow_zero_value_genesis_tokens(mut self, allow_multiple: bool) -> Self {
        self.allow_zero_value_genesis_tokens = allow_multiple;
        self
    }

    #[must_use]
    pub fn with_test_context(mut self, test_context: Option<String>) -> Self {
        self.test_context = test_context;
        self
    }

    #[must_use]
    pub const fn with_genesis_time(mut self, genesis_time: GenesisTime) -> Self {
        self.requested_genesis_time = Some(genesis_time);
        self
    }

    /// Returns the genesis time resolved while building this deployment.
    #[must_use]
    pub const fn genesis_time(&self) -> GenesisTime {
        self.genesis_time
            .expect("genesis time is available only on a built deployment")
    }

    #[must_use]
    pub const fn with_node_binary_profile(
        mut self,
        node_binary_profile: NodeBinaryProfile,
    ) -> Self {
        self.node_binary_profile = node_binary_profile;
        self
    }

    #[must_use]
    pub fn empty() -> Self {
        Self::with_node_count(0)
    }

    #[must_use]
    pub fn with_node_numbers(nodes: usize) -> Self {
        Self::with_node_count(nodes)
    }

    #[must_use]
    pub const fn with_blend_core_nodes(mut self, blend_core_nodes: usize) -> Self {
        self.blend_core_nodes = blend_core_nodes;
        self
    }

    #[must_use]
    pub const fn with_sdp_funding_config(mut self, sdp_funding_config: SdpFundingConfig) -> Self {
        self.sdp_funding_config = sdp_funding_config;
        self
    }

    #[must_use]
    pub fn node_config_override(&self, index: usize) -> Option<&RunConfig> {
        self.node_config_overrides.get(&index)
    }

    /// Configures a shared Cucumber test-parameter profile.
    #[must_use]
    pub const fn with_parameter_set(mut self, parameter_set: CucumberParameterSet) -> Self {
        self.security_param = parameter_set.security_param();
        self.active_slot_coeff = parameter_set.slot_activation_coeff();
        self.slot_duration = Some(parameter_set.slot_duration());
        self.cucumber_parameter_set = Some(parameter_set);
        self
    }

    /// Adds a consecutive-era schedule. Every declared era uses the effective
    /// deployment ruleset selected independently by the topology configuration.
    #[must_use]
    pub const fn with_era_boundary_schedule(mut self, schedule: EraBoundarySchedule) -> Self {
        self.era_boundary_schedule = Some(schedule);
        self
    }

    pub(crate) fn apply_deployment_overrides(&self, settings: &mut DeploymentSettings) {
        if let Some(parameter_set) = self.cucumber_parameter_set {
            parameter_set.apply_to(settings);
        } else {
            settings
                .update_genesis_era_parameters(|parameters| {
                    let cryptarchia = &mut parameters.cryptarchia;
                    cryptarchia.security_param = self.security_param;
                    cryptarchia.slot_activation_coeff = self
                        .active_slot_coeff
                        .try_into()
                        .expect("The topology's slot activation coefficient must be valid.");
                    constrain_default_uncle_reference_window(cryptarchia);
                })
                .expect("The changed deployment must be valid.");
        }
        if let Some(schedule) = self.era_boundary_schedule {
            let ruleset = EraRuleset::V1(settings.genesis_era_parameters().clone());
            let declarations = (0..=schedule.target_era)
                .map(|era| {
                    let first_epoch = if era == schedule.target_era {
                        schedule.activation_epoch
                    } else {
                        era
                    };
                    (Epoch::new(first_epoch), ruleset.clone())
                })
                .collect::<BTreeMap<_, _>>();
            let declarations = EraDeclarations::try_from(declarations)
                .expect("The era-boundary test schedule must be well-formed");
            *settings = DeploymentSettings::new(&declarations, settings.genesis_block().clone())
                .expect("The accelerated era-boundary deployment must validate");
        }
    }
}

impl Default for TopologyConfig {
    fn default() -> Self {
        Self {
            n_nodes: 0,
            blend_core_nodes: 0,
            sdp_funding_config: SdpFundingConfig::default(),
            network_params: Arc::new(NetworkParams::default()),
            wallet_config: WalletConfig::default(),
            scenario_base_dir: std::env::temp_dir(),
            genesis_block: None,
            requested_genesis_time: None,
            genesis_time: None,
            slot_duration: Some(Duration::from_secs(DEFAULT_SLOT_TIME_IN_SECS)),
            active_slot_coeff: DEFAULT_ACTIVE_SLOT_COEFF,
            security_param: DEFAULT_SECURITY_PARAM,
            cucumber_parameter_set: None,
            era_boundary_schedule: None,
            node_config_overrides: HashMap::new(),
            allow_multiple_genesis_tokens: false,
            allow_zero_value_genesis_tokens: false,
            test_context: None,
            node_binary_profile: NodeBinaryProfile::default(),
        }
    }
}

/// Deployment-facing builder.
#[derive(Clone)]
pub struct DeploymentBuilder {
    config: TopologyConfig,
    seed: Option<DeploymentSeed>,
    blend_ports: Vec<u16>,
}

impl DeploymentBuilder {
    #[must_use]
    pub const fn new(config: TopologyConfig) -> Self {
        Self {
            config,
            seed: None,
            blend_ports: Vec::new(),
        }
    }

    #[must_use]
    pub const fn with_deployment_seed(mut self, seed: DeploymentSeed) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Gives the nodes these Blend ports, one per node, instead of newly
    /// allocated ones. The genesis block declares each node's Blend port, so a
    /// deployment rebuilt with the same seed and ports has the same genesis
    /// block.
    #[must_use]
    pub fn with_blend_ports(mut self, ports: Vec<u16>) -> Self {
        self.blend_ports = ports;
        self
    }

    #[must_use]
    pub const fn with_node_count(mut self, nodes: usize) -> Self {
        self.config.n_nodes = nodes;
        self
    }

    #[must_use]
    pub const fn nodes(self, nodes: usize) -> Self {
        self.with_node_count(nodes)
    }

    #[must_use]
    pub fn scenario_base_dir(mut self, dir: PathBuf) -> Self {
        self.config.scenario_base_dir = dir;
        self
    }

    #[must_use]
    pub fn with_network_layout(mut self, layout: Libp2pNetworkLayout) -> Self {
        self.config.network_params = Arc::new(NetworkParams {
            libp2p_network_layout: layout,
        });
        self
    }

    #[must_use]
    pub fn with_wallet_config(mut self, wallet: WalletConfig) -> Self {
        self.config.wallet_config = wallet;
        self
    }

    /// Overrides the node deployment's Cryptarchia security parameter.
    #[must_use]
    pub const fn with_security_param(mut self, security_param: NonZeroU32) -> Self {
        self.config.security_param = security_param;
        self
    }

    /// Overrides the node deployment's Cryptarchia slot activation coefficient.
    #[must_use]
    pub const fn with_slot_activation_coeff(
        mut self,
        numerator: u32,
        denominator: NonZeroU32,
    ) -> Self {
        self.config.active_slot_coeff = NonNegativeRatio::new(numerator, denominator);
        self
    }

    #[must_use]
    pub fn with_test_context(mut self, test_context: &str) -> Self {
        self.config.test_context = Some(test_context.to_owned());
        self
    }

    #[must_use]
    pub const fn with_genesis_time(mut self, genesis_time: GenesisTime) -> Self {
        self.config.requested_genesis_time = Some(genesis_time);
        self
    }

    pub fn build(self) -> Result<DeploymentPlan, TopologyBuildError> {
        super::build_plan(self.prepare()?)
    }

    /// Prepares the shared network before constructing any Logos service
    /// config. Other implementations can consume these inputs without
    /// building a Logos plan.
    pub fn prepare(mut self) -> Result<PreparedDeployment, TopologyBuildError> {
        let genesis_time = self
            .config
            .requested_genesis_time
            .unwrap_or_else(resolve_automatic_genesis_time);
        self.config.genesis_time = Some(genesis_time);

        self.config.wallet_config.validate(
            self.config.allow_multiple_genesis_tokens,
            self.config.allow_zero_value_genesis_tokens,
        )?;

        let node_count = self.config.n_nodes;
        if node_count == 0 {
            return Ok(PreparedDeployment {
                config: self.config,
                network: None,
            });
        }

        assert!(
            self.config.blend_core_nodes <= node_count,
            "blend_core_nodes({}) must be <= n_nodes({node_count})",
            self.config.blend_core_nodes
        );

        let ids = generate_node_ids(node_count, self.seed.as_ref());

        let wallet_accounts = self
            .config
            .wallet_config
            .accounts
            .iter()
            .map(|account| (account.secret_key.clone(), account.value))
            .collect::<Vec<_>>();

        let blend_ports = if self.blend_ports.is_empty() {
            allocate_blend_ports(node_count)?
        } else {
            self.blend_ports.clone()
        };
        let network = prepare_network(
            &ids,
            &blend_ports,
            self.config.blend_core_nodes,
            wallet_accounts.len(),
            &wallet_accounts,
            self.config.sdp_funding_config,
            self.config.test_context.as_deref(),
            genesis_time,
        );
        self.config.genesis_block = Some(network.genesis.clone());

        Ok(PreparedDeployment {
            config: self.config,
            network: Some(network),
        })
    }
}

fn allocate_blend_ports(node_count: usize) -> Result<Vec<u16>, TopologyBuildError> {
    let mut ports = Vec::with_capacity(node_count);

    for _ in 0..node_count {
        let Some(port) = get_reserved_available_udp_port() else {
            return Err(TopologyBuildError::BlendPortAllocation);
        };
        ports.push(port);
    }

    Ok(ports)
}

fn generate_node_ids(node_count: usize, seed: Option<&DeploymentSeed>) -> Vec<[u8; 32]> {
    let mut ids = vec![[0; 32]; node_count];
    if let Some(seed) = seed {
        let mut rng = rand::rngs::StdRng::from_seed(*seed.bytes());
        fill_node_ids(&mut ids, &mut rng);
        return ids;
    }

    let mut rng = rand::thread_rng();
    fill_node_ids(&mut ids, &mut rng);

    ids
}

fn fill_node_ids<R>(ids: &mut [[u8; 32]], rng: &mut R)
where
    R: Rng + ?Sized,
{
    for id in ids {
        rng.fill(id);
    }
}

impl DeploymentProvider<DeploymentPlan> for DeploymentBuilder {
    fn build(&self, seed: Option<&DeploymentSeed>) -> Result<DeploymentPlan, DynTopologyError> {
        let builder = seed.map_or_else(
            || self.clone(),
            |seed| self.clone().with_deployment_seed(seed.clone()),
        );

        builder
            .build()
            .map_err(|error| Box::new(error) as DynTopologyError)
    }
}

#[cfg(test)]
mod tests {
    use lb_core::sdp::ServiceType;

    use super::*;

    #[test]
    fn automatic_genesis_times_are_unique_within_a_process() {
        let first = DeploymentBuilder::new(TopologyConfig::empty())
            .build()
            .expect("first deployment should build");
        let second = DeploymentBuilder::new(TopologyConfig::empty())
            .build()
            .expect("second deployment should build");

        assert_ne!(
            first.config().genesis_time(),
            second.config().genesis_time()
        );
    }

    #[test]
    fn explicit_genesis_times_can_be_shared() {
        let genesis_time = GenesisTime::new(1_000);
        let first = DeploymentBuilder::new(TopologyConfig::empty())
            .with_genesis_time(genesis_time)
            .build()
            .expect("first deployment should build");
        let second = DeploymentBuilder::new(TopologyConfig::empty())
            .with_genesis_time(genesis_time)
            .build()
            .expect("second deployment should build");

        assert_eq!(first.config().genesis_time(), genesis_time);
        assert_eq!(second.config().genesis_time(), genesis_time);
    }

    #[test]
    fn era_zero_to_era_one_deployment_uses_the_requested_boundary() {
        let topology = DeploymentBuilder::new(
            TopologyConfig::with_node_numbers(2)
                .with_parameter_set(
                    CucumberParameterSet::from_name("cucumber_fixture")
                        .expect("fixture profile should exist"),
                )
                .with_era_boundary_schedule(
                    EraBoundarySchedule::new(0, 1, 1).expect("schedule should be valid"),
                ),
        )
        .build()
        .expect("two node fixture must build");
        let settings = super::super::deployment_settings_for_topology(
            topology.config().genesis_block.as_ref().unwrap(),
            topology.config(),
        );
        let eras = settings.era_schedule().iter().collect::<Vec<_>>();
        assert_eq!(eras.len(), 2);
        assert_eq!(eras[0].first_slot.into_inner(), 0);
        assert_eq!(eras[1].first_epoch.into_inner(), 1);
        assert_eq!(eras[1].first_slot.into_inner(), 160);
        assert_ne!(
            eras[0].parameters.fork_digest,
            eras[1].parameters.fork_digest
        );
        let era_zero_rules =
            serde_yaml::to_string(&eras[0].parameters.ruleset).expect("era rules should serialize");
        let era_one_rules =
            serde_yaml::to_string(&eras[1].parameters.ruleset).expect("era rules should serialize");
        assert_eq!(era_zero_rules, era_one_rules);
        assert_eq!(
            settings
                .genesis_era_parameters()
                .cryptarchia
                .uncle_reference_window_in_block,
            4
        );
        assert_cucumber_fixture_parameters(settings.genesis_era_parameters());
    }

    #[test]
    fn era_one_to_era_two_deployment_keeps_the_genesis_era_declaration() {
        let topology = DeploymentBuilder::new(
            TopologyConfig::with_node_numbers(2)
                .with_parameter_set(
                    CucumberParameterSet::from_name("cucumber_fixture")
                        .expect("fixture profile should exist"),
                )
                .with_era_boundary_schedule(
                    EraBoundarySchedule::new(1, 2, 3).expect("schedule should be valid"),
                ),
        )
        .build()
        .expect("two node fixture must build");
        let settings = super::super::deployment_settings_for_topology(
            topology.config().genesis_block.as_ref().unwrap(),
            topology.config(),
        );
        let eras = settings.era_schedule().iter().collect::<Vec<_>>();

        assert_eq!(eras.len(), 3);
        assert_eq!(
            eras.iter()
                .map(|era| era.first_epoch.into_inner())
                .collect::<Vec<_>>(),
            [0, 1, 3]
        );
        assert_eq!(
            eras.iter()
                .map(|era| era.first_slot.into_inner())
                .collect::<Vec<_>>(),
            [0, 160, 480]
        );
        for pair in eras.windows(2) {
            assert_ne!(
                pair[0].parameters.fork_digest,
                pair[1].parameters.fork_digest
            );
            let previous_rules = serde_yaml::to_string(&pair[0].parameters.ruleset)
                .expect("era rules should serialize");
            let target_rules = serde_yaml::to_string(&pair[1].parameters.ruleset)
                .expect("era rules should serialize");
            assert_eq!(previous_rules, target_rules);
        }
        assert_cucumber_fixture_parameters(settings.genesis_era_parameters());
        assert_eq!(
            settings
                .genesis_era_parameters()
                .cryptarchia
                .uncle_reference_window_in_block,
            4
        );
    }

    #[test]
    fn cucumber_parameter_sets_validate_and_derive_expected_deployment_values() {
        for (name, k, f, phases, slots_per_epoch, uncle_window) in [
            ("cucumber_fixture", 8, (1, 2), (3, 3, 4), 160, 4),
            ("fast_repro", 3, (1, 10), (1, 1, 1), 90, 1),
            ("testnet_representative", 10, (1, 30), (3, 3, 4), 3_000, 6),
        ] {
            let parameter_set = CucumberParameterSet::from_name(name)
                .unwrap_or_else(|| panic!("profile {name} should exist"));
            let deployment = DeploymentBuilder::new(
                TopologyConfig::with_node_numbers(2).with_parameter_set(parameter_set),
            )
            .build()
            .unwrap_or_else(|error| panic!("profile {name} must validate: {error}"));
            let settings = super::super::deployment_settings_for_topology(
                deployment.config().genesis_block.as_ref().unwrap(),
                deployment.config(),
            );
            let parameters = settings.genesis_era_parameters();
            let cryptarchia = &parameters.cryptarchia;

            assert_eq!(cryptarchia.security_param.get(), k, "profile {name}");
            assert_eq!(
                (
                    cryptarchia.slot_activation_coeff.numerator,
                    cryptarchia.slot_activation_coeff.denominator.get(),
                ),
                f,
                "profile {name}"
            );
            assert_eq!(parameters.time.slot_duration(), Duration::from_secs(1));
            assert_eq!(
                (
                    cryptarchia
                        .epoch_config
                        .epoch_stake_distribution_stabilization
                        .get(),
                    cryptarchia.epoch_config.epoch_period_nonce_buffer.get(),
                    cryptarchia
                        .epoch_config
                        .epoch_period_nonce_stabilization
                        .get(),
                ),
                phases,
                "profile {name}"
            );
            assert_eq!(
                cryptarchia.slots_per_epoch(),
                slots_per_epoch,
                "profile {name}"
            );
            assert_eq!(
                cryptarchia.uncle_reference_window_in_block, uncle_window,
                "profile {name}"
            );
        }

        assert!(CucumberParameterSet::from_name("clean_control").is_none());
    }

    #[test]
    fn explicit_uncle_window_is_preserved_and_invalid_window_is_rejected() {
        let parameter_set = CucumberParameterSet::from_name("cucumber_fixture")
            .expect("fixture profile should exist");
        let mut settings = DeploymentSettings::default();
        parameter_set.apply_to(&mut settings);

        settings
            .update_genesis_era_parameters(|parameters| {
                parameters.cryptarchia.uncle_reference_window_in_block = 3;
            })
            .expect("valid explicit W override should be retained");
        assert_eq!(
            settings
                .genesis_era_parameters()
                .cryptarchia
                .uncle_reference_window_in_block,
            3
        );

        assert!(
            settings
                .update_genesis_era_parameters(|parameters| {
                    parameters.cryptarchia.uncle_reference_window_in_block = 5;
                })
                .is_err()
        );
        assert_eq!(
            settings
                .genesis_era_parameters()
                .cryptarchia
                .uncle_reference_window_in_block,
            3
        );
    }

    #[test]
    fn zero_blend_provider_topology_has_no_blend_genesis_declarations() {
        let deployment = DeploymentBuilder::new(
            TopologyConfig::with_node_numbers(2)
                .with_blend_core_nodes(0)
                .with_parameter_set(
                    CucumberParameterSet::from_name("cucumber_fixture")
                        .expect("fixture profile should exist"),
                )
                .with_era_boundary_schedule(
                    EraBoundarySchedule::new(0, 1, 1).expect("schedule should be valid"),
                ),
        )
        .build()
        .expect("broadcast topology should build");
        let genesis = deployment.config().genesis_block.as_ref().unwrap();
        let declared_blend_providers = genesis
            .genesis_tx()
            .sdp_declarations()
            .filter(|declaration| declaration.operation().service_type == ServiceType::BlendNetwork)
            .count();

        assert_eq!(deployment.config().blend_core_nodes, 0);
        assert_eq!(declared_blend_providers, 0);
    }

    #[test]
    fn era_boundary_schedule_rejects_nonconsecutive_eras_and_insufficient_epochs() {
        assert!(EraBoundarySchedule::new(0, 2, 3).is_err());
        assert!(EraBoundarySchedule::new(1, 2, 1).is_err());
    }

    fn assert_cucumber_fixture_parameters(
        parameters: &lb_node::config::deployment::era::ruleset::v1::Parameters,
    ) {
        let cryptarchia = &parameters.cryptarchia;
        assert_eq!(cryptarchia.security_param.get(), 8);
        assert_eq!(cryptarchia.slot_activation_coeff.numerator, 1);
        assert_eq!(cryptarchia.slot_activation_coeff.denominator.get(), 2);
        assert_eq!(parameters.time.slot_duration(), Duration::from_secs(1));
        assert_eq!(
            (
                cryptarchia
                    .epoch_config
                    .epoch_stake_distribution_stabilization
                    .get(),
                cryptarchia.epoch_config.epoch_period_nonce_buffer.get(),
                cryptarchia
                    .epoch_config
                    .epoch_period_nonce_stabilization
                    .get(),
            ),
            (3, 3, 4)
        );
        assert_eq!(cryptarchia.slots_per_epoch(), 160);
    }
}
