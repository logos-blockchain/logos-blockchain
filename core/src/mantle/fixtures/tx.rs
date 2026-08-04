use ark_ff::AdditiveGroup as _;
use lb_codec::codec_fixtures;
use lb_groth16::Fr;

use crate::mantle::{
    NoteId, Op, RawMantleTx, ledger::Outputs, ops::transfer::TransferOp, transactions::Ops,
};

codec_fixtures!(RawMantleTx,
    Self(Ops::empty()) => "00",
    Self([Op::Transfer(TransferOp { inputs: [NoteId(Fr::ZERO)].into(), outputs: Outputs::empty() })].into()) => "010001000000000000000000000000000000000000000000000000000000000000000000"
);
