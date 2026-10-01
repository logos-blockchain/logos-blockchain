use std::net::SocketAddr;

use lb_key_management_system_service::keys::Key;
use lb_node::{
    UserConfig,
    config::{
        ApiConfig, CryptarchiaConfig, PoWConfig, SdpConfig, StorageConfig, WalletConfig,
        api::serde::AxumBackendSettings, kms::serde::KmsBackendSettings,
        state::Config as StateConfig,
    },
};

use crate::{GeneralConfig, consensus::GeneralConsensusConfig};

/// Builds the node user configuration from generated deployment material.
///
/// `GeneralConfig` is the neutral config-generation shape used by cfgsync,
/// test deployments, and release tooling. This conversion is the boundary
/// where that generated material becomes the node binary's `UserConfig`.
#[must_use]
pub fn create_node_user_config(config: GeneralConfig) -> UserConfig {
    let api_config = create_api_config(&config);
    let mut cryptarchia_config = CryptarchiaConfig::default();
    cryptarchia_config
        .service
        .bootstrap
        .prolonged_bootstrap_period = config.consensus_config.prolonged_bootstrap_period;

    let mut sdp_config = SdpConfig::default();
    sdp_config.declaration_id = config.sdp_config.declaration_id;

    UserConfig {
        network: config.network_config,
        blend: config.blend_config.0,
        time: config.time_config,
        cryptarchia: cryptarchia_config,
        tracing: config.tracing_config.tracing_settings,
        api: api_config,
        storage: StorageConfig::default(),
        sdp: sdp_config,
        wallet: create_wallet_config(&config.consensus_config, &config.kms_config.backend),
        // Mining defaults, auto-claim off: generated nodes mine and claim on
        // demand, naming the destination key on each claim request.
        pow: PoWConfig::default(),
        kms: config.kms_config,
        state: StateConfig::default(),
    }
}

fn create_api_config(config: &GeneralConfig) -> ApiConfig {
    ApiConfig {
        backend: create_axum_backend_settings(config.api_config.address),
    }
}

fn create_axum_backend_settings(listen_address: SocketAddr) -> AxumBackendSettings {
    AxumBackendSettings {
        listen_address,
        max_concurrent_requests: 1000,
        ..Default::default()
    }
}

/// The wallet of a generated node tracks the ZK static keys of its KMS, and
/// its HD keys. Its stake is never spent to fund transactions.
#[must_use]
pub fn create_wallet_config(
    consensus: &GeneralConsensusConfig,
    kms: &KmsBackendSettings,
) -> WalletConfig {
    WalletConfig {
        static_keys: kms
            .static_keys
            .iter()
            .filter_map(|(key_id, key)| match key {
                Key::Zk(sk) => Some((key_id.clone(), sk.to_public_key())),
                Key::Ed25519(_) => None,
            })
            .collect(),
        unspendable_keys: [consensus.known_key.to_public_key()].into(),
        ..WalletConfig::default()
    }
}
