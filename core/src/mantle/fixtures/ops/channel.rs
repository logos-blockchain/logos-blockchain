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
            inscribe::{ChannelStep, Inscription, InscriptionOp, MiniBlocks, Step},
            program_step::{ProgramDeposit, ProgramStep},
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
        mini_blocks: MiniBlocks::default(),
    } => "00000000000000000000000000000000000000000000000000000000000000000700000067656e65736973000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    INSCRIPTION.clone() => INSCRIPTION_PAYLOAD_HEX,
);
codec_fixtures!(
    ChannelStep,
    Self { inputs: [NoteNullifier(Fr::ONE)].into(), outputs: OutputCommitments::default(), commitment_merkle_root: Fr::ZERO, inscription: Inscription::default() }
        => "01010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
);
// `ProgramDeposit::sample` and `ProgramStep::sample` only exist with
// `test-utils`, the examples always do.
fn program_deposit() -> ProgramDeposit {
    ProgramDeposit {
        program_id: Fr::from(0x34u64),
        inputs: [NoteNullifier(Fr::from(0x35u64))].into(),
        outputs: [NoteCommitment(Fr::from(0x36u64))].into(),
        commitment_merkle_root: Fr::from(0x37u64),
        value: 0x38,
        intent: Fr::from(0x39u64),
        inscription: b"deposit-inscription".into(),
    }
}

fn program_step() -> ProgramStep {
    ProgramStep {
        program_id: Fr::from(0x34u64),
        inputs: [NoteNullifier(Fr::from(0x3au64))].into(),
        program_outputs: [NoteCommitment(Fr::from(0x3bu64))].into(),
        outputs: OutputCommitments::default(),
        commitment_merkle_root: Fr::from(0x3cu64),
        program_step_id: Fr::from(0x3du64),
        inscription: b"program-step-inscription".into(),
    }
}

codec_fixtures!(
    ProgramDeposit,
    program_deposit() => "3400000000000000000000000000000000000000000000000000000000000000013500000000000000000000000000000000000000000000000000000000000000013600000000000000000000000000000000000000000000000000000000000000370000000000000000000000000000000000000000000000000000000000000038000000000000003900000000000000000000000000000000000000000000000000000000000000130000006465706f7369742d696e736372697074696f6e",
);
codec_fixtures!(
    ProgramStep,
    program_step() => "3400000000000000000000000000000000000000000000000000000000000000013a00000000000000000000000000000000000000000000000000000000000000013b00000000000000000000000000000000000000000000000000000000000000003c000000000000000000000000000000000000000000000000000000000000003d000000000000000000000000000000000000000000000000000000000000001800000070726f6772616d2d737465702d696e736372697074696f6e",
);
codec_fixtures!(
    Step,
    Step::User(ChannelStep { inputs: [NoteNullifier(Fr::ONE)].into(), outputs: OutputCommitments::default(), commitment_merkle_root: Fr::ZERO, inscription: Inscription::default() })
        => "0001010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    Step::ProgramDeposit(program_deposit()) => "013400000000000000000000000000000000000000000000000000000000000000013500000000000000000000000000000000000000000000000000000000000000013600000000000000000000000000000000000000000000000000000000000000370000000000000000000000000000000000000000000000000000000000000038000000000000003900000000000000000000000000000000000000000000000000000000000000130000006465706f7369742d696e736372697074696f6e",
    Step::Program(program_step()) => "023400000000000000000000000000000000000000000000000000000000000000013a00000000000000000000000000000000000000000000000000000000000000013b00000000000000000000000000000000000000000000000000000000000000003c000000000000000000000000000000000000000000000000000000000000003d000000000000000000000000000000000000000000000000000000000000001800000070726f6772616d2d737465702d696e736372697074696f6e",
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
