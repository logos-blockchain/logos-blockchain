use std::time::Duration;

use blake2::{Blake2b, Digest as _, digest::consts::U32};
use lb_config::consensus::{GeneralConsensusConfig, create_base_consensus_material};
use lb_core::{
    block::genesis::GenesisBlock,
    mantle::{Note, Utxo},
};
use lb_key_management_system_service::keys::{ZkKey, ZkPublicKey};
use lb_tests::topology::configs::consensus::create_genesis_block;
use num_bigint::BigUint;

use crate::{Entropy, FaucetSettings};

const FAUCET_KEY_CONTEXT: &[u8] = b"faucet";

pub struct FaucetInfo {
    pub sk: ZkKey,
    pub pk: ZkPublicKey,
}

#[must_use]
pub fn create_consensus_configs(
    entropy: &Entropy,
    ids: &[[u8; 32]],
    prolonged_bootstrap_period: Duration,
    faucet_settings: &FaucetSettings,
) -> (
    Vec<GeneralConsensusConfig>,
    Option<FaucetInfo>,
    GenesisBlock,
) {
    let material = create_base_consensus_material(ids);
    let (utxos, faucet_info) = create_utxos(entropy, material.utxos, faucet_settings);
    let genesis_block = create_genesis_block(&utxos, None);
    let consensus_configs = material
        .regular_note_keys
        .into_iter()
        .enumerate()
        .map(|(i, sk)| {
            let funding_sk = material.sdp_notes[i].sk.clone();
            let funding_pk = material.sdp_notes[i].pk;
            let blend_note = material.blend_notes[i].clone();

            GeneralConsensusConfig {
                blend_note,
                known_key: sk,
                funding_sk,
                funding_pk,
                other_keys: Vec::new(),
                prolonged_bootstrap_period,
            }
        })
        .collect();

    (consensus_configs, faucet_info, genesis_block)
}

fn generate_faucet_key(entropy: &Entropy) -> ZkKey {
    let mut hasher = Blake2b::<U32>::new();
    hasher.update(entropy);
    hasher.update(FAUCET_KEY_CONTEXT);
    let bytes: [u8; 32] = hasher.finalize().into();
    ZkKey::from(BigUint::from_bytes_le(&bytes))
}

fn create_utxos(
    entropy: &Entropy,
    mut utxos: Vec<Utxo>,
    faucet_settings: &FaucetSettings,
) -> (Vec<Utxo>, Option<FaucetInfo>) {
    // Create a single faucet UTXO with value = u64::MAX - sum(other UTXOs)
    let faucet_info = faucet_settings.enabled.then(|| {
        let other_sum: u64 = utxos.iter().map(|u| u.note.value).sum();
        let faucet_value = u64::MAX - other_sum;
        let faucet_sk = generate_faucet_key(entropy);
        let faucet_pk = faucet_sk.to_public_key();
        let output_index = utxos.len();
        utxos.push(Utxo {
            note: Note::new(faucet_value, faucet_pk),
            op_id: [0u8; 32],
            output_index,
        });
        FaucetInfo {
            sk: faucet_sk,
            pk: faucet_pk,
        }
    });

    (utxos, faucet_info)
}
