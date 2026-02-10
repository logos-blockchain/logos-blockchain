mod consensus;
mod kms;

use std::{collections::HashMap, net::Ipv4Addr, str::FromStr as _};

use lb_core::{
    mantle::{GenesisTx as _, genesis_tx::GenesisTx},
    sdp::{Locator, ServiceType},
};
use lb_key_management_system_service::keys::ZkKey;
use lb_libp2p::{Multiaddr, multiaddr};
use lb_node::config::{TracingConfig, network::serde as network, tracing::serde as tracing};
use lb_tests::topology::configs::{
    GeneralConfig,
    api::GeneralApiConfig,
    blend::{GeneralBlendConfig, create_blend_configs},
    consensus::{
        GeneralConsensusConfig, ProviderInfo, SHORT_PROLONGED_BOOTSTRAP_PERIOD,
        create_genesis_tx_with_declarations,
    },
    network::{NetworkParams, create_network_configs},
    time::default_time_config,
    tracing::GeneralTracingConfig,
};
use rand::{Rng as _, thread_rng};

use crate::{
    FaucetSettings, Host,
    config::{consensus::create_consensus_configs, kms::create_kms_configs},
};

type FaucetNotes = Vec<ZkKey>;

#[must_use]
pub fn create_node_configs(
    faucet_settings: &FaucetSettings,
    tracing_settings: &TracingConfig,
    hosts: Vec<Host>,
) -> (HashMap<Host, GeneralConfig>, GenesisTx) {
    let mut ids = Vec::with_capacity(hosts.len());

    for host in &hosts {
        let mut id_bytes = [0u8; 32];
        let identifier = host.identifier.as_bytes();
        let len = std::cmp::min(identifier.len(), 32);

        id_bytes[..len].copy_from_slice(&identifier[..len]);
        ids.push(id_bytes);
    }

    // Clippy in 1.93.0:
    // > an unstable sort typically performs faster without any
    // > observable difference for this data type. [stable_sort_primitive]
    ids.sort_unstable();

    let (consensus_configs, faucet_note_keys, genesis_tx) =
        create_consensus_configs(&ids, SHORT_PROLONGED_BOOTSTRAP_PERIOD, faucet_settings);
    let network_configs = create_network_configs(&ids, &NetworkParams::default());
    let blend_configs = create_blend_configs(
        &ids,
        hosts
            .iter()
            .map(|h| h.blend_port)
            .collect::<Vec<_>>()
            .as_slice(),
    );
    let api_configs = hosts
        .iter()
        .map(|host| GeneralApiConfig {
            address: format!("0.0.0.0:{}", host.api_port).parse().unwrap(),
            testing_http_address: format!("0.0.0.0:{}", host.api_port).parse().unwrap(),
        })
        .collect::<Vec<_>>();
    let mut configured_hosts = HashMap::new();

    // Rebuild network address lists.
    let host_network_init_peers = update_network_init_peers(&hosts);

    let providers = create_providers(&hosts, &consensus_configs, &blend_configs);

    // Update genesis TX to contain Blend providers.
    let ledger_tx = genesis_tx.mantle_tx().ledger_tx.clone();
    let genesis_tx_with_declarations = create_genesis_tx_with_declarations(ledger_tx, providers);

    // Set Blend keys in KMS of each node config.
    let kms_configs = create_kms_configs(&blend_configs, &consensus_configs, &faucet_note_keys);

    for (i, host) in hosts.into_iter().enumerate() {
        let consensus_config = consensus_configs[i].clone();
        let api_config = api_configs[i].clone();

        // Libp2p network config.
        let mut network_config = network_configs[i].clone();
        network_config.backend.swarm.host = Ipv4Addr::from_str("0.0.0.0").unwrap();
        network_config.backend.swarm.port = host.network_port;
        network_config
            .backend
            .initial_peers
            .clone_from(&host_network_init_peers);
        network_config.backend.swarm.nat = network::nat::Config::Static {
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
                network_config,
                blend_config: blend_configs[i].clone(),
                api_config,
                tracing_config,
                time_config,
                kms_config: kms_configs[i].clone(),
            },
        );
    }

    (configured_hosts, genesis_tx_with_declarations)
}

#[must_use]
pub fn create_node_config_from_template(
    tracing_settings: &TracingConfig,
    new_host: &Host,
    template: &GeneralConfig,
) -> GeneralConfig {
    let mut id = [0u8; 32];
    thread_rng().fill(&mut id);
    let ids = vec![id];

    let (consensus_configs, _, _) = create_consensus_configs(
        &ids,
        SHORT_PROLONGED_BOOTSTRAP_PERIOD,
        &FaucetSettings::default(),
    );
    let network_configs = create_network_configs(&ids, &NetworkParams::default());
    let blend_configs = create_blend_configs(&ids, &[new_host.blend_port]);

    let kms_configs = create_kms_configs(&blend_configs, &consensus_configs, &[]);

    let mut network_config = network_configs[0].clone();
    network_config.backend.swarm.host = Ipv4Addr::from_str("0.0.0.0").unwrap();
    network_config.backend.swarm.port = new_host.network_port;

    network_config
        .backend
        .initial_peers
        .clone_from(&template.network_config.backend.initial_peers);

    network_config.backend.swarm.nat = network::nat::Config::Static {
        external_address: Multiaddr::from_str(&format!(
            "/ip4/{}/udp/{}/quic-v1",
            new_host.ip, new_host.network_port
        ))
        .unwrap(),
    };

    GeneralConfig {
        consensus_config: consensus_configs[0].clone(),
        network_config,
        blend_config: blend_configs[0].clone(),
        api_config: GeneralApiConfig {
            address: format!("0.0.0.0:{}", new_host.api_port).parse().unwrap(),
            testing_http_address: format!("0.0.0.0:{}", new_host.api_port).parse().unwrap(),
        },
        tracing_config: update_tracing_identifier(
            tracing_settings.clone(),
            new_host.identifier.clone(),
        ),
        time_config: template.time_config.clone(),
        kms_config: kms_configs[0].clone(),
    }
}

fn create_providers(
    hosts: &[Host],
    consensus_configs: &[GeneralConsensusConfig],
    blend_configs: &[GeneralBlendConfig],
) -> Vec<ProviderInfo> {
    blend_configs
        .iter()
        .enumerate()
        .map(|(i, (_, private_key, secret_zk_key))| ProviderInfo {
            service_type: ServiceType::BlendNetwork,
            provider_sk: private_key.clone(),
            zk_sk: secret_zk_key.clone(),
            locator: Locator(
                Multiaddr::from_str(&format!(
                    "/ip4/{}/udp/{}/quic-v1",
                    hosts[i].ip, hosts[i].blend_port
                ))
                .unwrap(),
            ),
            note: consensus_configs[0].blend_notes[i].clone(),
        })
        .collect()
}

fn update_network_init_peers(hosts: &[Host]) -> Vec<Multiaddr> {
    hosts
        .iter()
        .map(|h| multiaddr(h.ip, h.network_port))
        .collect()
}

fn update_tracing_identifier(settings: TracingConfig, identifier: String) -> GeneralTracingConfig {
    GeneralTracingConfig {
        tracing_settings: TracingConfig {
            logger: match settings.logger {
                tracing::logger::Layer::Loki(mut config) => {
                    config.host_identifier.clone_from(&identifier);
                    tracing::logger::Layer::Loki(config)
                }
                other => other,
            },
            tracing: match settings.tracing {
                tracing::tracing::Layer::Otlp(mut config) => {
                    config.service_name.clone_from(&identifier);
                    tracing::tracing::Layer::Otlp(config)
                }
                other @ tracing::tracing::Layer::None => other,
            },
            filter: settings.filter,
            metrics: match settings.metrics {
                tracing::metrics::Layer::Otlp(mut config) => {
                    config.host_identifier = identifier;
                    tracing::metrics::Layer::Otlp(config)
                }
                other @ tracing::metrics::Layer::None => other,
            },
            console: settings.console,
            level: settings.level,
        },
    }
}

#[cfg(test)]
mod cfgsync_tests {
    use std::{net::Ipv4Addr, str::FromStr as _};

    use ::tracing::Level;
    use lb_libp2p::{Multiaddr, Protocol};
    use lb_node::config::{TracingConfig, tracing::serde as tracing};

    use super::{Host, create_node_configs};
    use crate::{FaucetSettings, config::create_node_config_from_template};

    fn extract_port(multiaddr: &Multiaddr) -> u16 {
        multiaddr
            .iter()
            .find_map(|protocol| match protocol {
                Protocol::Udp(port) => Some(port),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn basic_ip_list() {
        let hosts = (0..10)
            .map(|i| Host {
                ip: Ipv4Addr::from_str(&format!("10.1.1.{i}")).unwrap(),
                identifier: "node".into(),
                network_port: 3000,
                blend_port: 5000,
                api_port: 8000,
            })
            .collect();

        let (configs, _) =
            create_node_configs(&FaucetSettings::default(), &TracingConfig::none(), hosts);

        for (host, config) in &configs {
            let network_port = config.network_config.backend.swarm.port;
            let blend_port = extract_port(&config.blend_config.0.core.backend.listening_address);

            assert_eq!(network_port, host.network_port);
            assert_eq!(blend_port, host.blend_port);
        }
    }

    #[test]
    fn append_node() {
        let tracing = TracingConfig {
            logger: tracing::logger::Layer::None,
            tracing: tracing::tracing::Layer::None,
            filter: tracing::filter::Layer::None,
            metrics: tracing::metrics::Layer::None,
            console: tracing::console::Layer::None,
            level: Level::DEBUG,
        };

        let init_host = Host {
            ip: Ipv4Addr::LOCALHOST,
            identifier: "init".into(),
            ..Default::default()
        };
        let (init_configs, _) = create_node_configs(
            &FaucetSettings::default(),
            &tracing,
            vec![init_host.clone()],
        );
        let template = init_configs.get(&init_host).unwrap();

        let new_host = Host {
            ip: Ipv4Addr::new(127, 0, 0, 2),
            identifier: "joiner".into(),
            network_port: 4000,
            blend_port: 5000,
            api_port: 9000,
        };

        let appended_config = create_node_config_from_template(&tracing, &new_host, template);

        assert_eq!(
            appended_config.network_config.backend.initial_peers,
            template.network_config.backend.initial_peers,
            "Appended node should inherit the initial peer list for discovery"
        );

        assert_eq!(appended_config.network_config.backend.swarm.port, 4000);
        assert_eq!(appended_config.api_config.address.port(), 9000);
    }
}
