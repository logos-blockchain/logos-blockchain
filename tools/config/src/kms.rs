use lb_groth16::fr_to_bytes;
use lb_key_management_system_service::{
    backend::preload::KeyId,
    keys::{Key, secured_key::SecuredKey as _},
};
use lb_node::config::{KmsConfig, kms::serde::PreloadKmsBackendSettings};

use crate::{blend::GeneralBlendConfig, consensus::GeneralConsensusConfig};

#[must_use]
pub fn key_id_for_preload_backend(key: &Key) -> KeyId {
    let key_id_bytes = match key {
        Key::Ed25519(ed25519_secret_key) => ed25519_secret_key.as_public_key().to_bytes(),
        Key::Zk(zk_secret_key) => fr_to_bytes(zk_secret_key.as_public_key().as_fr()),
    };
    hex::encode(key_id_bytes)
}

#[must_use]
pub fn create_kms_configs(
    blend_configs: &[GeneralBlendConfig],
    consensus_configs: &[GeneralConsensusConfig],
    shared_keys: Option<&[Key]>,
) -> Vec<KmsConfig> {
    let mut kms_configs: Vec<KmsConfig> = blend_configs
        .iter()
        .enumerate()
        .map(|(i, (blend_conf, private_key, zk_secret_key))| KmsConfig {
            backend: PreloadKmsBackendSettings {
                keys: [
                    (
                        blend_conf.non_ephemeral_signing_key_id.clone(),
                        private_key.clone().into(),
                    ),
                    (
                        blend_conf.core.zk.secret_key_kms_id.clone(),
                        zk_secret_key.clone().into(),
                    ),
                    (
                        key_id_for_preload_backend(
                            &consensus_configs[i].blend_note.sk.clone().into(),
                        ),
                        consensus_configs[i].blend_note.sk.clone().into(),
                    ),
                    (
                        key_id_for_preload_backend(&consensus_configs[i].known_key.clone().into()),
                        consensus_configs[i].known_key.clone().into(),
                    ),
                    (
                        key_id_for_preload_backend(&consensus_configs[i].funding_sk.clone().into()),
                        consensus_configs[i].funding_sk.clone().into(),
                    ),
                ]
                .into(),
            },
        })
        .collect();

    if let Some(shared_keys) = shared_keys {
        for key in shared_keys {
            let key_id = key_id_for_preload_backend(key);
            for kms in &mut kms_configs {
                kms.backend.keys.insert(key_id.clone(), key.clone());
            }
        }
    }

    kms_configs
}
