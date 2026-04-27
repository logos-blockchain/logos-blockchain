use core::time::Duration;

use lb_core::{
    block::genesis::{GenesisBlock, GenesisBlockBuilder},
    mantle::{
        MantleTx, Note, NoteId, OpProof, Utxo,
        genesis_tx::{GENESIS_EXECUTION_GAS_PRICE, GENESIS_STORAGE_GAS_PRICE, GenesisTx},
        ops::{
            Op, OpId as _,
            channel::{ChannelId, Ed25519PublicKey, MsgId, inscribe::InscriptionOp},
            transfer::TransferOp,
        },
    },
    sdp::{DeclarationMessage, Locator, ProviderId, ServiceType},
};
use lb_groth16::CompressedGroth16Proof;
use lb_key_management_system_service::keys::{
    Ed25519Key, Ed25519Signature, ZkKey, ZkPublicKey, ZkSignature,
};
use lb_node::{SignedMantleTx, Transaction as _};
use num_bigint::BigUint;

use crate::unique::unique_test_context;

pub const SHORT_PROLONGED_BOOTSTRAP_PERIOD: Duration = Duration::from_secs(1);

const EMPTY_CHANNEL_ID: [u8; 32] = [0; 32];
const EMPTY_ED25519_PUBLIC_KEY: [u8; 32] = [0; 32];
const EMPTY_GROTH16_PROOF_BYTES: [u8; 128] = [0u8; 128];

const LEADER_KEY_PREFIX: &[u8] = b"ld";
const BLEND_KEY_PREFIX: &[u8] = b"bn";
const SDP_KEY_PREFIX: &[u8] = b"sdp";
const KEY_MATERIAL_LEN: usize = 16;

const REGULAR_NOTE_VALUE: u64 = 100_000;
const BLEND_NOTE_VALUE: u64 = 1;
const SDP_NOTE_VALUE: u64 = 100;

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

pub struct BaseConsensusMaterial {
    pub regular_note_keys: Vec<ZkKey>,
    pub blend_notes: Vec<ServiceNote>,
    pub sdp_notes: Vec<ServiceNote>,
    pub utxos: Vec<Utxo>,
}

fn inscription_for_current_test(test_context: Option<&str>) -> InscriptionOp {
    let owner = unique_test_context(test_context);
    println!("Genesis inscription: {owner}");
    InscriptionOp {
        channel_id: ChannelId::from(EMPTY_CHANNEL_ID),
        inscription: owner.into_bytes(),
        parent: MsgId::root(),
        signer: Ed25519PublicKey::from_bytes(&EMPTY_ED25519_PUBLIC_KEY).unwrap(),
    }
}

#[must_use]
pub fn create_genesis_block(utxos: &[Utxo], test_context: Option<&str>) -> GenesisBlock {
    // Create transfer op with the utxos as outputs
    let mut outputs = utxos.iter().map(|u| u.note);
    #[expect(
        clippy::option_if_let_else,
        reason = "Moving notes inside of consuming lambda function is harder to read"
    )]
    let genesis_builder = if let Some(note) = outputs.next() {
        let mut genesis_builder = GenesisBlockBuilder::new().add_note(note);
        for note in outputs {
            genesis_builder = genesis_builder.add_note(note);
        }
        genesis_builder
    } else {
        panic!("No outputs provided for genesis block")
    };

    let inscription = inscription_for_current_test(test_context);

    genesis_builder
        .set_inscription(inscription)
        .build()
        .expect("Genesis block shoudl build properly")
}

#[must_use]
pub fn create_consensus_configs(
    ids: &[[u8; 32]],
    prolonged_bootstrap_period: Duration,
    test_context: Option<&str>,
) -> (Vec<GeneralConsensusConfig>, GenesisBlock) {
    let material = create_base_consensus_material(ids);
    let genesis_block = create_genesis_block(&material.utxos, test_context);

    (
        material
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
            .collect(),
        genesis_block,
    )
}

#[must_use]
pub fn create_base_consensus_material(ids: &[[u8; 32]]) -> BaseConsensusMaterial {
    let mut regular_note_keys = Vec::new();
    let mut blend_notes = Vec::new();
    let mut sdp_notes = Vec::new();
    let utxos = create_utxos(
        ids,
        &mut regular_note_keys,
        &mut blend_notes,
        &mut sdp_notes,
    );

    BaseConsensusMaterial {
        regular_note_keys,
        blend_notes,
        sdp_notes,
        utxos,
    }
}

fn create_utxos(
    ids: &[[u8; 32]],
    regular_note_keys: &mut Vec<ZkKey>,
    blend_notes: &mut Vec<ServiceNote>,
    sdp_notes: &mut Vec<ServiceNote>,
) -> Vec<Utxo> {
    let derive_key_material = |prefix: &[u8], id_bytes: &[u8]| -> [u8; 16] {
        let mut sk_data = [0; KEY_MATERIAL_LEN];
        let prefix_len = prefix.len();

        sk_data[..prefix_len].copy_from_slice(prefix);
        let remaining_len = KEY_MATERIAL_LEN - prefix_len;
        sk_data[prefix_len..].copy_from_slice(&id_bytes[..remaining_len]);

        sk_data
    };

    let mut utxos = Vec::new();
    let mut output_index = 0;

    for &id in ids {
        let sk_data = derive_key_material(LEADER_KEY_PREFIX, &id);
        let sk = ZkKey::from(BigUint::from_bytes_le(&sk_data));
        let pk = sk.to_public_key();
        regular_note_keys.push(sk);
        utxos.push(Utxo {
            note: Note::new(REGULAR_NOTE_VALUE, pk),
            op_id: [0u8; 32],
            output_index: 0,
        });
        output_index += 1;

        let sk_blend_data = derive_key_material(BLEND_KEY_PREFIX, &id);
        let sk_blend = ZkKey::from(BigUint::from_bytes_le(&sk_blend_data));
        let pk_blend = sk_blend.to_public_key();
        let note_blend = Note::new(BLEND_NOTE_VALUE, pk_blend);
        let utxo = Utxo {
            note: note_blend,
            op_id: [0u8; 32],
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

        let sk_sdp_data = derive_key_material(SDP_KEY_PREFIX, &id);
        let sk_sdp = ZkKey::from(BigUint::from_bytes_le(&sk_sdp_data));
        let pk_sdp = sk_sdp.to_public_key();
        let note_sdp = Note::new(SDP_NOTE_VALUE, pk_sdp);
        let utxo = Utxo {
            note: note_sdp,
            op_id: [0u8; 32],
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
pub fn create_genesis_block_with_declarations(
    transfer_op: TransferOp,
    providers: Vec<ProviderInfo>,
    test_context: Option<&str>,
) -> GenesisBlock {
    let inscription = inscription_for_current_test(test_context);
    let transfer_id = transfer_op.op_id();

    let mut ops = vec![Op::Transfer(transfer_op), Op::ChannelInscribe(inscription)];

    for provider in &providers {
        let utxo = Utxo {
            op_id: transfer_id,
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
        execution_gas_price: GENESIS_EXECUTION_GAS_PRICE,
        storage_gas_price: GENESIS_STORAGE_GAS_PRICE,
    };

    let mantle_tx_hash = mantle_tx.hash();
    let mut ops_proofs = vec![
        OpProof::ZkSig(ZkSignature::new(CompressedGroth16Proof::from_bytes(
            &EMPTY_GROTH16_PROOF_BYTES,
        ))),
        OpProof::Ed25519Sig(Ed25519Signature::zero()),
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

    // TODO: Maybe use the builder instead of trusting the signed mantle tx
    GenesisBlockBuilder::new()
        .with_genesis_tx(GenesisTx::from_tx(signed_mantle_tx).expect("Genesis tx should build"))
        .build()
}
