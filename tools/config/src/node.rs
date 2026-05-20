use std::{collections::HashMap, net::SocketAddr};

use lb_key_management_system_service::{
    backend::preload::KeyId,
    keys::{Key, secured_key::SecuredKey as _},
};
use lb_node::{
    UserConfig,
    config::{
        ApiConfig, CryptarchiaConfig, SdpConfig, StorageConfig, WalletConfig,
        api::serde::AxumBackendSettings,
        cryptarchia::serde::RequiredValues as CryptarchiaConfigRequiredValues,
        sdp::serde::RequiredValues as SdpConfigRequiredValues, state::Config as StateConfig,
        wallet::serde::RequiredValues as WalletConfigRequiredValues,
    },
};

use crate::{GeneralConfig, consensus::GeneralConsensusConfig, kms::key_id_for_preload_backend};

/// Builds the node user configuration from generated deployment material.
///
/// `GeneralConfig` is the neutral config-generation shape used by cfgsync,
/// test deployments, and release tooling. This conversion is the boundary
/// where that generated material becomes the node binary's `UserConfig`.
#[must_use]
pub fn create_node_user_config(config: GeneralConfig) -> UserConfig {
    let api_config = create_api_config(&config);
    let mut cryptarchia_config =
        CryptarchiaConfig::with_required_values(CryptarchiaConfigRequiredValues {
            funding_pk: config.consensus_config.funding_pk,
        });
    cryptarchia_config
        .service
        .bootstrap
        .prolonged_bootstrap_period = config.consensus_config.prolonged_bootstrap_period;

    let mut sdp_config = SdpConfig::with_required_values(SdpConfigRequiredValues {
        funding_pk: config.consensus_config.funding_sk.as_public_key(),
    });
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
        wallet: create_wallet_config(&config.consensus_config, &config.kms_config.backend.keys),
        kms: config.kms_config,
        state: StateConfig::default(),
    }
}

fn create_api_config(config: &GeneralConfig) -> ApiConfig {
    ApiConfig {
        backend: create_axum_backend_settings(config.api_config.address),
        #[cfg(feature = "testing")]
        testing: create_axum_backend_settings(config.api_config.testing_http_address),
    }
}

fn create_axum_backend_settings(listen_address: SocketAddr) -> AxumBackendSettings {
    AxumBackendSettings {
        listen_address,
        max_concurrent_requests: 1000,
        ..Default::default()
    }
}

fn create_wallet_config(
    consensus: &GeneralConsensusConfig,
    kms_keys: &HashMap<KeyId, Key>,
) -> WalletConfig {
    let known_keys = [
        (
            key_id_for_preload_backend(&Key::Zk(consensus.known_key.clone())),
            consensus.known_key.as_public_key(),
        ),
        (
            key_id_for_preload_backend(&Key::Zk(consensus.funding_sk.clone())),
            consensus.funding_sk.as_public_key(),
        ),
    ]
    .into_iter()
    .chain(consensus.other_keys.iter().map(|sk| {
        (
            key_id_for_preload_backend(&Key::Zk(sk.clone())),
            sk.as_public_key(),
        )
    }))
    .chain(kms_keys.values().filter_map(|key| match key {
        Key::Zk(sk) => Some((
            key_id_for_preload_backend(&Key::Zk(sk.clone())),
            sk.as_public_key(),
        )),
        Key::Ed25519(_) => None,
    }))
    .collect();

    let mut config = WalletConfig::with_required_values(WalletConfigRequiredValues {
        voucher_master_key_id: key_id_for_preload_backend(&Key::Zk(consensus.known_key.clone())),
    });
    config.known_keys = known_keys;
    config
}
