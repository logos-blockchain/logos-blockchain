use core::time::Duration;

use lb_codec::BinaryEncode as _;
use lb_core::{
    block::genesis::{GenesisBlock, GenesisBlockBuilder},
    mantle::{
        CryptarchiaParameter, GenesisTime, Note, NoteId, OpProof, RawMantleTx, Utxo,
        ops::{
            Op, OpId as _, ZkAndEd25519Proof,
            channel::{
                ChannelId, Ed25519PublicKey, MsgId,
                inscribe::{Inscription, InscriptionOp},
            },
            transfer::TransferOp,
        },
        transactions::{GenesisTx, Ops, OpsProofs},
    },
    sdp::{DeclarationMessage, Locator, ProviderId, ServiceType},
};
use lb_groth16::{AdditiveGroup as _, CompressedGroth16Proof, Fr};
use lb_key_management_system_service::keys::{
    Ed25519Key, Ed25519Signature, ZkKey, ZkPublicKey, ZkSignature,
};
use lb_node::{Hashable as _, SignedMantleTx};
use num_bigint::BigUint;

use crate::unique::unique_test_context;

pub const SHORT_PROLONGED_BOOTSTRAP_PERIOD: Duration = Duration::from_secs(1);

pub const EMPTY_CHANNEL_ID: [u8; 32] = [0; 32];
pub const EMPTY_ED25519_PUBLIC_KEY: [u8; 32] = [0; 32];
const EMPTY_GROTH16_PROOF_BYTES: [u8; 128] = [0u8; 128];

const LEADER_KEY_PREFIX: &[u8] = b"ld";
const BLEND_KEY_PREFIX: &[u8] = b"bn";
const SDP_KEY_PREFIX: &[u8] = b"sdp";
const KEY_MATERIAL_LEN: usize = 16;

const REGULAR_NOTE_VALUE: u64 = 100_000;
const BLEND_NOTE_VALUE: u64 = 1;
/// Funds SDP declare/activity transaction fees at non-zero gas prices; an
/// activity transaction costs roughly 400-1000 at genesis prices.
const DEFAULT_SDP_FUNDING_VALUE_PER_NODE: u64 = 10_000;
const DEFAULT_SDP_FUNDING_NOTES_PER_NODE: usize = 1;

/// Test-tool mirror of the genesis transfer-output bound.
///
/// The bound is the transaction output bound used by `BoundedOutputs` in the
/// release core. Keeping this value in test tooling avoids exporting a
/// production constant solely for fixture validation.
pub const GENESIS_TRANSFER_OUTPUT_LIMIT: usize = u8::MAX as usize;

/// Controls how much SDP funding is allocated to each generated node and how
/// many outputs are used to hold that funding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SdpFundingConfig {
    pub total_value_per_node: u64,
    pub target_notes_per_node: usize,
}

impl SdpFundingConfig {
    #[must_use]
    pub const fn new(total_value_per_node: u64, target_notes_per_node: usize) -> Self {
        assert!(
            target_notes_per_node > 0,
            "SDP funding note count must be greater than zero"
        );
        Self {
            total_value_per_node,
            target_notes_per_node,
        }
    }
}

impl Default for SdpFundingConfig {
    fn default() -> Self {
        Self {
            total_value_per_node: DEFAULT_SDP_FUNDING_VALUE_PER_NODE,
            target_notes_per_node: DEFAULT_SDP_FUNDING_NOTES_PER_NODE,
        }
    }
}

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

fn select_sdp_funding_notes_per_node(
    node_count: usize,
    additional_wallet_outputs: usize,
    sdp_funding_config: SdpFundingConfig,
) -> usize {
    assert!(
        node_count > 0,
        "SDP funding allocation requires at least one node"
    );

    let fixed_node_outputs = node_count
        .checked_mul(2)
        .expect("node count overflow while calculating genesis outputs");
    let required_outputs = fixed_node_outputs
        .checked_add(additional_wallet_outputs)
        .expect("genesis output count overflow while calculating SDP funding capacity");
    let available_for_sdp = GENESIS_TRANSFER_OUTPUT_LIMIT
        .checked_sub(required_outputs)
        .unwrap_or_else(|| {
            panic!(
                "genesis transfer output capacity exhausted: limit={GENESIS_TRANSFER_OUTPUT_LIMIT}, node_count={node_count}, fixed_node_outputs={fixed_node_outputs}, additional_wallet_outputs={additional_wallet_outputs}",
            )
        });
    let max_sdp_notes_per_node = available_for_sdp / node_count;
    let selected_sdp_notes_per_node =
        max_sdp_notes_per_node.min(sdp_funding_config.target_notes_per_node);

    assert!(
        selected_sdp_notes_per_node > 0,
        "genesis transfer output capacity cannot provide one SDP funding note per node: limit={GENESIS_TRANSFER_OUTPUT_LIMIT}, node_count={node_count}, fixed_node_outputs={fixed_node_outputs}, additional_wallet_outputs={additional_wallet_outputs}",
    );

    let selected_as_u64 = u64::try_from(selected_sdp_notes_per_node)
        .expect("SDP funding split count should fit in u64");
    let base_sdp_note_value = sdp_funding_config.total_value_per_node / selected_as_u64;
    let sdp_value_remainder = sdp_funding_config.total_value_per_node % selected_as_u64;
    let sdp_outputs = node_count
        .checked_mul(selected_sdp_notes_per_node)
        .expect("SDP funding output count overflow");
    let projected_genesis_outputs = required_outputs
        .checked_add(sdp_outputs)
        .expect("genesis output count overflow after SDP funding allocation");

    assert!(
        projected_genesis_outputs <= GENESIS_TRANSFER_OUTPUT_LIMIT,
        "projected genesis transfer outputs exceed capacity: projected={projected_genesis_outputs}, limit={GENESIS_TRANSFER_OUTPUT_LIMIT}",
    );

    println!(
        "SDP funding allocation genesis_transfer_output_limit={GENESIS_TRANSFER_OUTPUT_LIMIT} node_count={node_count} fixed_node_outputs={fixed_node_outputs} additional_wallet_outputs={additional_wallet_outputs} requested_sdp_notes_per_node={} selected_sdp_notes_per_node={selected_sdp_notes_per_node} sdp_note_value={base_sdp_note_value} sdp_value_remainder={sdp_value_remainder} total_sdp_funding_per_node={}",
        sdp_funding_config.target_notes_per_node, sdp_funding_config.total_value_per_node,
    );

    selected_sdp_notes_per_node
}

fn inscription_for_current_test(
    test_context: Option<&str>,
    genesis_time: GenesisTime,
) -> InscriptionOp {
    let chain_id = unique_test_context(test_context);
    println!("Genesis inscription: {chain_id}, genesis_time: {genesis_time:?}");
    InscriptionOp {
        channel_id: ChannelId::from(EMPTY_CHANNEL_ID),
        inscription: Inscription::new_unchecked(
            CryptarchiaParameter {
                chain_id,
                genesis_time,
                epoch_nonce: Fr::ZERO,
            }
            .encode_to_vec(),
        ),
        parent: MsgId::root(),
        signer: Ed25519PublicKey::from_bytes(&EMPTY_ED25519_PUBLIC_KEY).unwrap(),
    }
}

#[must_use]
pub fn create_genesis_block(
    utxos: &[Utxo],
    test_context: Option<&str>,
    genesis_time: GenesisTime,
) -> GenesisBlock {
    // Create transfer op with the utxos as outputs
    let mut outputs = utxos.iter().map(|u| u.note);
    #[expect(
        clippy::option_if_let_else,
        reason = "Moving notes inside of consuming lambda function is harder to read"
    )]
    let genesis_builder = if let Some(note) = outputs.next() {
        let mut genesis_builder = GenesisBlockBuilder::new().add_note(note);
        for note in outputs {
            genesis_builder = genesis_builder
                .try_add_note(note)
                .expect("note count must fit in genesis transfer outputs");
        }
        genesis_builder
    } else {
        panic!("No outputs provided for genesis block")
    };

    let inscription = inscription_for_current_test(test_context, genesis_time);

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
    genesis_time: GenesisTime,
) -> (Vec<GeneralConsensusConfig>, GenesisBlock) {
    create_consensus_configs_with_additional_wallet_outputs(
        ids,
        prolonged_bootstrap_period,
        test_context,
        0,
        genesis_time,
    )
}

#[must_use]
pub fn create_consensus_configs_with_additional_wallet_outputs(
    ids: &[[u8; 32]],
    prolonged_bootstrap_period: Duration,
    test_context: Option<&str>,
    additional_wallet_outputs: usize,
    genesis_time: GenesisTime,
) -> (Vec<GeneralConsensusConfig>, GenesisBlock) {
    create_consensus_configs_with_additional_wallet_outputs_and_sdp_funding_config(
        ids,
        prolonged_bootstrap_period,
        test_context,
        additional_wallet_outputs,
        SdpFundingConfig::default(),
        genesis_time,
    )
}

#[must_use]
pub fn create_consensus_configs_with_additional_wallet_outputs_and_sdp_funding_config(
    ids: &[[u8; 32]],
    prolonged_bootstrap_period: Duration,
    test_context: Option<&str>,
    additional_wallet_outputs: usize,
    sdp_funding_config: SdpFundingConfig,
    genesis_time: GenesisTime,
) -> (Vec<GeneralConsensusConfig>, GenesisBlock) {
    let material =
        create_base_consensus_material_with_additional_wallet_outputs_and_sdp_funding_config(
            ids,
            additional_wallet_outputs,
            sdp_funding_config,
        );
    let genesis_block = create_genesis_block(&material.utxos, test_context, genesis_time);

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
    create_base_consensus_material_with_additional_wallet_outputs(ids, 0)
}

#[must_use]
pub fn create_base_consensus_material_with_additional_wallet_outputs(
    ids: &[[u8; 32]],
    additional_wallet_outputs: usize,
) -> BaseConsensusMaterial {
    create_base_consensus_material_with_additional_wallet_outputs_and_sdp_funding_config(
        ids,
        additional_wallet_outputs,
        SdpFundingConfig::default(),
    )
}

#[must_use]
pub fn create_base_consensus_material_with_additional_wallet_outputs_and_sdp_funding_config(
    ids: &[[u8; 32]],
    additional_wallet_outputs: usize,
    sdp_funding_config: SdpFundingConfig,
) -> BaseConsensusMaterial {
    let mut regular_note_keys = Vec::new();
    let mut blend_notes = Vec::new();
    let mut sdp_notes = Vec::new();
    let utxos = create_utxos(
        ids,
        &mut regular_note_keys,
        &mut blend_notes,
        &mut sdp_notes,
        additional_wallet_outputs,
        sdp_funding_config,
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
    additional_wallet_outputs: usize,
    sdp_funding_config: SdpFundingConfig,
) -> Vec<Utxo> {
    if ids.is_empty() {
        return Vec::new();
    }

    let derive_key_material = |prefix: &[u8], id_bytes: &[u8]| -> [u8; 16] {
        let mut sk_data = [0; KEY_MATERIAL_LEN];
        let prefix_len = prefix.len();

        sk_data[..prefix_len].copy_from_slice(prefix);
        let remaining_len = KEY_MATERIAL_LEN - prefix_len;
        sk_data[prefix_len..].copy_from_slice(&id_bytes[..remaining_len]);

        sk_data
    };

    let sdp_notes_per_node =
        select_sdp_funding_notes_per_node(ids.len(), additional_wallet_outputs, sdp_funding_config);
    let sdp_notes_per_node_u64 =
        u64::try_from(sdp_notes_per_node).expect("SDP funding split count should fit in u64");
    let base_sdp_note_value = sdp_funding_config.total_value_per_node / sdp_notes_per_node_u64;
    let sdp_value_remainder =
        usize::try_from(sdp_funding_config.total_value_per_node % sdp_notes_per_node_u64)
            .expect("SDP funding value remainder should fit in usize");

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
        for sdp_note_index in 0..sdp_notes_per_node {
            let note_value = base_sdp_note_value + u64::from(sdp_note_index < sdp_value_remainder);
            let note_sdp = Note::new(note_value, pk_sdp);
            let utxo = Utxo {
                note: note_sdp,
                op_id: [0u8; 32],
                output_index,
            };
            if sdp_note_index == 0 {
                sdp_notes.push(ServiceNote {
                    pk: pk_sdp,
                    sk: sk_sdp.clone(),
                    note: note_sdp,
                    note_id: utxo.id(),
                    output_index,
                });
            }
            utxos.push(utxo);
            output_index += 1;
        }
    }

    utxos
}

#[must_use]
pub fn create_genesis_block_with_declarations(
    transfer_op: TransferOp,
    providers: Vec<ProviderInfo>,
    test_context: Option<&str>,
    genesis_time: GenesisTime,
) -> GenesisBlock {
    let inscription = inscription_for_current_test(test_context, genesis_time);
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
            locators: provider.locator.clone().into(),
            provider_id: provider.provider_id(),
            zk_id: provider.zk_id(),
            locked_note_id: utxo.id(),
        };
        ops.push(Op::SDPDeclare(declaration));
    }

    let mantle_tx = RawMantleTx(Ops::new_unchecked(ops));

    let mantle_tx_hash = mantle_tx.hash();
    let mut ops_proofs = OpsProofs::from([
        OpProof::ZkSig(ZkSignature::new(CompressedGroth16Proof::from_bytes(
            &EMPTY_GROTH16_PROOF_BYTES,
        ))),
        OpProof::Ed25519Sig(Ed25519Signature::zero()),
    ]);

    for provider in providers {
        let zk_sig =
            ZkKey::multi_sign(&[provider.note.sk, provider.zk_sk], &mantle_tx_hash.to_fr())
                .unwrap();
        let ed25519_sig = provider
            .provider_sk
            .sign_payload(mantle_tx_hash.as_signing_bytes().as_ref());
        let proof = ZkAndEd25519Proof {
            zk_sig,
            ed25519_sig,
        };
        ops_proofs
            .try_push(OpProof::ZkAndEd25519Sigs(proof))
            .expect("genesis transaction proofs are bounded");
    }

    let signed_mantle_tx = SignedMantleTx::new_trusted(mantle_tx, ops_proofs);

    // TODO: Maybe use the builder instead of trusting the signed mantle tx
    GenesisBlockBuilder::new()
        .with_genesis_tx(GenesisTx::from_tx(signed_mantle_tx).expect("Genesis tx should build"))
        .build()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn sdp_outputs(material: &BaseConsensusMaterial) -> Vec<Note> {
        let funding_keys = material
            .sdp_notes
            .iter()
            .map(|note| note.pk)
            .collect::<HashSet<_>>();
        material
            .utxos
            .iter()
            .filter(|utxo| funding_keys.contains(&utxo.note.pk))
            .map(|utxo| utxo.note)
            .collect()
    }

    #[test]
    fn default_sdp_funding_keeps_one_ten_thousand_note_per_node() {
        let ids = [[1; 32], [2; 32]];
        let material = create_base_consensus_material_with_additional_wallet_outputs(&ids, 0);

        let outputs = sdp_outputs(&material);
        assert_eq!(SdpFundingConfig::default().total_value_per_node, 10_000);
        assert_eq!(SdpFundingConfig::default().target_notes_per_node, 1);
        assert_eq!(outputs.len(), ids.len());
        assert!(outputs.iter().all(|note| note.value == 10_000));
    }

    #[test]
    fn configured_sdp_funding_splits_each_provider_total_across_requested_notes() {
        let ids = [[1; 32], [2; 32]];
        let config = SdpFundingConfig::new(10_000_000, 5);
        let material =
            create_base_consensus_material_with_additional_wallet_outputs_and_sdp_funding_config(
                &ids, 0, config,
            );

        let outputs = sdp_outputs(&material);
        assert_eq!(outputs.len(), ids.len() * 5);
        for provider_outputs in outputs.chunks(5) {
            assert_eq!(
                provider_outputs.iter().map(|note| note.value).sum::<u64>(),
                10_000_000
            );
        }
    }

    #[test]
    fn configured_sdp_funding_caps_note_count_without_losing_provider_total() {
        let ids = (0..50).map(|id| [id; 32]).collect::<Vec<_>>();
        let material =
            create_base_consensus_material_with_additional_wallet_outputs_and_sdp_funding_config(
                &ids,
                0,
                SdpFundingConfig::new(10_000_000, 5),
            );

        let outputs = sdp_outputs(&material);
        assert_eq!(outputs.len(), ids.len() * 3);
        for provider_outputs in outputs.chunks(3) {
            assert_eq!(
                provider_outputs.iter().map(|note| note.value).sum::<u64>(),
                10_000_000
            );
        }
    }
}
