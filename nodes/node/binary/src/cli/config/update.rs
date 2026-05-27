use color_eyre::eyre::Result;
use libp2p::{Multiaddr, PeerId};
use thiserror::Error;

use crate::{
    NetworkArgs, UserConfig,
    cli::{
        UpdateArgs,
        config::{
            confirm_overwrite,
            keystore::{KeyTitle, Keystore},
        },
    },
    config::{
        BlendArgs, BlendConfig, CryptarchiaArgs, CryptarchiaConfig, KmsConfig, NetworkConfig,
        SdpArgs, SdpConfig, WalletConfig, update_api, update_blend, update_cryptarchia,
        update_network, update_sdp, update_state, update_tracing,
    },
};

#[derive(Error, Debug)]
pub enum UpdateError {
    #[error("Update command cancelled by user.")]
    UserCancelled,

    #[error("User configuration file does not exist. Use `init` command.")]
    UserFileDoesNotExist,

    #[error("Keystore file does not exist. Use `init` command.")]
    KeystoreFileDoesNotExist,
}

pub fn run(args: UpdateArgs) -> Result<()> {
    let user_config_path = args.user_config.clone();
    let keystore_path = args.keystore.clone();

    if !user_config_path.exists() {
        return Err(UpdateError::UserFileDoesNotExist.into());
    }

    if !keystore_path.exists() {
        return Err(UpdateError::KeystoreFileDoesNotExist.into());
    }

    if !args.yes && !confirm_overwrite("Do you want to overwrite existing user configuration?")? {
        return Err(UpdateError::UserCancelled.into());
    }

    let user_config_yaml = std::fs::read_to_string(&user_config_path)?;
    let mut user_config: UserConfig = serde_yaml::from_str(&user_config_yaml)?;

    let keystore_yaml = std::fs::read_to_string(&keystore_path)?;
    let keystore: Keystore = serde_yaml::from_str(&keystore_yaml)?;

    update_user_config(&mut user_config, &keystore, args);

    let user_config_yaml = serde_yaml::to_string(&user_config)?;
    std::fs::write(&user_config_path, &user_config_yaml)?;

    Ok(())
}

fn update_user_config(user_config: &mut UserConfig, keystore: &Keystore, args: UpdateArgs) {
    let UpdateArgs {
        log: log_args,
        network: network_args,
        blend: blend_args,
        cryptarchia: cryptarchia_args,
        sdp: sdp_args,
        api: api_args,
        state: state_args,
        ..
    } = args;

    update_state(&mut user_config.state, state_args);

    update_api(&mut user_config.api, api_args);

    update_tracing(&mut user_config.tracing, log_args).expect("Cli tracing params can be parsed");

    let initial_peers = network_args.initial_peers.clone();
    update_network_config(keystore, &mut user_config.network, network_args);

    update_blend_config(keystore, &mut user_config.blend, blend_args);

    update_cryptarchia_config(
        keystore,
        initial_peers,
        &mut user_config.cryptarchia,
        cryptarchia_args,
    );

    update_sdp_config(keystore, &mut user_config.sdp, sdp_args);

    update_kms_config(keystore, &mut user_config.kms);

    update_wallet_config(keystore, &mut user_config.wallet);
}

fn update_network_config(
    keystore: &Keystore,
    network_config: &mut NetworkConfig,
    network_args: NetworkArgs,
) {
    let unsecured_key = keystore
        .get_ed25519(KeyTitle::NetworkSwarm)
        .map(|(_, key)| key)
        .expect("Network key set by default");
    let mut network_secret_key_bytes: [u8; 32] = *unsecured_key.as_bytes();

    network_config.backend.swarm.node_key =
        lb_libp2p::ed25519::SecretKey::try_from_bytes(&mut network_secret_key_bytes)
            .expect("Valid default secret key structure");
    update_network(network_config, network_args)
        .expect("Network configuration should update from cli args");
}

fn update_blend_config(keystore: &Keystore, blend_config: &mut BlendConfig, blend_args: BlendArgs) {
    let (blend_signing_key_id, _) = keystore
        .get(KeyTitle::BlendSigning)
        .expect("Blend signing key set by default");
    let (blend_zk_key_id, _) = keystore
        .get(KeyTitle::BlendZk)
        .expect("Blend zk key set by default");

    blend_config.set_non_ephemeral_signing_key_id(blend_signing_key_id);
    blend_config.set_secret_zk_key_id(blend_zk_key_id);

    update_blend(blend_config, blend_args);
}

fn update_cryptarchia_config(
    keystore: &Keystore,
    initial_peers: Option<Vec<Multiaddr>>,
    cryptarchia_config: &mut CryptarchiaConfig,
    cryptarchia_args: CryptarchiaArgs,
) {
    let (_, cryptarchia_funding_key) = keystore
        .get_zk(KeyTitle::LeaderFunding)
        .expect("Cryptarchia funding key set by default");
    cryptarchia_config.set_funding_pk(cryptarchia_funding_key.to_public_key());

    if let Some(initial_peers) = initial_peers {
        cryptarchia_config.network.bootstrap.ibd.peers = initial_peers
            .iter()
            .filter_map(|addr| match addr.iter().last() {
                Some(lb_libp2p::Protocol::P2p(bytes)) => PeerId::from_multihash(bytes.into()).ok(),
                _ => None,
            })
            .collect();
    }

    update_cryptarchia(cryptarchia_config, cryptarchia_args);
}

fn update_sdp_config(keystore: &Keystore, sdp_config: &mut SdpConfig, sdp_args: SdpArgs) {
    let (_, sdp_funding_key) = keystore
        .get_zk(KeyTitle::SdpFunding)
        .expect("Sdp funding key set by default");
    sdp_config.set_funding_pk(sdp_funding_key.to_public_key());

    update_sdp(sdp_config, sdp_args);
}

fn update_kms_config(keystore: &Keystore, kms_config: &mut KmsConfig) {
    kms_config.backend.keys = KeyTitle::ALL
        .into_iter()
        .map(|title| {
            let (id, key) = keystore.get(title).expect("Key is set by default");
            (id, key.clone())
        })
        .collect();
}

fn update_wallet_config(keystore: &Keystore, wallet_config: &mut WalletConfig) {
    let wallet_keys = [
        KeyTitle::BlendZk,
        KeyTitle::LeaderFunding,
        KeyTitle::SdpFunding,
        KeyTitle::VaucherMaster,
        KeyTitle::Stake,
    ];

    let (voucher_master_key_id, _) = keystore
        .get(KeyTitle::VaucherMaster)
        .expect("Vaucher master key set by default");

    wallet_config.voucher_master_key_id = voucher_master_key_id;
    wallet_config.known_keys = wallet_keys
        .into_iter()
        .map(|title| {
            let (id, key) = keystore.get_zk(title).expect("Key is set by default");
            (id, key.to_public_key())
        })
        .collect();
}
