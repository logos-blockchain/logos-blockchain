use core::time::Duration;

use lb_core::{
    mantle::{
        MantleTx, Note, NoteId, OpProof, Utxo,
        genesis_tx::GenesisTx,
        ops::{
            Op,
            channel::{ChannelId, Ed25519PublicKey, MsgId, inscribe::InscriptionOp},
            transfer::TransferOp,
        },
    },
    sdp::{DeclarationMessage, Locator, ProviderId, ServiceType},
};
use lb_groth16::CompressedGroth16Proof;
use lb_key_management_system_service::keys::{Ed25519Key, ZkKey, ZkPublicKey, ZkSignature};
use lb_node::{SignedMantleTx, Transaction as _};
use lb_testing_framework::unique_test_context;
use num_bigint::BigUint;

pub const SHORT_PROLONGED_BOOTSTRAP_PERIOD: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct ProviderInfo {
    pub service_type: ServiceType,
    pub provider_sk: Ed25519Key,
    pub zk_sk: ZkKey,
    pub locator: Locator,
    pub note: ServiceNote,
}

impl ProviderInfo {
    #[must_use]
    pub fn provider_id(&self) -> ProviderId {
        ProviderId(self.provider_sk.public_key())
    }

    #[must_use]
    pub fn zk_id(&self) -> ZkPublicKey {
        self.zk_sk.to_public_key()
    }
}

/// General consensus configuration for a chosen participant, that later could
/// be converted into a specific service or services configuration.
#[derive(Clone, Debug)]
pub struct GeneralConsensusConfig {
    pub known_key: ZkKey,
    pub blend_note: ServiceNote,
    pub funding_sk: ZkKey,
    pub funding_pk: ZkPublicKey,
    pub other_keys: Vec<ZkKey>,
    pub prolonged_bootstrap_period: Duration,
}

#[derive(Clone, Debug)]
pub struct ServiceNote {
    pub pk: ZkPublicKey,
    pub sk: ZkKey,
    pub note: Note,
    pub note_id: NoteId,
    pub output_index: usize,
}

fn inscription_for_current_test(test_context: Option<&str>) -> InscriptionOp {
    let owner = unique_test_context(test_context);
    println!("Genesis inscription: {owner}");
    InscriptionOp {
        channel_id: ChannelId::from([0; 32]),
        inscription: owner.into_bytes(),
        parent: MsgId::root(),
        signer: Ed25519PublicKey::from_bytes(&[0; 32]).unwrap(),
    }
}

#[must_use]
pub fn create_genesis_tx(utxos: &[Utxo], test_context: Option<&str>) -> GenesisTx {
    let inscription = inscription_for_current_test(test_context);

    // Create transfer op with the utxos as outputs
    let outputs: Vec<Note> = utxos.iter().map(|u| u.note).collect();
    let transfer_op = TransferOp::new(vec![], outputs);

    // Create the mantle transaction
    let mantle_tx = MantleTx {
        ops: vec![Op::Transfer(transfer_op), Op::ChannelInscribe(inscription)],
        execution_gas_price: 0.into(),
        storage_gas_price: 0.into(),
    };
    let signed_mantle_tx = SignedMantleTx {
        mantle_tx,
        ops_proofs: vec![
            OpProof::ZkSig(ZkSignature::new(CompressedGroth16Proof::from_bytes(
                &[0u8; 128],
            ))),
            OpProof::NoProof,
        ],
    };

    // Wrap in GenesisTx
    GenesisTx::from_tx(signed_mantle_tx).expect("Invalid genesis transaction")
}

#[must_use]
pub fn create_consensus_configs(
    ids: &[[u8; 32]],
    prolonged_bootstrap_period: Duration,
    test_context: Option<&str>,
) -> (Vec<GeneralConsensusConfig>, GenesisTx) {
    let mut regular_note_keys = Vec::new();
    let mut blend_notes = Vec::new();
    let mut sdp_notes = Vec::new();

    let utxos = create_utxos(
        ids,
        &mut regular_note_keys,
        &mut blend_notes,
        &mut sdp_notes,
    );
    let genesis_tx = create_genesis_tx(&utxos, test_context);

    (
        regular_note_keys
            .into_iter()
            .enumerate()
            .map(|(i, sk)| {
                let funding_sk = sdp_notes[i].sk.clone();
                let funding_pk = sdp_notes[i].pk;
                let blend_note = blend_notes[i].clone();

                GeneralConsensusConfig {
                    blend_note,
                    known_key: sk,
                    funding_sk,
                    funding_pk,
                    other_keys: Vec::new(),
                    prolonged_bootstrap_period,
                }
            })
            .collect(),
        genesis_tx,
    )
}

fn create_utxos(
    ids: &[[u8; 32]],
    regular_note_keys: &mut Vec<ZkKey>,
    blend_notes: &mut Vec<ServiceNote>,
    sdp_notes: &mut Vec<ServiceNote>,
) -> Vec<Utxo> {
    let derive_key_material = |prefix: &[u8], id_bytes: &[u8]| -> [u8; 16] {
        let mut sk_data = [0; 16];
        let prefix_len = prefix.len();

        sk_data[..prefix_len].copy_from_slice(prefix);
        let remaining_len = 16 - prefix_len;
        sk_data[prefix_len..].copy_from_slice(&id_bytes[..remaining_len]);

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
            transfer_hash: BigUint::from(0u8).into(),
            output_index: 0,
        });
        output_index += 1;

        let sk_blend_data = derive_key_material(b"bn", &id);
        let sk_blend = ZkKey::from(BigUint::from_bytes_le(&sk_blend_data));
        let pk_blend = sk_blend.to_public_key();
        let note_blend = Note::new(1, pk_blend);
        let utxo = Utxo {
            note: note_blend,
            transfer_hash: BigUint::from(0u8).into(),
            output_index: 0,
        };
        blend_notes.push(ServiceNote {
            pk: pk_blend,
            sk: sk_blend,
            note: note_blend,
            note_id: utxo.id(),
            output_index,
        });
        utxos.push(utxo);
        output_index += 1;

        let sk_sdp_data = derive_key_material(b"sdp", &id);
        let sk_sdp = ZkKey::from(BigUint::from_bytes_le(&sk_sdp_data));
        let pk_sdp = sk_sdp.to_public_key();
        let note_sdp = Note::new(100, pk_sdp);
        let utxo = Utxo {
            note: note_sdp,
            transfer_hash: BigUint::from(0u8).into(),
            output_index,
        };
        sdp_notes.push(ServiceNote {
            pk: pk_sdp,
            sk: sk_sdp,
            note: note_sdp,
            note_id: utxo.id(),
            output_index,
        });
        utxos.push(utxo);
        output_index += 1;
    }

    utxos
}

#[must_use]
pub fn create_genesis_tx_with_declarations(
    transfer_op: TransferOp,
    providers: Vec<ProviderInfo>,
    test_context: Option<&str>,
) -> GenesisTx {
    let inscription = inscription_for_current_test(test_context);

    let transfer_hash = transfer_op.hash();

    let mut ops = vec![Op::Transfer(transfer_op), Op::ChannelInscribe(inscription)];

    for provider in &providers {
        let utxo = Utxo {
            transfer_hash,
            output_index: provider.note.output_index,
            note: provider.note.note,
        };
        let declaration = DeclarationMessage {
            service_type: provider.service_type,
            locators: vec![provider.locator.clone()],
            provider_id: provider.provider_id(),
            zk_id: provider.zk_id(),
            locked_note_id: utxo.id(),
        };
        ops.push(Op::SDPDeclare(declaration));
    }

    let mantle_tx = MantleTx {
        ops,
        execution_gas_price: 0.into(),
        storage_gas_price: 0.into(),
    };

    let mantle_tx_hash = mantle_tx.hash();
    let mut ops_proofs = vec![
        OpProof::ZkSig(ZkSignature::new(CompressedGroth16Proof::from_bytes(
            &[0u8; 128],
        ))),
        OpProof::NoProof,
    ];

    for provider in providers {
        let zk_sig =
            ZkKey::multi_sign(&[provider.note.sk, provider.zk_sk], mantle_tx_hash.as_ref())
                .unwrap();
        let ed25519_sig = provider
            .provider_sk
            .sign_payload(mantle_tx_hash.as_signing_bytes().as_ref());
        ops_proofs.push(OpProof::ZkAndEd25519Sigs {
            zk_sig,
            ed25519_sig,
        });
    }

    let signed_mantle_tx = SignedMantleTx {
        mantle_tx,
        ops_proofs,
    };

    GenesisTx::from_tx(signed_mantle_tx).expect("Invalid genesis transaction")
}
