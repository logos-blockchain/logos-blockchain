use std::time::Duration;

use lb_core::mantle::{Note, Utxo, genesis_tx::GenesisTx};
use lb_key_management_system_service::keys::ZkKey;
use lb_tests::topology::configs::consensus::{
    GeneralConsensusConfig, ServiceNote, create_genesis_tx,
};
use num_bigint::BigUint;

use crate::{FaucetSettings, config::FaucetNotes};

#[must_use]
pub fn create_consensus_configs(
    ids: &[[u8; 32]],
    prolonged_bootstrap_period: Duration,
    faucet_settings: &FaucetSettings,
) -> (Vec<GeneralConsensusConfig>, Vec<FaucetNotes>, GenesisTx) {
    let mut regular_note_keys = Vec::new();
    let mut blend_notes = Vec::new();
    let mut sdp_notes = Vec::new();
    let mut faucet_note_keys = Vec::new();

    let utxos = create_utxos(
        ids,
        &mut regular_note_keys,
        &mut blend_notes,
        &mut sdp_notes,
        &mut faucet_note_keys,
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
                other_keys: faucet_note_keys[i].clone(),
                prolonged_bootstrap_period,
            }
        })
        .collect();

    (consensus_configs, faucet_note_keys, genesis_tx)
}

fn create_utxos(
    ids: &[[u8; 32]],
    regular_note_keys: &mut Vec<ZkKey>,
    blend_notes: &mut Vec<ServiceNote>,
    sdp_notes: &mut Vec<ServiceNote>,
    faucet_note_keys: &mut Vec<FaucetNotes>,
    faucet_settings: &FaucetSettings,
) -> Vec<Utxo> {
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

    faucet_note_keys.resize(ids.len(), Vec::new());

    // Create notes for leader and Blend declarations.
    for (config_idx, &id) in ids.iter().enumerate() {
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

        let sk_faucet_data = derive_key_material(b"fc", &id);
        let sk_faucet = ZkKey::from(BigUint::from_bytes_le(&sk_faucet_data));
        let pk_faucet = sk_faucet.to_public_key();

        // Create notes for faucet.
        for _ in 0..faucet_settings.note_count {
            utxos.push(Utxo {
                note: Note::new(faucet_settings.note_value, pk_faucet),
                tx_hash: BigUint::from(0u8).into(),
                output_index,
            });
            output_index += 1;
        }

        faucet_note_keys[config_idx].push(sk_faucet.clone());
    }

    utxos
}
