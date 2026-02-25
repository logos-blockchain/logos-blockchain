pub mod configs;

use std::{collections::HashSet, time::Duration};

use configs::{
    GeneralConfig,
    consensus::{GeneralConsensusConfig, ProviderInfo, create_genesis_tx_with_declarations},
    network::{NetworkParams, create_network_configs},
    tracing::create_tracing_configs,
};
use lb_core::{
    mantle::{GenesisTx as _, Note, NoteId, genesis_tx::GenesisTx},
    sdp::{Locator, ServiceType},
};
use lb_key_management_system_service::keys::ZkKey;
use lb_network_service::backends::libp2p::Libp2pInfo;
use lb_node::config::{KmsConfig, kms::serde::PreloadKmsBackendSettings};
use lb_utils::net::get_available_udp_port;
use rand::{Rng as _, thread_rng};

use crate::{
    common::kms::key_id_for_preload_backend,
    nodes::validator::{Validator, create_validator_config},
    topology::configs::{
        api::create_api_configs,
        blend::{GeneralBlendConfig, create_blend_configs},
        consensus::{SHORT_PROLONGED_BOOTSTRAP_PERIOD, create_consensus_configs},
        deployment::e2e_deployment_settings_with_genesis_tx,
        sdp::create_sdp_configs,
        time::default_time_config,
    },
};

pub struct TopologyConfig {
    pub n_validators: usize,
    pub network_params: NetworkParams,
    pub extra_genesis_notes: Vec<GenesisNoteSpec>,
    /// Override the SDP `lock_period` for this test topology.
    /// If None, uses the default from deployment settings (10).
    pub lock_period_override: Option<u64>,
}

impl TopologyConfig {
    #[must_use]
    pub fn one_validator() -> Self {
        Self {
            n_validators: 1,
            network_params: NetworkParams::default(),
            extra_genesis_notes: Vec::new(),
            lock_period_override: None,
        }
    }

    #[must_use]
    pub fn two_validators() -> Self {
        Self {
            n_validators: 2,
            network_params: NetworkParams::default(),
            extra_genesis_notes: Vec::new(),
            lock_period_override: None,
        }
    }

    #[must_use]
    pub fn with_extra_genesis_note(mut self, note_spec: GenesisNoteSpec) -> Self {
        self.extra_genesis_notes.push(note_spec);
        self
    }

    #[must_use]
    pub const fn with_lock_period(mut self, lock_period: u64) -> Self {
        self.lock_period_override = Some(lock_period);
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
    general_configs: Vec<GeneralConfig>,
    injected_genesis_notes: Vec<InjectedGenesisNote>,
}

impl Topology {
    pub async fn spawn(config: TopologyConfig) -> Self {
        let n_participants = config.n_validators;

        // we use the same random bytes for:
        // * coin sk
        // * coin nonce
        // * libp2p node key
        let mut ids = vec![[0; 32]; n_participants];
        let mut blend_ports = vec![];
        for id in &mut ids {
            thread_rng().fill(id);
            blend_ports.push(get_available_udp_port().unwrap());
        }

        let (consensus_configs, genesis_tx) =
            create_consensus_configs(&ids, SHORT_PROLONGED_BOOTSTRAP_PERIOD);
        let network_configs = create_network_configs(&ids, &config.network_params);
        let blend_configs = create_blend_configs(&ids, &blend_ports);
        let api_configs = create_api_configs(&ids);
        let tracing_configs = create_tracing_configs(&ids);
        let time_config = default_time_config();

        // Setup genesis TX with Blend service declarations.
        let base_ledger_tx = genesis_tx.mantle_tx().ledger_tx.clone();
        let mut ledger_tx = base_ledger_tx.clone();
        let base_outputs = ledger_tx.outputs.len();
        for note_spec in &config.extra_genesis_notes {
            ledger_tx.outputs.push(note_spec.note);
        }
        let providers: Vec<_> = blend_configs
            .iter()
            .enumerate()
            .map(
                |(i, (blend_conf, private_key, zk_secret_key))| ProviderInfo {
                    service_type: ServiceType::BlendNetwork,
                    provider_sk: private_key.clone(),
                    zk_sk: zk_secret_key.clone(),
                    locator: Locator(blend_conf.core.backend.listening_address.clone()),
                    note: consensus_configs[i].blend_note.clone(),
                },
            )
            .collect();

        // Update genesis TX to contain Blend providers.
        let genesis_tx_with_declarations =
            create_genesis_tx_with_declarations(ledger_tx, providers);
        let updated_ledger_tx = genesis_tx_with_declarations.mantle_tx().ledger_tx.clone();
        let injected_utxos: Vec<_> = updated_ledger_tx
            .utxos()
            .skip(base_outputs)
            .collect::<Vec<_>>();

        let injected_infos = injected_utxos
            .iter()
            .map(|utxo| InjectedGenesisNote { note_id: utxo.id() })
            .collect::<Vec<_>>();

        // Set Blend keys in KMS of each node config.
        let kms_configs = create_kms_configs(&blend_configs, &consensus_configs);

        let sdp_configs = create_sdp_configs(&genesis_tx_with_declarations);

        let mut node_configs = vec![];

        for i in 0..n_participants {
            node_configs.push(GeneralConfig {
                consensus_config: consensus_configs[i].clone(),
                network_config: network_configs[i].clone(),
                blend_config: blend_configs[i].clone(),
                api_config: api_configs[i].clone(),
                tracing_config: tracing_configs[i].clone(),
                time_config: time_config.clone(),
                kms_config: kms_configs[i].clone(),
                sdp_config: sdp_configs[i].clone(),
            });
        }

        let general_configs = node_configs.clone();

        let validators = Self::spawn_validators(
            node_configs,
            genesis_tx_with_declarations,
            config.lock_period_override,
        )
        .await;

        Self {
            validators,
            general_configs,
            injected_genesis_notes: injected_infos,
        }
    }

    async fn spawn_validators(
        config: Vec<GeneralConfig>,
        genesis_tx: GenesisTx,
        lock_period_override: Option<u64>,
    ) -> Vec<Validator> {
        let mut validators = Vec::new();
        for general_config in config {
            let mut deployment = e2e_deployment_settings_with_genesis_tx(genesis_tx.clone());
            if let Some(lock_period) = lock_period_override {
                for params in deployment
                    .cryptarchia
                    .sdp_config
                    .service_params
                    .values_mut()
                {
                    params.lock_period = lock_period;
                }
            }
            let config = create_validator_config(general_config, deployment);
            validators.push(Validator::spawn(config).await.unwrap());
        }
        validators
    }

    #[must_use]
    pub fn validators(&self) -> &[Validator] {
        &self.validators
    }

    #[must_use]
    pub fn validators_mut(&mut self) -> &mut [Validator] {
        &mut self.validators
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

    fn node_listen_ports(&self) -> Vec<u16> {
        self.validators
            .iter()
            .map(|node| node.config().user.network.backend.swarm.port)
            .collect()
    }

    fn node_initial_peer_ports(&self) -> Vec<HashSet<u16>> {
        self.validators
            .iter()
            .map(|v| {
                v.config()
                    .user
                    .network
                    .backend
                    .initial_peers
                    .iter()
                    .filter_map(multiaddr_port)
                    .collect()
            })
            .collect()
    }

    fn node_labels(&self) -> Vec<String> {
        self.validators
            .iter()
            .enumerate()
            .map(|(i, _)| format!("validator_{i}"))
            .collect()
    }
}

#[async_trait::async_trait]
trait ReadinessCheck<'a> {
    type Data: Send;

    async fn collect(&'a self) -> Self::Data;
    fn is_ready(&self, data: &Self::Data) -> bool;
    fn timeout_message(&self, data: Self::Data) -> String;

    async fn wait(&'a self) {
        let timeout = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let data = self.collect().await;
                if self.is_ready(&data) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });

        if timeout.await.is_err() {
            let data = self.collect().await;
            panic!("{}", self.timeout_message(data));
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
        futures::future::join_all(self.topology.validators.iter().map(Validator::network_info))
            .await
    }

    fn is_ready(&self, data: &Self::Data) -> bool {
        data.iter()
            .zip(self.expected_peer_counts.iter())
            .all(|(info, expected)| info.n_peers >= *expected)
    }

    fn timeout_message(&self, data: Self::Data) -> String {
        let summary = build_timeout_summary(self.labels, data, self.expected_peer_counts);
        format!("timed out waiting for network readiness: {summary}")
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

fn multiaddr_port(addr: &lb_libp2p::Multiaddr) -> Option<u16> {
    for protocol in addr {
        match protocol {
            lb_libp2p::Protocol::Udp(port) | lb_libp2p::Protocol::Tcp(port) => {
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
    consensus_configs: &[GeneralConsensusConfig],
) -> Vec<KmsConfig> {
    blend_configs
        .iter()
        .enumerate()
        .map(|(i, (blend_conf, private_key, zk_secret_key))| KmsConfig {
            backend: PreloadKmsBackendSettings {
                keys: [
                    (
                        blend_conf.non_ephemeral_signing_key_id.clone(),
                        private_key.clone().into(),
                    ),
                    (
                        blend_conf.core.zk.secret_key_kms_id.clone(),
                        zk_secret_key.clone().into(),
                    ),
                    (
                        key_id_for_preload_backend(&consensus_configs[i].known_key.clone().into()),
                        consensus_configs[i].known_key.clone().into(),
                    ),
                    // SDP funding secret key - used by wallet for signing SDP transactions
                    (
                        key_id_for_preload_backend(&consensus_configs[i].funding_sk.clone().into()),
                        consensus_configs[i].funding_sk.clone().into(),
                    ),
                ]
                .into(),
            },
        })
        .collect()
}
