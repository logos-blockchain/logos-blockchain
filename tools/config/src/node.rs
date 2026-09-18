use std::{collections::BTreeSet, net::SocketAddr};

use lb_key_management_system_service::{backend::preload::KeyId, keys::Key};
use lb_node::{
    UserConfig,
    config::{
        ApiConfig, CryptarchiaConfig, PoWConfig, SdpConfig, StorageConfig, WalletConfig,
        api::serde::AxumBackendSettings,
        cryptarchia::serde::RequiredValues as CryptarchiaConfigRequiredValues,
        kms::serde::{KeyEntry, PreloadKmsBackendSettings},
        sdp::serde::RequiredValues as SdpConfigRequiredValues,
        state::Config as StateConfig,
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
    let funding_key_id =
        key_id_for_preload_backend(&Key::Zk(config.consensus_config.funding_sk.clone()));
    let mut cryptarchia_config =
        CryptarchiaConfig::with_required_values(CryptarchiaConfigRequiredValues {
            funding_key_id: funding_key_id.clone(),
        });
    cryptarchia_config
        .service
        .bootstrap
        .prolonged_bootstrap_period = config.consensus_config.prolonged_bootstrap_period;

    let mut sdp_config =
        SdpConfig::with_required_values(SdpConfigRequiredValues { funding_key_id });
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

fn create_wallet_config(
    consensus: &GeneralConsensusConfig,
    kms: &PreloadKmsBackendSettings,
) -> WalletConfig {
    // Every ZK key in the KMS, in a stable order.
    let known_keys: BTreeSet<KeyId> = kms
        .keys
        .iter()
        .filter(|(_, entry)| !matches!(entry, KeyEntry::Ed25519(_)))
        .map(|(key_id, _)| key_id.clone())
        .collect();

    let mut config = WalletConfig::with_required_values(WalletConfigRequiredValues {
        voucher_master_key_id: key_id_for_preload_backend(&Key::Zk(consensus.known_key.clone())),
    });
    config.known_keys = known_keys.into_iter().collect();
    config
}
