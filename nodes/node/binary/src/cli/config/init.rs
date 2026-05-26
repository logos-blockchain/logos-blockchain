use color_eyre::eyre::Result;
use libp2p::{Multiaddr, PeerId};

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

pub fn run(args: InitArgs) -> Result<()> {
    let output_path = args.output.clone();
    let keystore = build_keystore();
    let user_config = build_user_config(&keystore, args);

    Ok(())
}

fn build_user_config(keystore: &Keystore, args: InitArgs) -> UserConfig {
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
        .get_ed25519(KeyTitle::NetworkSwarm)
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
        .get(KeyTitle::BlendSigning)
        .expect("Blend signing key set by default");
    let (blend_zk_key_id, _) = keystore
        .get(KeyTitle::BlendZk)
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
        .get_zk(KeyTitle::LeaderFunding)
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
        .get_zk(KeyTitle::SdpFunding)
        .expect("Sdp funding key set by default");
    let mut sdp_config = SdpConfig::with_required_values(SdpConfigRequiredValues {
        funding_pk: sdp_funding_key.to_public_key(),
    });
    update_sdp(&mut sdp_config, sdp_args);

    sdp_config
}

fn build_kms_config(keystore: &Keystore) -> KmsConfig {
    let mut kms_config = KmsConfig::default();
    kms_config.backend.keys = KeyTitle::ALL
        .into_iter()
        .map(|title| {
            let (id, key) = keystore.get(title).expect("Key is set by default");
            (id, key.clone())
        })
        .collect();

    kms_config
}

fn build_wallet_config(keystore: &Keystore) -> WalletConfig {
    let wallet_keys = [
        KeyTitle::BlendZk,
        KeyTitle::LeaderFunding,
        KeyTitle::SdpFunding,
        KeyTitle::BlendFunding,
        KeyTitle::VaucherMaster,
    ];

    let (voucher_master_key_id, _) = keystore
        .get(KeyTitle::VaucherMaster)
        .expect("Vaucher master key set by default");

    let mut wallet_config = WalletConfig::with_required_values(WalletConfigRequiredValues {
        voucher_master_key_id,
    });
    wallet_config.known_keys = wallet_keys
        .into_iter()
        .map(|title| {
            let (id, key) = keystore.get_zk(title).expect("Key is set by default");
            (id, key.to_public_key())
        })
        .collect();

    wallet_config
}

fn build_keystore() -> Keystore {
    let mut keystore = Keystore::default();

    // By default keystore generates the a unique key for every key title.
    // To simplify the `init` command behaviour we use the same key for sdp and blend funding.
    // Leader is still using a unique funding key.
    let funding_key = keystore
        .get(KeyTitle::BlendFunding)
        .map(|(_, key)| key.clone())
        .expect("Blend funding key set by default");

    keystore.set(KeyTitle::SdpFunding, funding_key);

    keystore
}
