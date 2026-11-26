use std::{collections::HashMap, net::SocketAddr};

use lb_key_management_system_service::{
    backend::preload::KeyId,
    keys::{Key, secured_key::SecuredKey as _},
};
use lb_node::{
    UserConfig,
    config::{
        ApiConfig, CryptarchiaConfig, MempoolConfig, PoWConfig, SdpConfig, StorageConfig,
        WalletConfig, api::serde::AxumBackendSettings, check_wallet_spending_keys,
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
pub fn create_node_user_config(config: GeneralConfig) -> Result<UserConfig, String> {
    config.consensus_config.validate_spending_keys()?;
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
    sdp_config.declaration = config.sdp_config.declaration.map(Into::into);

    let user = UserConfig {
        network: config.network_config,
        blend: config.blend_config.0,
        time: config.time_config,
        cryptarchia: cryptarchia_config,
        tracing: config.tracing_config.tracing_settings,
        api: api_config,
        storage: StorageConfig::default(),
        sdp: sdp_config,
        wallet: create_wallet_config(&config.consensus_config, &config.kms_config.backend.keys),
        // Mining defaults, auto-claim off: generated nodes mine and claim on
        // demand, naming the destination key on each claim request.
        pow: PoWConfig::default(),
        kms: config.kms_config,
        mempool: MempoolConfig::default(),
        state: StateConfig::default(),
    };
    check_wallet_spending_keys(&user).map_err(|error| error.to_string())?;
    Ok(user)
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
    kms_keys: &HashMap<KeyId, Key>,
) -> WalletConfig {
    let known_keys = kms_keys
        .iter()
        .filter_map(|(key_id, key)| match key {
            Key::Zk(sk) if sk.is_spending_key() => Some((key_id.clone(), sk.as_public_key())),
            _ => None,
        })
        .chain([
            (
                key_id_for_preload_backend(&Key::Zk(consensus.known_key.clone())),
                consensus.known_key.as_public_key(),
            ),
            (
                key_id_for_preload_backend(&Key::Zk(consensus.funding_sk.clone())),
                consensus.funding_sk.as_public_key(),
            ),
        ])
        .chain(consensus.other_keys.iter().map(|sk| {
            (
                key_id_for_preload_backend(&Key::Zk(sk.clone())),
                sk.as_public_key(),
            )
        }))
        .collect();

    let mut config = WalletConfig::with_required_values(WalletConfigRequiredValues {
        voucher_master_key_id: key_id_for_preload_backend(&Key::Zk(consensus.known_key.clone())),
    });
    config.known_keys = known_keys;
    config
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use lb_core::mantle::GenesisTime;
    use lb_groth16::fr_to_bytes;
    use lb_key_management_system_service::keys::ZkKey;

    use super::*;
    use crate::{create_general_configs_from_ids, kms::create_kms_configs, network::NetworkParams};

    #[expect(clippy::too_many_lines, reason = "Test function.")]
    #[test]
    fn generated_config_keeps_distinct_authorities_and_explicit_key_ids() {
        let (mut configs, _) = create_general_configs_from_ids(
            &[[1; 32]],
            &[3400],
            1,
            &NetworkParams::default(),
            Duration::from_secs(1),
            None,
            GenesisTime::new(1000),
        );
        let mut config = configs.remove(0);
        let ed25519 = Key::Ed25519(config.blend_config.1.clone());
        assert_eq!(
            key_id_for_preload_backend(&ed25519),
            hex::encode(config.blend_config.1.as_public_key().to_bytes())
        );
        let spending = ZkKey::new(123u64.into());
        let direct = ZkKey::from_direct_secret(spending.nullifier_key());
        let spending_key = Key::Zk(spending.clone());
        let direct_key = Key::Zk(direct.clone());
        let spending_id = key_id_for_preload_backend(&spending_key);
        let direct_id = key_id_for_preload_backend(&direct_key);
        assert_eq!(spending.to_public_key(), direct.to_public_key());
        assert_eq!(
            spending_id,
            hex::encode(fr_to_bytes(spending.as_public_key().as_fr()))
        );
        assert_eq!(direct_id, format!("zk-direct:{spending_id}"));

        for shared in [
            vec![spending_key.clone(), direct_key.clone()],
            vec![direct_key.clone(), spending_key.clone()],
        ] {
            let kms = create_kms_configs(
                &[config.blend_config.clone()],
                &[config.consensus_config.clone()],
                Some(&shared),
            )
            .remove(0);
            assert_eq!(kms.backend.keys.get(&spending_id), Some(&spending_key));
            assert_eq!(kms.backend.keys.get(&direct_id), Some(&direct_key));
        }
        config.kms_config = create_kms_configs(
            &[config.blend_config.clone()],
            &[config.consensus_config.clone()],
            Some(&[spending_key.clone(), direct_key.clone()]),
        )
        .remove(0);
        let alias_key = ZkKey::new(456u64.into());
        config
            .kms_config
            .backend
            .keys
            .insert("explicit-spending".into(), Key::Zk(alias_key.clone()));
        config
            .kms_config
            .backend
            .keys
            .insert("explicit-direct".into(), direct_key.clone());
        let consensus_id =
            key_id_for_preload_backend(&Key::Zk(config.consensus_config.known_key.clone()));
        let consensus_key = Key::Zk(config.consensus_config.known_key.clone());
        let mut invalid = config.clone();
        invalid.kms_config.backend.keys.remove(&consensus_id);
        invalid
            .kms_config
            .backend
            .keys
            .insert("consensus-alias".into(), consensus_key.clone());
        let error = create_node_user_config(invalid).unwrap_err();
        assert!(
            error.contains(&consensus_id) && error.contains("missing"),
            "{error}"
        );
        let mut invalid = config.clone();
        invalid
            .kms_config
            .backend
            .keys
            .insert(consensus_id.clone(), Key::Zk(ZkKey::new(987u64.into())));
        let error = create_node_user_config(invalid).unwrap_err();
        assert!(
            error.contains(&consensus_id) && error.contains("does not match"),
            "{error}"
        );
        let mut invalid = config.clone();
        invalid.consensus_config.other_keys.push(alias_key.clone());
        assert!(
            create_node_user_config(invalid)
                .unwrap_err()
                .contains("missing")
        );
        for field in ["known_key", "funding_sk", "other_keys"] {
            let mut invalid = config.clone();
            match field {
                "known_key" => invalid.consensus_config.known_key = direct.clone(),
                "funding_sk" => invalid.consensus_config.funding_sk = direct.clone(),
                _ => invalid.consensus_config.other_keys.push(direct.clone()),
            }
            invalid.kms_config.backend.keys.remove(&direct_id);
            assert!(
                create_node_user_config(invalid)
                    .unwrap_err()
                    .contains(field)
            );
        }
        let user = create_node_user_config(config).unwrap();
        assert_eq!(user.wallet.voucher_master_key_id, consensus_id);
        assert_eq!(
            user.kms.backend.keys.get(&consensus_id),
            Some(&consensus_key)
        );
        assert!(user.wallet.known_keys.contains_key(&consensus_id));
        assert_eq!(user.kms.backend.keys.get(&spending_id), Some(&spending_key));
        assert_eq!(user.kms.backend.keys.get(&direct_id), Some(&direct_key));
        assert_eq!(
            user.wallet.known_keys.get(&spending_id),
            Some(&spending.to_public_key())
        );
        assert_eq!(
            user.wallet.known_keys.get("explicit-spending"),
            Some(&alias_key.to_public_key())
        );
        assert!(!user.wallet.known_keys.contains_key(&direct_id));
        assert!(!user.wallet.known_keys.contains_key("explicit-direct"));
    }
}
