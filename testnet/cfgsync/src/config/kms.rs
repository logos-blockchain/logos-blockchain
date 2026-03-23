use lb_node::config::{KmsConfig, kms::serde::PreloadKmsBackendSettings};
use lb_tests::{
    common::kms::key_id_for_preload_backend,
    topology::configs::{blend::GeneralBlendConfig, consensus::GeneralConsensusConfig},
};

use crate::config::consensus::FaucetInfo;

#[must_use]
pub fn create_kms_configs(
    blend_configs: &[GeneralBlendConfig],
    consensus_configs: &[GeneralConsensusConfig],
    faucet_info: Option<&FaucetInfo>,
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
                    // SDP funding secret key - used by wallet for signing SDP transactions
                    (
                        key_id_for_preload_backend(&consensus_configs[i].funding_sk.clone().into()),
                        consensus_configs[i].funding_sk.clone().into(),
                    ),
                ]
                .into(),
            },
        })
        .collect();

    // Give faucet SK to all nodes so the faucet service can route to any node.
    if let Some(faucet) = faucet_info {
        let key = faucet.sk.clone().into();
        let key_id = key_id_for_preload_backend(&key);
        for kms in &mut kms_configs {
            kms.backend.keys.insert(key_id.clone(), key.clone());
        }
    }

    kms_configs
}
