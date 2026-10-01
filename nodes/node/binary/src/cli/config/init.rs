use std::{collections::HashMap, path::Path};

use color_eyre::eyre::Result;
use lb_core::mantle::Value;
use lb_groth16::fr_to_bytes;
use lb_key_management_system_keys::{hd::Mnemonic, keys::ZkPublicKey};
use lb_key_management_system_service::backend::preload::KeyId;
use lb_pow_service::ClaimTarget;
use libp2p::{Multiaddr, PeerId};
use rand::rngs::OsRng;
use thiserror::Error;

use crate::{
    NetworkArgs, UserConfig,
    cli::{
        InitArgs,
        config::keystore::{KeyTitle, Keystore, KeystoreError},
    },
    config::{
        ApiConfig, BlendArgs, CryptarchiaArgs, CryptarchiaConfig, KmsConfig, PoWConfig, SdpConfig,
        StateConfig, StorageConfig, TimeConfig, TracingConfig, WalletConfig,
        blend::serde::{Config as BlendConfig, RequiredValues as BlendConfigRequiredValues},
        network::serde::Config as NetworkConfig,
        update_api, update_blend, update_network, update_state, update_tracing,
    },
};

#[derive(Error, Debug)]
enum InitError {
    #[error("User configuration file exists. Use `update` command.")]
    UserFileExists,

    #[error("Keystore file exists. Use `update` command.")]
    KeystoreFileExists,
}

pub fn run(args: InitArgs) -> Result<()> {
    let user_config_path = args.output.clone();
    let keystore_path = args.keystore.clone().unwrap_or_else(|| {
        user_config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("keystore.yaml")
    });

    if user_config_path.exists() && !args.overwrite {
        return Err(InitError::UserFileExists.into());
    }

    if keystore_path.exists() && !args.overwrite {
        return Err(InitError::KeystoreFileExists.into());
    }

    let keystore = Keystore::new(
        args.mnemonic
            .clone()
            .unwrap_or_else(|| Mnemonic::generate(&mut OsRng)),
        args.mnemonic_passphrase.clone(),
    );
    let user_config = build_user_config(&keystore, args)?;

    let user_config_yaml = serde_yaml::to_string(&user_config)?;
    std::fs::write(&user_config_path, &user_config_yaml)?;

    let keystore_yaml = serde_yaml::to_string(&keystore)?;
    std::fs::write(&keystore_path, &keystore_yaml)?;

    println!(
        "Stake address, which the node never spends from: {}",
        public_key_hex(&keystore.stake_public_key())
    );
    println!(
        "Address that pays the transaction fees and receives the PoW rewards: {}",
        public_key_hex(&keystore.pow_claim_public_key())
    );

    Ok(())
}

/// # Errors
///
/// Returns [`KeystoreError`] if the keystore lacks a predefined key or holds
/// it with the wrong key type.
pub fn build_user_config(keystore: &Keystore, args: InitArgs) -> Result<UserConfig, KeystoreError> {
    let InitArgs {
        log: log_args,
        network: network_args,
        blend: blend_args,
        cryptarchia: cryptarchia_args,
        api: api_args,
        state: state_args,
        storage_path: storage_args,
        ..
    } = args;

    let time_config = TimeConfig::default();

    let mut storage_config = StorageConfig::default();
    if let Some(storage_path) = storage_args {
        storage_config.backend.folder_name = storage_path.to_string_lossy().into_owned();
    }

    let mut state_config = StateConfig::default();
    update_state(&mut state_config, state_args);

    let mut api_config = ApiConfig::default();
    update_api(&mut api_config, api_args);

    let mut tracing_config = TracingConfig::default();
    update_tracing(&mut tracing_config, log_args).expect("Cli tracing params can be parsed");

    let initial_peers = network_args.initial_peers.clone();
    let network_config = build_network_config(keystore, network_args)?;

    let blend_config = build_blend_config(keystore, blend_args)?;

    let cryptarchia_config = build_cryptarchia_config(initial_peers, cryptarchia_args);

    let sdp_config = SdpConfig::default();

    let wallet_config = build_wallet_config(keystore);

    let kms_config = build_kms_config(keystore);

    let pow_config = build_pow_config(keystore);

    Ok(UserConfig {
        network: network_config,
        blend: blend_config,
        cryptarchia: cryptarchia_config,
        time: time_config,
        sdp: sdp_config,
        api: api_config,
        storage: storage_config,
        kms: kms_config,
        wallet: wallet_config,
        pow: pow_config,
        tracing: tracing_config,
        state: state_config,
    })
}

fn build_network_config(
    keystore: &Keystore,
    network_args: NetworkArgs,
) -> Result<NetworkConfig, KeystoreError> {
    let (_, unsecured_key) = keystore.get_ed25519_static_key(KeyTitle::NETWORK_SWARM)?;
    let mut network_secret_key_bytes: [u8; 32] = *unsecured_key.as_bytes();

    let mut network_config = NetworkConfig::default();
    network_config.backend.swarm.node_key =
        lb_libp2p::ed25519::SecretKey::try_from_bytes(&mut network_secret_key_bytes)
            .expect("Valid default secret key structure");
    update_network(&mut network_config, network_args)
        .expect("Network configuration should update from cli args");

    Ok(network_config)
}

fn build_blend_config(
    keystore: &Keystore,
    blend_args: BlendArgs,
) -> Result<BlendConfig, KeystoreError> {
    let (blend_signing_key_id, _) = keystore
        .get_static_key(KeyTitle::BLEND_SIGNING)
        .ok_or_else(|| KeystoreError::NotFound(KeyTitle::BLEND_SIGNING.into()))?;
    let (blend_zk_key_id, _) = keystore
        .get_static_key(KeyTitle::BLEND_ZK)
        .ok_or_else(|| KeystoreError::NotFound(KeyTitle::BLEND_ZK.into()))?;
    let mut blend_config = BlendConfig::with_required_values(BlendConfigRequiredValues {
        non_ephemeral_signing_key_id: blend_signing_key_id.into(),
        secret_key_kms_id: blend_zk_key_id.into(),
    });
    update_blend(&mut blend_config, blend_args);

    Ok(blend_config)
}

fn build_cryptarchia_config(
    initial_peers: Option<Vec<Multiaddr>>,
    cryptarchia_args: CryptarchiaArgs,
) -> CryptarchiaConfig {
    let mut cryptarchia_config = CryptarchiaConfig::default();
    if !cryptarchia_args.skip_ibd
        && let Some(initial_peers) = initial_peers
    {
        cryptarchia_config.network.bootstrap.ibd.peers = initial_peers
            .iter()
            .filter_map(|addr| match addr.iter().last() {
                Some(lb_libp2p::Protocol::P2p(bytes)) => PeerId::from_multihash(bytes.into()).ok(),
                _ => None,
            })
            .collect();
    }
    cryptarchia_config
}

fn build_kms_config(keystore: &Keystore) -> KmsConfig {
    KmsConfig {
        backend: keystore.kms_backend_settings(),
    }
}

/// Mining defaults, with auto-claim paying the `PoWClaim` key without a cap,
/// so a generated node claims its mined rewards unattended once mining is
/// started.
fn build_pow_config(keystore: &Keystore) -> PoWConfig {
    let mut pow_config = PoWConfig::default();
    pow_config.auto_claim.targets = vec![ClaimTarget {
        public_key: keystore.pow_claim_public_key(),
        threshold: Value::MAX,
    }];
    pow_config
}

fn build_wallet_config(keystore: &Keystore) -> WalletConfig {
    WalletConfig {
        static_keys: static_zk_public_keys(keystore),
        // Include the stake key to the unspendable keys.
        // We don't want the stake key to be used for spending becuase its output
        // should be aged again to participate in leadership.
        unspendable_keys: keystore.unspendable_public_keys(),
        ..WalletConfig::default()
    }
}

/// The public keys of the ZK static keys of the keystore, by id
#[must_use]
pub fn static_zk_public_keys(keystore: &Keystore) -> HashMap<KeyId, ZkPublicKey> {
    keystore
        .get_all_zk_static_key()
        .map(|(key_id, key)| (key_id, key.to_public_key()))
        .collect()
}

fn public_key_hex(public_key: &ZkPublicKey) -> String {
    hex::encode(fr_to_bytes(public_key.as_fr()))
}

#[cfg(test)]
mod tests {
    use lb_key_management_system_service::{backend::hd_and_preload, keys::Key};
    use lb_wallet_service::hd::{FUNDING_RECEIVE_INDEX, STAKE_RECEIVE_INDEX};

    use super::*;

    const MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn user_config_holds_three_static_keys_and_uses_hd_keys() {
        let keystore = Keystore::new(MNEMONIC.parse().unwrap(), None);

        let config = build_user_config(&keystore, InitArgs::default()).unwrap();

        assert_eq!(config.kms.backend.mnemonic, MNEMONIC.parse().unwrap());
        assert_eq!(config.kms.backend.static_keys.len(), 3);
        let (blend_zk_key_id, blend_zk_key) = config.blend_zk_key().unwrap();
        let hd_and_preload::KeyId::Static(blend_zk_key_id) = blend_zk_key_id else {
            panic!("Blend ZK key is a static key");
        };
        assert_eq!(
            config.wallet.static_keys,
            [(blend_zk_key_id, blend_zk_key)].into()
        );
        assert_eq!(
            config.wallet.unspendable_keys,
            [keystore.receive_public_key(STAKE_RECEIVE_INDEX)].into()
        );
        assert_eq!(
            config
                .pow
                .auto_claim
                .targets
                .iter()
                .map(|target| target.public_key)
                .collect::<Vec<_>>(),
            [keystore.receive_public_key(FUNDING_RECEIVE_INDEX)]
        );
        config.blend_provider_id().unwrap();
    }

    #[test]
    fn migrated_node_keeps_its_legacy_stake_unspendable() {
        let mut keystore = Keystore::new(MNEMONIC.parse().unwrap(), None);
        let (_, stake) = keystore.generate_zk_static_key(KeyTitle::LEGACY_STAKE);
        keystore.generate_zk_static_key(KeyTitle::LEGACY_POW_CLAIM);

        let config = build_user_config(&keystore, InitArgs::default()).unwrap();

        assert_eq!(
            config.wallet.unspendable_keys,
            [
                keystore.receive_public_key(STAKE_RECEIVE_INDEX),
                stake.to_public_key()
            ]
            .into()
        );
        assert_eq!(
            config.pow.auto_claim.targets[0].public_key,
            keystore.receive_public_key(FUNDING_RECEIVE_INDEX)
        );
        let stake = Key::Zk(stake.into());
        assert!(
            config
                .kms
                .backend
                .static_keys
                .values()
                .any(|key| key == &stake)
        );
    }
}
