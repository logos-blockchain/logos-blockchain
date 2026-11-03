use std::borrow::Cow;

use ark_ff::AdditiveGroup as _;
use lb_binary_codec::canonical::{CodecFixtures, decode_fixture_hex};
use lb_groth16::{CompressedGroth16Proof, Fr};
use lb_key_management_system_keys::keys::{Ed25519Signature, UnverifiedEd25519PublicKey};

use crate::mantle::{
    fixtures::{
        ops::op_values::{
            ALL_OPS_COLUMN_HEX, CHANNEL_CONFIG, CHANNEL_WITHDRAW, CLAIM_POW_REWARD, DEPOSIT,
            INSCRIPTION, SDP_ACTIVE, SDP_DECLARE, SDP_WITHDRAW, TRANSFER,
        },
        proofs::proof_values::{
            CHANNEL_MULTI_SIG, CHANNEL_MULTI_SIG_HEX, ED25519_SIG, ED25519_SIG_HEX,
            PROGRAM_STEP_PROOF, PROGRAM_STEP_PROOF_HEX, ZK_AND_ED25519_SIGS,
            ZK_AND_ED25519_SIGS_HEX, ZK_TRANSFER, ZK_TRANSFER_HEX,
        },
    },
    ledger::verification_mode::VerificationMode,
    notes::{NoteNullifier, OutputCommitments},
    ops::{
        ChannelInscribeProof, NoOpProof, SignedOp, SignedOperation, StepProof, ZkTransferProof,
        channel::{
            ChannelId, MsgId,
            inscribe::{ChannelSteps, Inscription, InscriptionOp},
        },
        transfer::TransferOp,
    },
    transactions::{
        SignedOps,
        states::{Unverified, VerificationState},
    },
};

impl<State: VerificationState, Mode: VerificationMode> lb_binary_codec::canonical::sealed::Sealed
    for SignedOps<State, Mode>
{
}

const TWO_OPS_HEX: &str = concat!(
    // Count. One, for both columns.
    "02",
    // Op 0: Transfer.
    "00",                                                               // code
    "01",                                                               // one input
    "0000000000000000000000000000000000000000000000000000000000000000", // NoteNullifier
    "00",                                                               // no outputs
    "0000000000000000000000000000000000000000000000000000000000000000", // CommitmentMerkleRoot
    "0000000000000000",                                                 // Value
    // Op 1: ChannelInscribe.
    "11",                                                               // code
    "0000000000000000000000000000000000000000000000000000000000000000", // channel_id
    "00000000",                                                         // empty inscription
    "0000000000000000000000000000000000000000000000000000000000000000", // parent
    "0101010101010101010101010101010101010101010101010101010101010101", // signer
    "0000",                                                             // no steps
    // Proof 0: ZkTransferProof, for op 0.
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    // Proof 1: Ed25519Signature, for op 1.
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
);

fn two_ops<State: VerificationState, Mode: VerificationMode>() -> SignedOps<State, Mode> {
    let transfer = SignedOperation::<TransferOp, Unverified, Mode>::new(
        TransferOp::new(
            [NoteNullifier(Fr::ZERO)].into(),
            OutputCommitments::default(),
            Fr::ZERO,
            0,
        ),
        ZkTransferProof::new(CompressedGroth16Proof::from_bytes(&[0xAAu8; 128])),
    );
    let inscription = SignedOperation::<InscriptionOp, Unverified, Mode>::new(
        InscriptionOp {
            channel_id: ChannelId::from([0u8; 32]),
            inscription: Inscription::default(),
            parent: MsgId::root(),
            signer: UnverifiedEd25519PublicKey::from_bytes(&[1u8; 32]).unwrap(),
            steps: ChannelSteps::default(),
        },
        ChannelInscribeProof::without_steps(Ed25519Signature::from_bytes(&[0xBBu8; 64])),
    );

    SignedOps::from([
        SignedOp::Transfer(transfer.into_state_trusted()),
        SignedOp::ChannelInscribe(inscription.into_state_trusted()),
    ])
}

fn all_ops_hex() -> String {
    [
        ALL_OPS_COLUMN_HEX,
        ZK_TRANSFER_HEX,
        CHANNEL_MULTI_SIG_HEX,
        ED25519_SIG_HEX,
        ZK_TRANSFER_HEX,
        PROGRAM_STEP_PROOF_HEX,
        ZK_TRANSFER_HEX,
        ZK_TRANSFER_HEX,
        ZK_AND_ED25519_SIGS_HEX,
        ZK_AND_ED25519_SIGS_HEX,
        ED25519_SIG_HEX,
    ]
    .concat()
}

fn all_ops<State: VerificationState, Mode: VerificationMode>() -> SignedOps<State, Mode> {
    macro_rules! signed {
        ($variant:ident, $op:ident, $proof:expr) => {
            SignedOp::$variant(SignedOperation::new($op.clone(), $proof).into_state_trusted())
        };
    }

    SignedOps::from([
        signed!(Transfer, TRANSFER, *ZK_TRANSFER),
        signed!(ChannelConfig, CHANNEL_CONFIG, CHANNEL_MULTI_SIG.clone()),
        signed!(
            ChannelInscribe,
            INSCRIPTION,
            ChannelInscribeProof::new(
                *ED25519_SIG,
                vec![
                    StepProof::User(*ZK_TRANSFER),
                    StepProof::Program(PROGRAM_STEP_PROOF.clone())
                ]
            )
        ),
        signed!(ChannelDeposit, DEPOSIT, *ZK_TRANSFER),
        signed!(ChannelWithdraw, CHANNEL_WITHDRAW, *ZK_TRANSFER),
        signed!(SDPDeclare, SDP_DECLARE, ZK_AND_ED25519_SIGS.clone()),
        signed!(SDPWithdraw, SDP_WITHDRAW, ZK_AND_ED25519_SIGS.clone()),
        signed!(SDPActive, SDP_ACTIVE, *ED25519_SIG),
        signed!(ClaimPowReward, CLAIM_POW_REWARD, NoOpProof),
    ])
}

impl<State: VerificationState, Mode: VerificationMode> lb_binary_codec::canonical::CodecExamples
    for SignedOps<State, Mode>
{
    fn fixtures() -> CodecFixtures<Self> {
        [
            lb_binary_codec::canonical::CodecFixture {
                value: Self::empty(),
                bytes: Cow::Borrowed(&[0x00]),
            },
            lb_binary_codec::canonical::CodecFixture {
                value: two_ops(),
                bytes: Cow::Owned(decode_fixture_hex(TWO_OPS_HEX)),
            },
            lb_binary_codec::canonical::CodecFixture {
                value: all_ops(),
                bytes: Cow::Owned(decode_fixture_hex(&all_ops_hex())),
            },
        ]
        .into()
    }
}

#[cfg(test)]
mod tests {
    use crate::mantle::{
        ledger::verification_mode::StandardMode,
        transactions::{SignedOps, states::Unverified},
    };

    #[test]
    fn codec_fixtures_round_trip() {
        lb_binary_codec::canonical::assert_codec_fixtures::<SignedOps<Unverified, StandardMode>>();
    }
}
