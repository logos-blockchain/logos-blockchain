use lb_key_management_system_service::{backend::preload::KeyId, keys::Ed25519Key};
use lb_libp2p::ed25519::SecretKey;
use serde::{Deserialize, Deserializer};

pub use crate::config::{kms::serde::Config as KmsConfig, wallet::serde::Config as WalletConfig};
use crate::{
    cli::config::keystore::{KeyTitle, Keystore},
    config::parse_hex_ed25519_key,
};

#[derive(Debug, Clone, Deserialize)]
pub struct OldNetworkConfig {
    pub backend: OldNetworkBackend,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OldNetworkBackend {
    pub swarm: OldSwarmConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OldSwarmConfig {
    #[serde(deserialize_with = "deserialize_secret_key")]
    pub node_key: SecretKey,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OldBlendConfig {
    pub non_ephemeral_signing_key_id: KeyId,
    pub core: OldBlendCore,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OldBlendCore {
    pub zk: OldZkConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OldZkConfig {
    pub secret_key_kms_id: KeyId,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OldCryptarchiaConfig {
    pub leader: OldLeaderConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OldLeaderConfig {
    pub wallet: OldCryptarchiaWallet,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OldCryptarchiaWallet {
    pub funding_pk: KeyId,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OldSdpConfig {
    pub wallet: OldSdpWallet,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OldSdpWallet {
    pub funding_pk: KeyId,
}

/// Holds the values of v0.1.2 configuration that should be transfered to a new
/// config version.
#[derive(Debug, Clone, Deserialize)]
pub struct OldConfig {
    pub network: OldNetworkConfig,
    pub blend: OldBlendConfig,
    pub cryptarchia: OldCryptarchiaConfig,
    pub sdp: OldSdpConfig,
    pub kms: KmsConfig,
    pub wallet: WalletConfig,
}

impl OldConfig {
    #[must_use]
    pub fn into_keystore(self) -> Keystore {
        let mut keystore = Keystore::default();
        let mut old_kms = self.kms.backend.keys;

        let network_secret_key = self.network.backend.swarm.node_key;
        let network_secret_key = Ed25519Key::from_bytes(
            network_secret_key
                .as_ref()
                .try_into()
                .expect("SecretKey is guaranteed to be 32 bytes"),
        );
        keystore.set(KeyTitle::NETWORK_SWARM, network_secret_key.into());

        if let Some(blend_signing) = old_kms.remove(&self.blend.non_ephemeral_signing_key_id) {
            keystore.set(KeyTitle::BLEND_SIGNING, blend_signing);
        }

        if let Some(blend_zk) = old_kms.remove(&self.blend.core.zk.secret_key_kms_id) {
            keystore.set(KeyTitle::BLEND_ZK, blend_zk);
        }

        if let Some(leader_funding) = old_kms.remove(&self.cryptarchia.leader.wallet.funding_pk) {
            keystore.set(KeyTitle::LEADER_FUNDING, leader_funding);
        }

        if let Some(sdp_funding) = old_kms.remove(&self.sdp.wallet.funding_pk) {
            keystore.set(KeyTitle::SDP_FUNDING, sdp_funding);
        }

        if let Some(voucher_master) = old_kms.remove(&self.wallet.voucher_master_key_id) {
            keystore.set(KeyTitle::VAUCHER_MASTER, voucher_master);
        }

        keystore
    }
}

fn deserialize_secret_key<'de, D>(deserializer: D) -> Result<SecretKey, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    parse_hex_ed25519_key(&s).map_err(serde::de::Error::custom)
}
