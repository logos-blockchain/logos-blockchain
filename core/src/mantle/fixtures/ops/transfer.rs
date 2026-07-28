use lb_groth16::{AdditiveGroup as _, Field as _, Fr};
use lb_wire::wire_fixtures;

use crate::mantle::{Note, NoteId, ops::transfer::TransferOp};

wire_fixtures!(
    TransferOp,
    Self { inputs: [].into(), outputs: [].into() } => "0000",
    Self { inputs: [NoteId::from(Fr::ONE)].into(), outputs: [].into() } => "01010000000000000000000000000000000000000000000000000000000000000000",
    Self { inputs: [].into(), outputs: [Note { value: 0, pk: Fr::ZERO.into() }].into() } => "000100000000000000000000000000000000000000000000000000000000000000000000000000000000",
    Self { inputs: [NoteId::from(Fr::ZERO)].into(), outputs: [Note { value: 0, pk: Fr::ONE.into() }].into() } => "0100000000000000000000000000000000000000000000000000000000000000000100000000000000000100000000000000000000000000000000000000000000000000000000000000",
);
