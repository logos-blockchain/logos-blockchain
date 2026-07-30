use ark_ff::AdditiveGroup as _;
use lb_groth16::Fr;
use lb_wire::wire_fixtures;

use crate::mantle::{
    MantleTx, NoteId, Op, ledger::Outputs, ops::transfer::TransferOp, transactions::Ops,
};

wire_fixtures!(MantleTx,
    Self(Ops::empty()) => "00",
    Self([Op::Transfer(TransferOp { inputs: [NoteId(Fr::ZERO)].into(), outputs: Outputs::empty() })].into()) => "010001000000000000000000000000000000000000000000000000000000000000000000"
);
