use std::path::Path;

use color_eyre::eyre::Result;
use libp2p::{Multiaddr, PeerId};
use thiserror::Error;

use crate::{
    NetworkArgs, UserConfig,
    cli::{
        InitArgs,
        config::keystore::{KeyTitle, Keystore},
    },
    config::{
        ApiConfig, BlendArgs, CryptarchiaArgs, CryptarchiaConfig, KmsConfig, SdpArgs, SdpConfig,
        StateConfig, StorageConfig, TimeConfig, TracingConfig, WalletConfig,
        blend::serde::{Config as BlendConfig, RequiredValues as BlendConfigRequiredValues},
        cryptarchia::serde::RequiredValues as CryptarchiaConfigRequiredValues,
        network::serde::Config as NetworkConfig,
        sdp::serde::RequiredValues as SdpConfigRequiredValues,
        update_api, update_blend, update_cryptarchia, update_network, update_sdp, update_state,
        update_tracing,
        wallet::serde::RequiredValues as WalletConfigRequiredValues,
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

    if user_config_path.exists() {
        return Err(InitError::UserFileExists.into());
    }

    if keystore_path.exists() {
        return Err(InitError::KeystoreFileExists.into());
    }

    let keystore = Keystore::default();
    let user_config = build_user_config(&keystore, args);

    let user_config_yaml = serde_yaml::to_string(&user_config)?;
    std::fs::write(&user_config_path, &user_config_yaml)?;

    let keystore_yaml = serde_yaml::to_string(&keystore)?;
    std::fs::write(&keystore_path, &keystore_yaml)?;

    Ok(())
}

#[must_use]
pub fn build_user_config(keystore: &Keystore, args: InitArgs) -> UserConfig {
    let InitArgs {
        log: log_args,
        network: network_args,
        blend: blend_args,
        cryptarchia: cryptarchia_args,
        sdp: sdp_args,
        api: api_args,
        state: state_args,
        ..
    } = args;

    let time_config = TimeConfig::default();

    let storage_config = StorageConfig::default();

    let mut state_config = StateConfig::default();
    update_state(&mut state_config, state_args);

    let mut api_config = ApiConfig::default();
    update_api(&mut api_config, api_args);

    let mut tracing_config = TracingConfig::default();
    update_tracing(&mut tracing_config, log_args).expect("Cli tracing params can be parsed");

    let initial_peers = network_args.initial_peers.clone();
    let network_config = build_network_config(keystore, network_args);

    let blend_config = build_blend_config(keystore, blend_args);

    let cryptarchia_config = build_cryptarchia_config(keystore, initial_peers, cryptarchia_args);

    let sdp_config = build_sdp_config(keystore, sdp_args);

    let wallet_config = build_wallet_config(keystore);

    let kms_config = build_kms_config(keystore);

    UserConfig {
        network: network_config,
        blend: blend_config,
        cryptarchia: cryptarchia_config,
        time: time_config,
        sdp: sdp_config,
        api: api_config,
        storage: storage_config,
        kms: kms_config,
        wallet: wallet_config,
        tracing: tracing_config,
        state: state_config,
    }
}

fn build_network_config(keystore: &Keystore, network_args: NetworkArgs) -> NetworkConfig {
    let unsecured_key = keystore
        .get_ed25519(KeyTitle::NETWORK_SWARM)
        .map(|(_, key)| key)
        .expect("Network key set by default");
    let mut network_secret_key_bytes: [u8; 32] = *unsecured_key.as_bytes();

    let mut network_config = NetworkConfig::default();
    network_config.backend.swarm.node_key =
        lb_libp2p::ed25519::SecretKey::try_from_bytes(&mut network_secret_key_bytes)
            .expect("Valid default secret key structure");
    update_network(&mut network_config, network_args)
        .expect("Network configuration should update from cli args");

    network_config
}

fn build_blend_config(keystore: &Keystore, blend_args: BlendArgs) -> BlendConfig {
    let (blend_signing_key_id, _) = keystore
        .get(KeyTitle::BLEND_SIGNING)
        .expect("Blend signing key set by default");
    let (blend_zk_key_id, _) = keystore
        .get(KeyTitle::BLEND_ZK)
        .expect("Blend zk key set by default");
    let mut blend_config = BlendConfig::with_required_values(BlendConfigRequiredValues {
        non_ephemeral_signing_key_id: blend_signing_key_id,
        secret_key_kms_id: blend_zk_key_id,
    });
    update_blend(&mut blend_config, blend_args);

    blend_config
}

fn build_cryptarchia_config(
    keystore: &Keystore,
    initial_peers: Option<Vec<Multiaddr>>,
    cryptarchia_args: CryptarchiaArgs,
) -> CryptarchiaConfig {
    let (_, cryptarchia_funding_key) = keystore
        .get_zk(KeyTitle::LEADER_FUNDING)
        .expect("Cryptarchia funding key set by default");
    let mut cryptarchia_config =
        CryptarchiaConfig::with_required_values(CryptarchiaConfigRequiredValues {
            funding_pk: cryptarchia_funding_key.to_public_key(),
        });
    if let Some(initial_peers) = initial_peers {
        cryptarchia_config.network.bootstrap.ibd.peers = initial_peers
            .iter()
            .filter_map(|addr| match addr.iter().last() {
                Some(lb_libp2p::Protocol::P2p(bytes)) => PeerId::from_multihash(bytes.into()).ok(),
                _ => None,
            })
            .collect();
    }
    update_cryptarchia(&mut cryptarchia_config, cryptarchia_args);

    cryptarchia_config
}

fn build_sdp_config(keystore: &Keystore, sdp_args: SdpArgs) -> SdpConfig {
    let (_, sdp_funding_key) = keystore
        .get_zk(KeyTitle::SDP_FUNDING)
        .expect("Sdp funding key set by default");
    let mut sdp_config = SdpConfig::with_required_values(SdpConfigRequiredValues {
        funding_pk: sdp_funding_key.to_public_key(),
    });
    update_sdp(&mut sdp_config, sdp_args);

    sdp_config
}

fn build_kms_config(keystore: &Keystore) -> KmsConfig {
    let mut kms_config = KmsConfig::default();
    kms_config.backend.keys = keystore
        .get_all()
        .map(|(id, key)| (id, key.clone()))
        .collect();

    kms_config
}

fn build_wallet_config(keystore: &Keystore) -> WalletConfig {
    let (voucher_master_key_id, _) = keystore
        .get(KeyTitle::VAUCHER_MASTER)
        .expect("Vaucher master key set by default");

    let mut wallet_config = WalletConfig::with_required_values(WalletConfigRequiredValues {
        voucher_master_key_id,
    });
    wallet_config.known_keys = keystore
        .get_all_zk()
        .map(|(id, key)| (id, key.to_public_key()))
        .collect();

    wallet_config
}
