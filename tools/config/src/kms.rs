use lb_key_management_system_service::keys::Key;
pub use lb_node::config::keystore::key_id_for_preload_backend;
use lb_node::config::{KmsConfig, kms::serde::PreloadKmsBackendSettings};

use crate::{blend::GeneralBlendConfig, consensus::GeneralConsensusConfig};

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
            ..KmsConfig::default()
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
