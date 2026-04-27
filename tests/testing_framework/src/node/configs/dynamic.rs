use lb_config::kms::key_id_for_preload_backend;
use lb_key_management_system_service::keys::Key;
use lb_libp2p::Multiaddr;
use lb_node::config::KmsConfig;
use thiserror::Error;

use super::node_configs::{
    self, GeneralConfig as Config,
    blend::GeneralBlendConfig,
    consensus::{GeneralConsensusConfig, SHORT_PROLONGED_BOOTSTRAP_PERIOD},
    network::NetworkParams,
    sdp::GeneralSdpConfig,
    time::GeneralTimeConfig,
};

#[derive(Debug, Error)]
pub enum DynamicConfigBuildError {
    #[error("config generation requires at least one consensus config")]
    Consensus,
    #[error("config generation requires at least one blend config")]
    Blend,
    #[error("config generation requires at least one network config")]
    Network,
    #[error("config generation requires at least one api config")]
    Api,
    #[error("config generation requires at least one tracing config")]
    Tracing,
}

pub fn create_node_config_for_node(
    id: [u8; 32],
    network_port: u16,
    initial_peers: Vec<Multiaddr>,
    blend_port: u16,
    base_consensus: &GeneralConsensusConfig,
    time_config: &GeneralTimeConfig,
    test_context: Option<&str>,
) -> Result<Config, DynamicConfigBuildError> {
    let consensus_config = build_consensus_config_for_node(id, base_consensus, test_context)?;

    let blend_config = node_configs::blend::create_blend_configs(&[id], &[blend_port])
        .into_iter()
        .next()
        .ok_or(DynamicConfigBuildError::Blend)?;

    let mut network_config =
        node_configs::network::create_network_configs(&[id], &NetworkParams::default())
            .into_iter()
            .next()
            .ok_or(DynamicConfigBuildError::Network)?;
    network_config.backend.initial_peers = initial_peers;
    network_config.backend.swarm.port = network_port;

    let api_config = node_configs::api::create_api_configs(&[id])
        .into_iter()
        .next()
        .ok_or(DynamicConfigBuildError::Api)?;

    let tracing_config = node_configs::tracing::create_tracing_configs(&[id])
        .into_iter()
        .next()
        .ok_or(DynamicConfigBuildError::Tracing)?;

    let kms_config = build_kms_config_for_node(&blend_config, &consensus_config);

    Ok(Config {
        consensus_config,
        network_config,
        blend_config,
        api_config,
        tracing_config,
        time_config: time_config.clone(),
        kms_config,
        sdp_config: GeneralSdpConfig {
            declaration_id: None,
        },
    })
}

fn build_consensus_config_for_node(
    id: [u8; 32],
    base: &GeneralConsensusConfig,
    test_context: Option<&str>,
) -> Result<GeneralConsensusConfig, DynamicConfigBuildError> {
    let (mut configs, _) = node_configs::consensus::create_consensus_configs(
        &[id],
        SHORT_PROLONGED_BOOTSTRAP_PERIOD,
        test_context,
    );
    let mut config = configs.pop().ok_or(DynamicConfigBuildError::Consensus)?;
    config.blend_note.clone_from(&base.blend_note);

    Ok(config)
}

fn build_kms_config_for_node(
    blend_config: &GeneralBlendConfig,
    consensus_config: &GeneralConsensusConfig,
) -> KmsConfig {
    let (blend_conf, private_key, secret_zk_key) = blend_config;

    KmsConfig {
        backend: lb_node::config::kms::serde::PreloadKmsBackendSettings {
            keys: [
                (
                    blend_conf.non_ephemeral_signing_key_id.clone(),
                    Key::Ed25519(private_key.clone()),
                ),
                (
                    blend_conf.core.zk.secret_key_kms_id.clone(),
                    Key::Zk(secret_zk_key.clone()),
                ),
                (
                    key_id_for_preload_backend(&Key::Zk(consensus_config.blend_note.sk.clone())),
                    Key::Zk(consensus_config.blend_note.sk.clone()),
                ),
                (
                    key_id_for_preload_backend(&Key::Zk(consensus_config.known_key.clone())),
                    Key::Zk(consensus_config.known_key.clone()),
                ),
                (
                    key_id_for_preload_backend(&Key::Zk(consensus_config.funding_sk.clone())),
                    Key::Zk(consensus_config.funding_sk.clone()),
                ),
            ]
            .into(),
        },
    }
}
