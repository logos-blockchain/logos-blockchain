use lb_core_macros::nom_wire_fixtures;
use lb_groth16::{AdditiveGroup as _, Field as _, Fr};

use crate::mantle::{Note, NoteId, ops::transfer::TransferOp};

nom_wire_fixtures!(
    TransferOp,
    TransferOp { inputs: [].into(), outputs: [].into() } => "0000",
    TransferOp { inputs: [NoteId::from(Fr::ONE)].into(), outputs: [].into() } => "01010000000000000000000000000000000000000000000000000000000000000000",
    TransferOp { inputs: [].into(), outputs: [Note { value: 0, pk: Fr::ZERO.into() }].into() } => "000100000000000000000000000000000000000000000000000000000000000000000000000000000000",
    TransferOp { inputs: [NoteId::from(Fr::ZERO)].into(), outputs: [Note { value: 0, pk: Fr::ONE.into() }].into() } => "0100000000000000000000000000000000000000000000000000000000000000000100000000000000000100000000000000000000000000000000000000000000000000000000000000",
);
