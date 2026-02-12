use lb_node::config::{KmsConfig, kms::serde::PreloadKmsBackendSettings};
use lb_tests::{
    common::kms::key_id_for_preload_backend,
    topology::configs::{blend::GeneralBlendConfig, consensus::GeneralConsensusConfig},
};

use crate::config::FaucetNotes;

#[must_use]
pub fn create_kms_configs(
    blend_configs: &[GeneralBlendConfig],
    consensus_configs: &[GeneralConsensusConfig],
    faucet_note_keys: &[FaucetNotes],
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

    for (config, note_keys) in kms_configs.iter_mut().zip(faucet_note_keys.iter()) {
        config.backend.keys.extend(note_keys.iter().map(|sk| {
            (
                key_id_for_preload_backend(&sk.clone().into()),
                sk.clone().into(),
            )
        }));
    }

    kms_configs
}
