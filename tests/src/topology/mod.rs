pub mod configs;

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use configs::{
    GeneralConfig,
    consensus::{GeneralConsensusConfig, ProviderInfo, create_genesis_tx_with_declarations},
    da::{DaParams, create_da_configs},
    network::{NetworkParams, create_network_configs},
    tracing::create_tracing_configs,
};
use futures::future::join_all;
use key_management_system_service::{
    backend::preload::PreloadKMSBackendSettings,
    keys::{Ed25519Key, ZkKey},
};
use nomos_core::{
    mantle::{GenesisTx as _, Note, NoteId},
    sdp::{Locator, ServiceType, SessionNumber},
};
use nomos_da_network_core::swarm::{BalancerStats, DAConnectionPolicySettings};
use nomos_da_network_service::MembershipResponse;
use nomos_network::backends::libp2p::Libp2pInfo;
use nomos_utils::net::get_available_udp_port;
use rand::{Rng as _, thread_rng};
use tokio::time::{sleep, timeout};

use crate::{
    adjust_timeout,
    common::kms::key_id_for_preload_backend,
    nodes::{
        executor::{Executor, create_executor_config},
        validator::{Validator, create_validator_config},
    },
    topology::configs::{
        api::create_api_configs,
        blend::{GeneralBlendConfig, create_blend_configs},
        consensus::{SHORT_PROLONGED_BOOTSTRAP_PERIOD, create_consensus_configs},
        da::GeneralDaConfig,
        time::default_time_config,
    },
};

pub struct TopologyConfig {
    pub n_validators: usize,
    pub n_executors: usize,
    pub da_params: DaParams,
    pub network_params: NetworkParams,
    pub extra_genesis_notes: Vec<GenesisNoteSpec>,
}

impl TopologyConfig {
    #[must_use]
    pub fn one_validator() -> Self {
        Self {
            n_validators: 1,
            n_executors: 0,
            da_params: DaParams::default(),
            network_params: NetworkParams::default(),
            extra_genesis_notes: Vec::new(),
        }
    }

    #[must_use]
    pub fn two_validators() -> Self {
        Self {
            n_validators: 2,
            n_executors: 0,
            da_params: DaParams::default(),
            network_params: NetworkParams::default(),
            extra_genesis_notes: Vec::new(),
        }
    }

    #[must_use]
    pub fn validator_and_executor() -> Self {
        Self {
            n_validators: 1,
            n_executors: 1,
            da_params: DaParams {
                dispersal_factor: 2,
                subnetwork_size: 2,
                num_subnets: 2,
                policy_settings: DAConnectionPolicySettings {
                    min_dispersal_peers: 1,
                    min_replication_peers: 1,
                    max_dispersal_failures: 0,
                    max_sampling_failures: 0,
                    max_replication_failures: 0,
                    malicious_threshold: 0,
                },
                balancer_interval: Duration::from_secs(1),
                ..Default::default()
            },
            network_params: NetworkParams::default(),
            extra_genesis_notes: Vec::new(),
        }
    }

    #[must_use]
    pub fn validators_and_executor(
        num_validators: usize,
        num_subnets: usize,
        dispersal_factor: usize,
    ) -> Self {
        Self {
            n_validators: num_validators,
            n_executors: 1,
            da_params: DaParams {
                dispersal_factor,
                subnetwork_size: num_subnets,
                num_subnets: num_subnets as u16,
                policy_settings: DAConnectionPolicySettings {
                    min_dispersal_peers: num_subnets,
                    min_replication_peers: dispersal_factor - 1,
                    max_dispersal_failures: 0,
                    max_sampling_failures: 0,
                    max_replication_failures: 0,
                    malicious_threshold: 0,
                },
                balancer_interval: Duration::from_secs(5),
                ..Default::default()
            },
            network_params: NetworkParams::default(),
            extra_genesis_notes: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_extra_genesis_note(mut self, note_spec: GenesisNoteSpec) -> Self {
        self.extra_genesis_notes.push(note_spec);
        self
    }
}

#[derive(Clone)]
pub struct GenesisNoteSpec {
    pub note: Note,
    pub note_sk: ZkKey,
}

#[derive(Clone)]
pub struct InjectedGenesisNote {
    pub note_id: NoteId,
}

pub struct Topology {
    validators: Vec<Validator>,
    executors: Vec<Executor>,
    general_configs: Vec<GeneralConfig>,
    injected_genesis_notes: Vec<InjectedGenesisNote>,
}

impl Topology {
    pub async fn spawn(config: TopologyConfig) -> Self {
        let n_participants = config.n_validators + config.n_executors;

        // we use the same random bytes for:
        // * da id
        // * coin sk
        // * coin nonce
        // * libp2p node key
        let mut ids = vec![[0; 32]; n_participants];
        let mut da_ports = vec![];
        let mut blend_ports = vec![];
        for id in &mut ids {
            thread_rng().fill(id);
            da_ports.push(get_available_udp_port().unwrap());
            blend_ports.push(get_available_udp_port().unwrap());
        }

        let mut consensus_configs =
            create_consensus_configs(&ids, SHORT_PROLONGED_BOOTSTRAP_PERIOD);
        let da_configs = create_da_configs(&ids, &config.da_params, &da_ports);
        let network_configs = create_network_configs(&ids, &config.network_params);
        let blend_configs = create_blend_configs(&ids, &blend_ports);
        let api_configs = create_api_configs(&ids);
        let tracing_configs = create_tracing_configs(&ids);
        let time_config = default_time_config();

        // Setup genesis TX with Blend and DA service declarations.
        let base_ledger_tx = consensus_configs[0]
            .genesis_tx()
            .mantle_tx()
            .ledger_tx
            .clone();
        let mut ledger_tx = base_ledger_tx.clone();
        let base_outputs = ledger_tx.outputs.len();
        for note_spec in &config.extra_genesis_notes {
            ledger_tx.outputs.push(note_spec.note);
        }
        let mut providers: Vec<_> = da_configs
            .iter()
            .enumerate()
            .map(|(i, da_conf)| ProviderInfo {
                service_type: ServiceType::DataAvailability,
                provider_sk: da_conf.signer.clone(),
                zk_sk: da_conf.secret_zk_key.clone(),
                locator: Locator(da_conf.listening_address.clone()),
                note: consensus_configs[0].da_notes[i].clone(),
            })
            .collect();
        providers.extend(blend_configs.iter().enumerate().map(
            |(i, (blend_conf, zk_secret_key))| ProviderInfo {
                service_type: ServiceType::BlendNetwork,
                provider_sk: blend_conf.non_ephemeral_signing_key.clone().into(),
                zk_sk: zk_secret_key.clone(),
                locator: Locator(blend_conf.core.backend.listening_address.clone()),
                note: consensus_configs[0].blend_notes[i].clone(),
            },
        ));

        // Update genesis TX to contain Blend and DA providers.
        let genesis_tx = create_genesis_tx_with_declarations(ledger_tx, providers);
        let updated_ledger_tx = genesis_tx.mantle_tx().ledger_tx.clone();
        let injected_utxos: Vec<_> = updated_ledger_tx
            .utxos()
            .skip(base_outputs)
            .collect::<Vec<_>>();

        for c in &mut consensus_configs {
            c.utxos.extend(injected_utxos.iter().copied());
        }

        let injected_infos = injected_utxos
            .iter()
            .map(|utxo| InjectedGenesisNote { note_id: utxo.id() })
            .collect::<Vec<_>>();

        for c in &mut consensus_configs {
            c.override_genesis_tx(genesis_tx.clone());
        }

        // Set Blend and DA keys in KMS of each node config.
        let kms_configs = create_kms_configs(&blend_configs, &da_configs);

        let mut node_configs = vec![];

        for i in 0..n_participants {
            node_configs.push(GeneralConfig {
                consensus_config: consensus_configs[i].clone(),
                da_config: da_configs[i].clone(),
                network_config: network_configs[i].clone(),
                blend_config: blend_configs[i].clone(),
                api_config: api_configs[i].clone(),
                tracing_config: tracing_configs[i].clone(),
                time_config: time_config.clone(),
                kms_config: kms_configs[i].clone(),
            });
        }

        let general_configs = node_configs.clone();

        let (validators, executors) =
            Self::spawn_validators_executors(node_configs, config.n_validators, config.n_executors)
                .await;

        Self {
            validators,
            executors,
            general_configs,
            injected_genesis_notes: injected_infos,
        }
    }

    pub async fn spawn_with_empty_membership(
        config: TopologyConfig,
        ids: &[[u8; 32]],
        da_ports: &[u16],
        blend_ports: &[u16],
    ) -> Self {
        let n_participants = config.n_validators + config.n_executors;

        let consensus_configs = create_consensus_configs(ids, SHORT_PROLONGED_BOOTSTRAP_PERIOD);
        let da_configs = create_da_configs(ids, &config.da_params, da_ports);
        let network_configs = create_network_configs(ids, &config.network_params);
        let blend_configs = create_blend_configs(ids, blend_ports);
        let api_configs = create_api_configs(ids);
        // Create membership configs without DA nodes.
        let tracing_configs = create_tracing_configs(ids);
        let time_config = default_time_config();

        let kms_config = PreloadKMSBackendSettings {
            keys: HashMap::new(),
        };

        let mut node_configs = vec![];

        for i in 0..n_participants {
            node_configs.push(GeneralConfig {
                consensus_config: consensus_configs[i].clone(),
                da_config: da_configs[i].clone(),
                network_config: network_configs[i].clone(),
                blend_config: blend_configs[i].clone(),
                api_config: api_configs[i].clone(),
                tracing_config: tracing_configs[i].clone(),
                time_config: time_config.clone(),
                kms_config: kms_config.clone(),
            });
        }
        let general_configs = node_configs.clone();
        let (validators, executors) =
            Self::spawn_validators_executors(node_configs, config.n_validators, config.n_executors)
                .await;

        Self {
            validators,
            executors,
            general_configs,
            injected_genesis_notes: Vec::new(),
        }
    }

    async fn spawn_validators_executors(
        config: Vec<GeneralConfig>,
        n_validators: usize,
        n_executors: usize,
    ) -> (Vec<Validator>, Vec<Executor>) {
        let mut validators = Vec::new();
        for i in 0..n_validators {
            let config = create_validator_config(config[i].clone());
            validators.push(Validator::spawn(config).await.unwrap());
        }

        let mut executors = Vec::new();
        for i in n_validators..(n_validators + n_executors) {
            let config = create_executor_config(config[i].clone());
            executors.push(Executor::spawn(config).await);
        }

        (validators, executors)
    }

    #[must_use]
    pub fn validators(&self) -> &[Validator] {
        &self.validators
    }

    #[must_use]
    pub fn executors(&self) -> &[Executor] {
        &self.executors
    }

    #[must_use]
    pub fn general_config(&self, index: usize) -> Option<&GeneralConfig> {
        self.general_configs.get(index)
    }

    #[must_use]
    pub fn validator_consensus_config(
        &self,
        validator_index: usize,
    ) -> Option<&GeneralConsensusConfig> {
        self.general_config(validator_index)
            .map(|config| &config.consensus_config)
    }

    #[must_use]
    pub fn injected_genesis_notes(&self) -> &[InjectedGenesisNote] {
        &self.injected_genesis_notes
    }

    pub async fn wait_network_ready(&self) {
        let listen_ports = self.node_listen_ports();
        if listen_ports.len() <= 1 {
            return;
        }

        let initial_peer_ports = self.node_initial_peer_ports();
        let expected_peer_counts = find_expected_peer_counts(&listen_ports, &initial_peer_ports);
        let labels = self.node_labels();

        let check = NetworkReadiness {
            topology: self,
            expected_peer_counts: &expected_peer_counts,
            labels: &labels,
        };

        check.wait().await;
    }

    pub async fn wait_da_network_ready(&self) {
        let total_nodes = self.validators.len() + self.executors.len();

        if total_nodes == 0 {
            return;
        }

        // Get num_subnets from first executor's config (all nodes have same num_subnets
        // in tests)
        let expected_subnets = if let Some(executor) = self.executors.first() {
            executor.config().da_network.backend.num_subnets as usize
        } else if let Some(validator) = self.validators.first() {
            validator
                .config()
                .da_network
                .backend
                .subnets_settings
                .num_of_subnets
        } else {
            return;
        };

        let labels = self.node_labels();

        let check = DANetworkReadiness {
            topology: self,
            labels: &labels,
            expected_subnets,
        };

        check.wait().await;
    }

    pub async fn wait_membership_ready(&self) {
        self.wait_membership_ready_for_session(SessionNumber::from(0u64))
            .await;
    }

    pub async fn wait_membership_ready_for_session(&self, session: SessionNumber) {
        self.wait_membership_assignations(session, true).await;
    }

    pub async fn wait_membership_empty_for_session(&self, session: SessionNumber) {
        self.wait_membership_assignations(session, false).await;
    }

    async fn wait_membership_assignations(&self, session: SessionNumber, expect_non_empty: bool) {
        let total_nodes = self.validators.len() + self.executors.len();

        if total_nodes == 0 {
            return;
        }

        let labels = self.node_labels();

        let check = MembershipReadiness {
            topology: self,
            session,
            labels: &labels,
            expect_non_empty,
        };

        check.wait().await;
    }

    fn node_listen_ports(&self) -> Vec<u16> {
        self.validators
            .iter()
            .map(|node| node.config().network.backend.swarm.port)
            .chain(
                self.executors
                    .iter()
                    .map(|node| node.config().network.backend.swarm.port),
            )
            .collect()
    }

    fn node_initial_peer_ports(&self) -> Vec<HashSet<u16>> {
        self.validators
            .iter()
            .map(|node| {
                node.config()
                    .network
                    .backend
                    .initial_peers
                    .iter()
                    .filter_map(multiaddr_port)
                    .collect::<HashSet<u16>>()
            })
            .chain(self.executors.iter().map(|node| {
                node.config()
                    .network
                    .backend
                    .initial_peers
                    .iter()
                    .filter_map(multiaddr_port)
                    .collect::<HashSet<u16>>()
            }))
            .collect()
    }

    fn node_labels(&self) -> Vec<String> {
        self.validators
            .iter()
            .enumerate()
            .map(|(idx, node)| {
                format!(
                    "validator#{idx}@{}",
                    node.config().network.backend.swarm.port
                )
            })
            .chain(self.executors.iter().enumerate().map(|(idx, node)| {
                format!(
                    "executor#{idx}@{}",
                    node.config().network.backend.swarm.port
                )
            }))
            .collect()
    }
}
#[async_trait::async_trait]
trait ReadinessCheck<'a> {
    type Data: Send;

    async fn collect(&'a self) -> Self::Data;

    fn is_ready(&self, data: &Self::Data) -> bool;

    fn timeout_message(&self, data: Self::Data) -> String;

    fn poll_interval(&self) -> Duration {
        Duration::from_millis(200)
    }

    async fn wait(&'a self) {
        let timeout_duration = adjust_timeout(Duration::from_secs(60));
        let poll_interval = self.poll_interval();
        let mut data = self.collect().await;

        let wait_result = timeout(timeout_duration, async {
            loop {
                if self.is_ready(&data) {
                    return;
                }

                sleep(poll_interval).await;

                data = self.collect().await;
            }
        })
        .await;

        if wait_result.is_err() {
            let message = self.timeout_message(data);
            panic!("{message}");
        }
    }
}

struct NetworkReadiness<'a> {
    topology: &'a Topology,
    expected_peer_counts: &'a [usize],
    labels: &'a [String],
}

#[async_trait::async_trait]
impl<'a> ReadinessCheck<'a> for NetworkReadiness<'a> {
    type Data = Vec<Libp2pInfo>;

    async fn collect(&'a self) -> Self::Data {
        let (validator_infos, executor_infos) = tokio::join!(
            join_all(self.topology.validators.iter().map(Validator::network_info)),
            join_all(self.topology.executors.iter().map(Executor::network_info))
        );

        validator_infos.into_iter().chain(executor_infos).collect()
    }

    fn is_ready(&self, data: &Self::Data) -> bool {
        data.iter()
            .enumerate()
            .all(|(idx, info)| info.n_peers >= self.expected_peer_counts[idx])
    }

    fn timeout_message(&self, data: Self::Data) -> String {
        let summary = build_timeout_summary(self.labels, data, self.expected_peer_counts);
        format!("timed out waiting for network readiness: {summary}")
    }
}

struct DANetworkReadiness<'a> {
    topology: &'a Topology,
    labels: &'a [String],
    expected_subnets: usize,
}

#[async_trait::async_trait]
impl<'a> ReadinessCheck<'a> for DANetworkReadiness<'a> {
    type Data = Vec<BalancerStats>;

    async fn collect(&'a self) -> Self::Data {
        let (validator_stats, executor_stats) = tokio::join!(
            join_all(
                self.topology
                    .validators
                    .iter()
                    .map(Validator::balancer_stats)
            ),
            join_all(self.topology.executors.iter().map(Executor::balancer_stats))
        );
        validator_stats.into_iter().chain(executor_stats).collect()
    }

    fn is_ready(&self, data: &Self::Data) -> bool {
        data.iter().all(|stats| {
            let connected_subnets = stats
                .values()
                .filter(|subnet_stats| subnet_stats.inbound > 0 || subnet_stats.outbound > 0)
                .count();

            // Check that enough subnets are connected (matches subnet_threshold check in
            // lib.rs)
            connected_subnets >= self.expected_subnets
        })
    }

    fn timeout_message(&self, data: Self::Data) -> String {
        let mut details = Vec::new();

        for (label, stats) in self.labels.iter().zip(data.iter()) {
            let connected_subnets = stats
                .values()
                .filter(|s| s.inbound > 0 || s.outbound > 0)
                .count();

            let subnet_details: Vec<String> = stats
                .iter()
                .map(|(subnet_id, s)| {
                    format!("subnet_{}: in={}, out={}", subnet_id, s.inbound, s.outbound)
                })
                .collect();

            details.push(format!(
                "{}: {}/{} subnets connected\n  Details: [{}]",
                label,
                connected_subnets,
                self.expected_subnets,
                subnet_details.join(", ")
            ));
        }

        format!(
            "timed out waiting for DA network connections:\n{}",
            details.join("\n")
        )
    }
}

struct MembershipReadiness<'a> {
    topology: &'a Topology,
    session: SessionNumber,
    labels: &'a [String],
    expect_non_empty: bool,
}

#[async_trait::async_trait]
impl<'a> ReadinessCheck<'a> for MembershipReadiness<'a> {
    type Data = Vec<Result<MembershipResponse, reqwest::Error>>;

    async fn collect(&'a self) -> Self::Data {
        let (validator_responses, executor_responses) = tokio::join!(
            join_all(
                self.topology
                    .validators
                    .iter()
                    .map(|node| node.da_get_membership(self.session)),
            ),
            join_all(
                self.topology
                    .executors
                    .iter()
                    .map(|node| node.da_get_membership(self.session)),
            )
        );

        validator_responses
            .into_iter()
            .chain(executor_responses)
            .collect()
    }

    fn is_ready(&self, data: &Self::Data) -> bool {
        self.assignation_statuses(data)
            .into_iter()
            .all(|ready| ready)
    }

    fn timeout_message(&self, data: Self::Data) -> String {
        let statuses = self.assignation_statuses(&data);
        let description = if self.expect_non_empty {
            "non-empty assignations"
        } else {
            "empty assignations"
        };
        let summary = build_membership_summary(self.labels, &statuses, description);
        format!("timed out waiting for DA membership readiness ({description}): {summary}")
    }
}

impl MembershipReadiness<'_> {
    fn assignation_statuses(
        &self,
        responses: &[Result<MembershipResponse, reqwest::Error>],
    ) -> Vec<bool> {
        responses
            .iter()
            .map(|res| {
                res.as_ref()
                    .map(|resp| {
                        let is_non_empty = !resp.assignations.is_empty();
                        if self.expect_non_empty {
                            is_non_empty
                        } else {
                            !is_non_empty
                        }
                    })
                    .unwrap_or(false)
            })
            .collect()
    }
}

fn build_timeout_summary(
    labels: &[String],
    infos: Vec<Libp2pInfo>,
    expected_counts: &[usize],
) -> String {
    infos
        .into_iter()
        .zip(expected_counts.iter())
        .zip(labels.iter())
        .map(|((info, expected), label)| {
            format!("{}: peers={}, expected={}", label, info.n_peers, expected)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn build_membership_summary(labels: &[String], statuses: &[bool], description: &str) -> String {
    statuses
        .iter()
        .zip(labels.iter())
        .map(|(ready, label)| {
            let status = if *ready { "ready" } else { "waiting" };
            format!("{label}: status={status}, expected {description}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn multiaddr_port(addr: &nomos_libp2p::Multiaddr) -> Option<u16> {
    for protocol in addr {
        match protocol {
            nomos_libp2p::Protocol::Udp(port) | nomos_libp2p::Protocol::Tcp(port) => {
                return Some(port);
            }
            _ => {}
        }
    }
    None
}

fn find_expected_peer_counts(
    listen_ports: &[u16],
    initial_peer_ports: &[HashSet<u16>],
) -> Vec<usize> {
    let mut expected: Vec<HashSet<usize>> = vec![HashSet::new(); initial_peer_ports.len()];

    for (idx, ports) in initial_peer_ports.iter().enumerate() {
        for port in ports {
            let Some(peer_idx) = listen_ports.iter().position(|p| p == port) else {
                continue;
            };
            if peer_idx == idx {
                continue;
            }

            expected[idx].insert(peer_idx);
            expected[peer_idx].insert(idx);
        }
    }

    expected.into_iter().map(|set| set.len()).collect()
}

#[must_use]
pub fn create_kms_configs(
    blend_configs: &[GeneralBlendConfig],
    da_configs: &[GeneralDaConfig],
) -> Vec<PreloadKMSBackendSettings> {
    da_configs
        .iter()
        .zip(blend_configs.iter())
        .map(
            |(da_conf, (blend_conf, zk_secret_key))| PreloadKMSBackendSettings {
                keys: [
                    (
                        key_id_for_preload_backend(
                            &Ed25519Key::from(blend_conf.non_ephemeral_signing_key.clone()).into(),
                        ),
                        Ed25519Key::from(blend_conf.non_ephemeral_signing_key.clone()).into(),
                    ),
                    (
                        blend_conf.core.zk.secret_key_kms_id.clone(),
                        zk_secret_key.clone().into(),
                    ),
                    (
                        key_id_for_preload_backend(&da_conf.signer.clone().into()),
                        da_conf.signer.clone().into(),
                    ),
                    (
                        key_id_for_preload_backend(&zk_secret_key.clone().into()),
                        zk_secret_key.clone().into(),
                    ),
                ]
                .into(),
            },
        )
        .collect()
}
