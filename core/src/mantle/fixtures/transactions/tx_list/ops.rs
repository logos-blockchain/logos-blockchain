use ark_ff::AdditiveGroup as _;
use lb_codec::codec_fixtures;
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::Ed25519PublicKey;

use crate::mantle::{
    NoteId, Op,
    ledger::Outputs,
    ops::{
        channel::{
            ChannelId, MsgId,
            inscribe::{Inscription, InscriptionOp},
        },
        transfer::TransferOp,
    },
    transactions::Ops,
};

fn transfer() -> Op {
    Op::Transfer(TransferOp {
        inputs: [NoteId(Fr::ZERO)].into(),
        outputs: Outputs::empty(),
    })
}

fn inscription() -> Op {
    Op::ChannelInscribe(InscriptionOp {
        channel_id: ChannelId::from([0u8; 32]),
        inscription: Inscription::default(),
        parent: MsgId::root(),
        signer: Ed25519PublicKey::from_bytes(&[1u8; 32]).unwrap(),
    })
}

// The one-op bytes must stay identical to [`OpRefs`]' one-op fixture: see
// the note there.
codec_fixtures!(Ops,
    Self::empty() => "00",
    Self::from([transfer()]) => "010001000000000000000000000000000000000000000000000000000000000000000000",
    Self::from([transfer(), inscription()]) => "0200010000000000000000000000000000000000000000000000000000000000000000001100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000101010101010101010101010101010101010101010101010101010101010101"
);
