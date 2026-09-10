use std::sync::LazyLock;

use ark_ff::AdditiveGroup as _;
use lb_codec::codec_fixtures;
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::{Ed25519PublicKey, ZkPublicKey};

use crate::mantle::{
    NoteId,
    ledger::Outputs,
    ops::{
        OpRef,
        channel::{
            ChannelId, MsgId,
            inscribe::{Inscription, InscriptionOp},
        },
        pow::ClaimPowRewardOp,
        transfer::TransferOp,
    },
};

static TRANSFER: LazyLock<TransferOp> = LazyLock::new(|| TransferOp {
    inputs: [NoteId(Fr::ZERO)].into(),
    outputs: Outputs::empty(),
});

static INSCRIPTION: LazyLock<InscriptionOp> = LazyLock::new(|| InscriptionOp {
    channel_id: ChannelId::from([0u8; 32]),
    inscription: Inscription::default(),
    parent: MsgId::root(),
    signer: Ed25519PublicKey::from_bytes(&[1u8; 32]).unwrap(),
});

static CLAIM_POW_REWARD: LazyLock<ClaimPowRewardOp> = LazyLock::new(|| ClaimPowRewardOp {
    epoch_nonce: Fr::ZERO,
    block_hash: [0u8; 32],
    public_key: ZkPublicKey::new(Fr::ZERO),
});

codec_fixtures!(
    OpRef<'_>,
    encode_only,
    OpRef::Transfer(&TRANSFER) => "0001000000000000000000000000000000000000000000000000000000000000000000",
    OpRef::ChannelInscribe(&INSCRIPTION) => "1100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000101010101010101010101010101010101010101010101010101010101010101",
    OpRef::ClaimPowReward(&CLAIM_POW_REWARD) => "40000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
);
