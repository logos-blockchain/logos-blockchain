use std::{
    fmt::{self, Display, Formatter},
    path::Path,
};

use color_eyre::eyre::Result;
use lb_groth16::fr_to_bytes;
use lb_key_management_system_keys::{
    hd::{self, HardenedIndex, Mnemonic, NoteRole, u31},
    keys::ZkPublicKey,
};
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

    wallet_pubkeys_from_config(&user_config).for_each(|pubkey| {
        println!("{pubkey}");
    });

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

    let pow_config = build_pow_config();

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
        non_ephemeral_signing_key_id: blend_signing_key_id,
        secret_key_kms_id: blend_zk_key_id,
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

/// Mining defaults, with auto-claim paying the wallet without a cap,
/// so a generated node claims its mined rewards unattended once mining is
/// started.
fn build_pow_config() -> PoWConfig {
    let pow_config = PoWConfig::default();
    // TODO: pow_config.auto_claim.threshold = Some(Value::MAX);
    pow_config
}

fn build_wallet_config(keystore: &Keystore) -> WalletConfig {
    WalletConfig {
        known_keys: keystore
            .get_static_keys()
            .map(|(key_id, _)| key_id)
            .collect(),
        ..WalletConfig::default()
    }
}

const HD_ACCOUNT: HardenedIndex = HardenedIndex::new(u31::new(0));

/// Derives public keys from HD paths in the wallet
#[must_use]
pub fn wallet_pubkeys_from_config(user_config: &UserConfig) -> impl Iterator<Item = PathPublicKey> {
    let funding_start_index = u32::from(user_config.wallet.funding_start_index.child_number());
    (0u32..=funding_start_index).map(|index| {
        let path = hd::Path::Note {
            account: HD_ACCOUNT,
            role: NoteRole::Receive,
            index: HardenedIndex::new(index.try_into().expect("must be u31")),
        };
        PathPublicKey {
            path,
            public_key: user_config
                .kms
                .backend
                .derive_key(&path)
                .to_zk_key()
                .to_public_key(),
        }
    })
}

pub struct PathPublicKey {
    pub path: hd::Path,
    pub public_key: ZkPublicKey,
}

impl Display for PathPublicKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let public_key = hex::encode(fr_to_bytes(self.public_key.as_fr()));
        write!(f, "{} - pub: {public_key}", self.path)
    }
}
