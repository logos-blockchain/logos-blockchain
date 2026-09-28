use std::net::SocketAddr;

use lb_node::{
    UserConfig,
    config::{
        ApiConfig, CryptarchiaConfig, PoWConfig, SdpConfig, StorageConfig, WalletConfig,
        api::serde::AxumBackendSettings, kms::serde::PreloadKmsBackendSettings,
        state::Config as StateConfig,
    },
};

use crate::GeneralConfig;

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
        wallet: create_wallet_config(&config.kms_config.backend),
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

fn create_wallet_config(kms: &PreloadKmsBackendSettings) -> WalletConfig {
    // Every key of the KMS, in a stable order.
    let mut known_keys = kms.keys.keys().cloned().collect::<Vec<_>>();
    known_keys.sort();

    WalletConfig {
        known_keys,
        ..WalletConfig::default()
    }
}
