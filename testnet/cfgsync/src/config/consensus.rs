use std::time::Duration;

use blake2::{Blake2b, Digest as _, digest::consts::U32};
use lb_core::mantle::{Note, Utxo, genesis_tx::GenesisTx};
use lb_key_management_system_service::keys::{ZkKey, ZkPublicKey};
use lb_tests::topology::configs::consensus::{
    GeneralConsensusConfig, ServiceNote, create_genesis_tx,
};
use num_bigint::BigUint;

use crate::{Entropy, FaucetSettings};

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
) -> (Vec<GeneralConsensusConfig>, Option<FaucetInfo>, GenesisTx) {
    let mut regular_note_keys = Vec::new();
    let mut blend_notes = Vec::new();
    let mut sdp_notes = Vec::new();

    let (utxos, faucet_info) = create_utxos(
        entropy,
        ids,
        &mut regular_note_keys,
        &mut blend_notes,
        &mut sdp_notes,
        faucet_settings,
    );
    let genesis_tx = create_genesis_tx(&utxos);
    let consensus_configs = regular_note_keys
        .into_iter()
        .enumerate()
        .map(|(i, sk)| {
            let funding_sk = sdp_notes[i].sk.clone();
            let funding_pk = sdp_notes[i].pk;

            GeneralConsensusConfig {
                blend_notes: blend_notes.clone(),
                known_key: sk,
                funding_sk,
                funding_pk,
                other_keys: Vec::new(),
                prolonged_bootstrap_period,
            }
        })
        .collect();

    (consensus_configs, faucet_info, genesis_tx)
}

fn generate_faucet_key(entropy: &Entropy) -> ZkKey {
    let mut hasher = Blake2b::<U32>::new();
    hasher.update(entropy);
    hasher.update(b"faucet");
    let bytes: [u8; 32] = hasher.finalize().into();
    ZkKey::from(BigUint::from_bytes_le(&bytes))
}

fn create_utxos(
    entropy: &Entropy,
    ids: &[[u8; 32]],
    regular_note_keys: &mut Vec<ZkKey>,
    blend_notes: &mut Vec<ServiceNote>,
    sdp_notes: &mut Vec<ServiceNote>,
    faucet_settings: &FaucetSettings,
) -> (Vec<Utxo>, Option<FaucetInfo>) {
    let derive_key_material = |prefix: &[u8], id_bytes: &[u8]| -> [u8; 16] {
        let mut sk_data = [0; 16];
        let prefix_len = prefix.len();

        sk_data[..prefix_len].copy_from_slice(prefix);
        let bytes_to_copy = std::cmp::min(16 - prefix_len, id_bytes.len());
        sk_data[prefix_len..prefix_len + bytes_to_copy].copy_from_slice(&id_bytes[..bytes_to_copy]);

        sk_data
    };

    let mut utxos = Vec::new();

    // Assume output index which will be set by the ledger tx.
    let mut output_index = 0;

    // Create notes for leader and Blend declarations.
    for &id in ids {
        let sk_data = derive_key_material(b"ld", &id);
        let sk = ZkKey::from(BigUint::from_bytes_le(&sk_data));
        let pk = sk.to_public_key();
        regular_note_keys.push(sk);
        utxos.push(Utxo {
            note: Note::new(100_000, pk),
            tx_hash: BigUint::from(0u8).into(),
            output_index: 0,
        });
        output_index += 1;

        let sk_blend_data = derive_key_material(b"bn", &id);
        let sk_blend = ZkKey::from(BigUint::from_bytes_le(&sk_blend_data));
        let pk_blend = sk_blend.to_public_key();
        let note_blend = Note::new(1, pk_blend);
        blend_notes.push(ServiceNote {
            pk: pk_blend,
            sk: sk_blend,
            note: note_blend,
            output_index,
        });
        utxos.push(Utxo {
            note: note_blend,
            tx_hash: BigUint::from(0u8).into(),
            output_index: 0,
        });
        output_index += 1;

        let sk_sdp_data = derive_key_material(b"sdp", &id);
        let sk_sdp = ZkKey::from(BigUint::from_bytes_le(&sk_sdp_data));
        let pk_sdp = sk_sdp.to_public_key();
        let note_sdp = Note::new(100, pk_sdp);
        sdp_notes.push(ServiceNote {
            pk: pk_sdp,
            sk: sk_sdp,
            note: note_sdp,
            output_index,
        });
        utxos.push(Utxo {
            note: note_sdp,
            tx_hash: BigUint::from(0u8).into(),
            output_index,
        });
        output_index += 1;
    }

    // Create a single faucet UTXO with value = u64::MAX - sum(other UTXOs)
    let faucet_info = faucet_settings.enabled.then(|| {
        let other_sum: u64 = utxos.iter().map(|u| u.note.value).sum();
        let faucet_value = u64::MAX - other_sum;
        let faucet_sk = generate_faucet_key(entropy);
        let faucet_pk = faucet_sk.to_public_key();
        utxos.push(Utxo {
            note: Note::new(faucet_value, faucet_pk),
            tx_hash: BigUint::from(0u8).into(),
            output_index,
        });
        FaucetInfo {
            sk: faucet_sk,
            pk: faucet_pk,
        }
    });

    (utxos, faucet_info)
}
