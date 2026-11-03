use std::num::NonZero;

use lb_binary_codec::canonical::codec_fixtures;
use lb_groth16::{AdditiveGroup as _, Field as _, Fr};
use lb_key_management_system_keys::keys::{
    Ed25519PublicKey, Ed25519Signature, UnverifiedEd25519PublicKey,
};

use crate::{
    mantle::{
        channel::{SlotTimeframe, SlotTimeout},
        fixtures::ops::op_values::{
            CHANNEL_CONFIG, CHANNEL_CONFIG_PAYLOAD_HEX, CHANNEL_WITHDRAW,
            CHANNEL_WITHDRAW_PAYLOAD_HEX, DEPOSIT, DEPOSIT_PAYLOAD_HEX, INSCRIPTION,
            INSCRIPTION_PAYLOAD_HEX,
        },
        notes::{InputNullifiers, NoteCommitment, NoteNullifier, OutputCommitments},
        ops::channel::{
            ChannelId, MsgId,
            config::ChannelConfigOp,
            deposit::{DepositOp, Metadata},
            inscribe::{ChannelStep, ChannelSteps, InscriptionOp, Step},
            program_step::{Payment, ProgramOutputs, ProgramStep},
            withdraw::ChannelWithdrawOp,
        },
    },
    proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignatures},
};

codec_fixtures!(ChannelId, Self::from([0u8; 32]) => "0000000000000000000000000000000000000000000000000000000000000000");
codec_fixtures!(MsgId, Self::from([0u8; 32]) => "0000000000000000000000000000000000000000000000000000000000000000");
codec_fixtures!(
    ChannelConfigOp,
    Self::new(
        ChannelId::from([0u8; 32]),
        MsgId::from([0u8; 32]),
        [Ed25519PublicKey::from_bytes(&[1u8; _]).unwrap()].into(),
        SlotTimeframe::from(0u32),
        SlotTimeout::from(0u32),
        NonZero::<u16>::MIN,
    )
    .unwrap() => "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000100010101010101010101010101010101010101010101010101010101010101010100000000000000000100",
    CHANNEL_CONFIG.clone() => CHANNEL_CONFIG_PAYLOAD_HEX,
);
codec_fixtures!(
    DepositOp,
    Self {
        channel_id: ChannelId::from([0u8; 32]),
        inputs: InputNullifiers::default(),
        commitment_merkle_root: Fr::ZERO,
        outputs: OutputCommitments::default(),
        excess_value: 0,
        metadata: Metadata::empty(),
    } => "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    DEPOSIT.clone() => DEPOSIT_PAYLOAD_HEX,
);
codec_fixtures!(
    InscriptionOp,
    Self {
        channel_id: ChannelId::from([0u8; 32]),
        inscription: b"genesis".into(),
        parent: MsgId::from([0u8; 32]),
        signer: UnverifiedEd25519PublicKey::from_bytes(&[0u8; _]).unwrap(),
        steps: ChannelSteps::default(),
    } => "00000000000000000000000000000000000000000000000000000000000000000700000067656e65736973000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    INSCRIPTION.clone() => INSCRIPTION_PAYLOAD_HEX,
);
codec_fixtures!(
    ChannelStep,
    Self { inputs: [NoteNullifier(Fr::ONE)].into(), outputs: OutputCommitments::default(), commitment_merkle_root: Fr::ZERO, buffered: 2 }
        => "0101000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000200",
);
// `Payment::sample` only exists with `test-utils`, the examples always do.
fn payment() -> Payment {
    Payment {
        inputs: [NoteNullifier(Fr::from(0x35u64))].into(),
        outputs: [NoteCommitment(Fr::from(0x36u64))].into(),
        commitment_merkle_root: Fr::from(0x37u64),
        buffered: 0x47,
        paid_value: 0x38,
        intent: Fr::from(0x39u64),
    }
}

codec_fixtures!(
    Payment,
    payment() => "0135000000000000000000000000000000000000000000000000000000000000000136000000000000000000000000000000000000000000000000000000000000003700000000000000000000000000000000000000000000000000000000000000470038000000000000003900000000000000000000000000000000000000000000000000000000000000",
);
codec_fixtures!(ProgramOutputs, ProgramOutputs::from(0b101) => "05");
codec_fixtures!(
    ProgramStep,
    ProgramStep {
        program_id: Fr::from(0x34u64),
        payments: [payment()].into(),
        inputs: [NoteNullifier(Fr::from(0x3au64))].into(),
        outputs: [NoteCommitment(Fr::from(0x3bu64))].into(),
        program_outputs: ProgramOutputs::from(0b1),
        commitment_merkle_root: Fr::from(0x3cu64),
        spend_notes_hash: Fr::from(0x3du64),
    } => "3400000000000000000000000000000000000000000000000000000000000000010135000000000000000000000000000000000000000000000000000000000000000136000000000000000000000000000000000000000000000000000000000000003700000000000000000000000000000000000000000000000000000000000000470038000000000000003900000000000000000000000000000000000000000000000000000000000000013a00000000000000000000000000000000000000000000000000000000000000013b00000000000000000000000000000000000000000000000000000000000000013c000000000000000000000000000000000000000000000000000000000000003d00000000000000000000000000000000000000000000000000000000000000",
);
codec_fixtures!(
    Step,
    Step::User(ChannelStep { inputs: [NoteNullifier(Fr::ONE)].into(), outputs: OutputCommitments::default(), commitment_merkle_root: Fr::ZERO, buffered: 2 })
        => "000101000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000200",
);
codec_fixtures!(
    ChannelWithdrawOp,
    Self {
        channel_id: ChannelId::from([0u8; 32]),
        inputs: InputNullifiers::default(),
        commitment_merkle_root: Fr::ZERO,
        outputs: OutputCommitments::default(),
    } => "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    CHANNEL_WITHDRAW.clone() => CHANNEL_WITHDRAW_PAYLOAD_HEX,
);

codec_fixtures!(ChannelMultiSigProof,
    Self::empty() => "0000",
    Self::new(IndexedSignatures::from((1, Ed25519Signature::from_bytes(&[0u8; _])))) => "0100010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
);
