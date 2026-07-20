use ark_ff::AdditiveGroup as _;
use lb_core_macros::nom_wire_fixtures;
use lb_groth16::Fr;

use crate::mantle::{
    MantleTx, NoteId, Op, ledger::Outputs, ops::transfer::TransferOp, transactions::Ops,
};

nom_wire_fixtures!(MantleTx,
    MantleTx(Ops::empty()) => "00",
    MantleTx([Op::Transfer(TransferOp { inputs: [NoteId(Fr::ZERO)].into(), outputs: Outputs::empty() })].into()) => "010001000000000000000000000000000000000000000000000000000000000000000000"
);
