use std::{collections::HashMap, net::Ipv4Addr, str::FromStr as _};

use nomos_core::{
    mantle::GenesisTx as _,
    sdp::{Locator, ServiceType},
};
use nomos_libp2p::{Multiaddr, multiaddr};
use nomos_tracing_service::{LoggerLayer, MetricsLayer, TracingLayer, TracingSettings};
use nomos_utils::net::get_available_udp_port;
use rand::{Rng as _, thread_rng};
use tests::topology::{
    configs::{
        GeneralConfig,
        api::GeneralApiConfig,
        blend::{GeneralBlendConfig, create_blend_configs},
        consensus::{
            GeneralConsensusConfig, ProviderInfo, SHORT_PROLONGED_BOOTSTRAP_PERIOD,
            create_consensus_configs, create_genesis_tx_with_declarations,
        },
        da::{DaParams, GeneralDaConfig, create_da_configs},
        network::{NetworkParams, create_network_configs},
        time::default_time_config,
        tracing::GeneralTracingConfig,
    },
    create_kms_configs,
};

const DEFAULT_LIBP2P_NETWORK_PORT: u16 = 3000;
const DEFAULT_DA_NETWORK_PORT: u16 = 3300;
const DEFAULT_BLEND_PORT: u16 = 3400;
const DEFAULT_API_PORT: u16 = 18080;

#[derive(Eq, PartialEq, Hash, Clone)]
pub enum HostKind {
    Validator,
    Executor,
}

#[derive(Eq, PartialEq, Hash, Clone)]
pub struct Host {
    pub kind: HostKind,
    pub ip: Ipv4Addr,
    pub identifier: String,
    pub network_port: u16,
    pub da_network_port: u16,
    pub blend_port: u16,
}

impl Host {
    #[must_use]
    pub const fn default_validator_from_ip(ip: Ipv4Addr, identifier: String) -> Self {
        Self {
            kind: HostKind::Validator,
            ip,
            identifier,
            network_port: DEFAULT_LIBP2P_NETWORK_PORT,
            da_network_port: DEFAULT_DA_NETWORK_PORT,
            blend_port: DEFAULT_BLEND_PORT,
        }
    }

    #[must_use]
    pub const fn default_executor_from_ip(ip: Ipv4Addr, identifier: String) -> Self {
        Self {
            kind: HostKind::Executor,
            ip,
            identifier,
            network_port: DEFAULT_LIBP2P_NETWORK_PORT,
            da_network_port: DEFAULT_DA_NETWORK_PORT,
            blend_port: DEFAULT_BLEND_PORT,
        }
    }
}

#[must_use]
pub fn create_node_configs(
    da_params: &DaParams,
    tracing_settings: &TracingSettings,
    hosts: Vec<Host>,
) -> HashMap<Host, GeneralConfig> {
    let mut ids = vec![[0; 32]; hosts.len()];
    let mut ports = vec![];
    for id in &mut ids {
        thread_rng().fill(id);
        ports.push(get_available_udp_port().unwrap());
    }

    let mut consensus_configs = create_consensus_configs(&ids, SHORT_PROLONGED_BOOTSTRAP_PERIOD);
    let da_configs = create_da_configs(&ids, da_params, &ports);
    let network_configs = create_network_configs(&ids, &NetworkParams::default());
    let blend_configs = create_blend_configs(
        &ids,
        hosts
            .iter()
            .map(|h| h.blend_port)
            .collect::<Vec<_>>()
            .as_slice(),
    );
    let api_configs = ids
        .iter()
        .map(|_| GeneralApiConfig {
            address: format!("0.0.0.0:{DEFAULT_API_PORT}").parse().unwrap(),
        })
        .collect::<Vec<_>>();
    let mut configured_hosts = HashMap::new();

    // Rebuild DA address lists.
    let host_network_init_peers = update_network_init_peers(&hosts);

    let providers = create_providers(&hosts, &consensus_configs, &blend_configs, &da_configs);

    // Update genesis TX to contain Blend and DA providers.
    let ledger_tx = consensus_configs[0]
        .genesis_tx()
        .mantle_tx()
        .ledger_tx
        .clone();
    let genesis_tx = create_genesis_tx_with_declarations(ledger_tx, providers);
    for c in &mut consensus_configs {
        c.override_genesis_tx(genesis_tx.clone());
    }

    // Set Blend and DA keys in KMS of each node config.
    let kms_configs = create_kms_configs(&blend_configs, &da_configs);

    for (i, host) in hosts.into_iter().enumerate() {
        let consensus_config = consensus_configs[i].clone();
        let api_config = api_configs[i].clone();

        // DA Libp2p network config.
        let mut da_config = da_configs[i].clone();
        da_config.listening_address = Multiaddr::from_str(&format!(
            "/ip4/0.0.0.0/udp/{}/quic-v1",
            host.da_network_port,
        ))
        .unwrap();
        if matches!(host.kind, HostKind::Validator) {
            da_config.policy_settings.min_dispersal_peers = 0;
        }

        // Libp2p network config.
        let mut network_config = network_configs[i].clone();
        network_config.backend.swarm.host = Ipv4Addr::from_str("0.0.0.0").unwrap();
        network_config.backend.swarm.port = host.network_port;
        network_config
            .backend
            .initial_peers
            .clone_from(&host_network_init_peers);
        network_config.backend.swarm.nat_config = nomos_libp2p::NatSettings::Static {
            external_address: Multiaddr::from_str(&format!(
                "/ip4/{}/udp/{}/quic-v1",
                host.ip, host.network_port
            ))
            .unwrap(),
        };

        // Tracing config.
        let tracing_config =
            update_tracing_identifier(tracing_settings.clone(), host.identifier.clone());

        // Time config
        let time_config = default_time_config();

        configured_hosts.insert(
            host.clone(),
            GeneralConfig {
                consensus_config,
                da_config,
                network_config,
                blend_config: blend_configs[i].clone(),
                api_config,
                tracing_config,
                time_config,
                kms_config: kms_configs[i].clone(),
            },
        );
    }

    configured_hosts
}

fn create_providers(
    hosts: &[Host],
    consensus_configs: &[GeneralConsensusConfig],
    blend_configs: &[GeneralBlendConfig],
    da_configs: &[GeneralDaConfig],
) -> Vec<ProviderInfo> {
    let mut providers: Vec<_> = da_configs
        .iter()
        .enumerate()
        .map(|(i, da_conf)| ProviderInfo {
            service_type: ServiceType::DataAvailability,
            provider_sk: da_conf.signer.clone(),
            zk_sk: da_conf.secret_zk_key.clone(),
            locator: Locator(
                Multiaddr::from_str(&format!(
                    "/ip4/{}/udp/{}/quic-v1",
                    hosts[i].ip, hosts[i].da_network_port
                ))
                .unwrap(),
            ),
            note: consensus_configs[0].da_notes[i].clone(),
        })
        .collect();
    providers.extend(
        blend_configs
            .iter()
            .enumerate()
            .map(|(i, (blend_conf, secret_zk_key))| ProviderInfo {
                service_type: ServiceType::BlendNetwork,
                provider_sk: blend_conf.non_ephemeral_signing_key.clone().into(),
                zk_sk: secret_zk_key.clone(),
                locator: Locator(
                    Multiaddr::from_str(&format!(
                        "/ip4/{}/udp/{}/quic-v1",
                        hosts[i].ip, hosts[i].blend_port
                    ))
                    .unwrap(),
                ),
                note: consensus_configs[0].blend_notes[i].clone(),
            }),
    );

    providers
}

fn update_network_init_peers(hosts: &[Host]) -> Vec<Multiaddr> {
    hosts
        .iter()
        .map(|h| multiaddr(h.ip, h.network_port))
        .collect()
}

fn update_tracing_identifier(
    settings: TracingSettings,
    identifier: String,
) -> GeneralTracingConfig {
    GeneralTracingConfig {
        tracing_settings: TracingSettings {
            logger: match settings.logger {
                LoggerLayer::Loki(mut config) => {
                    config.host_identifier.clone_from(&identifier);
                    LoggerLayer::Loki(config)
                }
                other => other,
            },
            tracing: match settings.tracing {
                TracingLayer::Otlp(mut config) => {
                    config.service_name.clone_from(&identifier);
                    TracingLayer::Otlp(config)
                }
                other @ TracingLayer::None => other,
            },
            filter: settings.filter,
            metrics: match settings.metrics {
                MetricsLayer::Otlp(mut config) => {
                    config.host_identifier = identifier;
                    MetricsLayer::Otlp(config)
                }
                other @ MetricsLayer::None => other,
            },
            console: settings.console,
            level: settings.level,
        },
    }
}

#[cfg(test)]
mod cfgsync_tests {
    use std::{net::Ipv4Addr, str::FromStr as _, time::Duration};

    use nomos_da_network_core::swarm::{
        DAConnectionMonitorSettings, DAConnectionPolicySettings, ReplicationConfig,
    };
    use nomos_libp2p::{Multiaddr, Protocol};
    use nomos_tracing_service::{
        ConsoleLayer, FilterLayer, LoggerLayer, MetricsLayer, TracingLayer, TracingSettings,
    };
    use tests::topology::configs::da::DaParams;
    use tracing::Level;

    use super::{Host, HostKind, create_node_configs};

    #[test]
    fn basic_ip_list() {
        let hosts = (0..10)
            .map(|i| Host {
                kind: HostKind::Validator,
                ip: Ipv4Addr::from_str(&format!("10.1.1.{i}")).unwrap(),
                identifier: "node".into(),
                network_port: 3000,
                da_network_port: 4044,
                blend_port: 5000,
            })
            .collect();

        let configs = create_node_configs(
            &DaParams {
                subnetwork_size: 2,
                dispersal_factor: 1,
                num_samples: 1,
                num_subnets: 2,
                old_blobs_check_interval: Duration::from_secs(5),
                blobs_validity_duration: Duration::from_secs(u64::MAX),
                global_params_path: String::new(),
                policy_settings: DAConnectionPolicySettings::default(),
                monitor_settings: DAConnectionMonitorSettings::default(),
                balancer_interval: Duration::ZERO,
                redial_cooldown: Duration::ZERO,
                replication_settings: ReplicationConfig {
                    seen_message_cache_size: 0,
                    seen_message_ttl: Duration::ZERO,
                },
                subnets_refresh_interval: Duration::from_secs(1),
                retry_shares_limit: 1,
                retry_commitments_limit: 1,
            },
            &TracingSettings {
                logger: LoggerLayer::None,
                tracing: TracingLayer::None,
                filter: FilterLayer::None,
                metrics: MetricsLayer::None,
                console: ConsoleLayer::None,
                level: Level::DEBUG,
            },
            hosts,
        );

        for (host, config) in &configs {
            let network_port = config.network_config.backend.swarm.port;
            let da_network_port = extract_port(&config.da_config.listening_address);
            let blend_port = extract_port(&config.blend_config.0.core.backend.listening_address);

            assert_eq!(network_port, host.network_port);
            assert_eq!(da_network_port, host.da_network_port);
            assert_eq!(blend_port, host.blend_port);
        }
    }

    fn extract_port(multiaddr: &Multiaddr) -> u16 {
        multiaddr
            .iter()
            .find_map(|protocol| match protocol {
                Protocol::Udp(port) => Some(port),
                _ => None,
            })
            .unwrap()
    }
}
