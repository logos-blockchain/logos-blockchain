use ark_ff::Field as _;
use lb_groth16::Fr;
use lb_wire::wire_fixtures;

use crate::mantle::{ledger::Outputs, ops::channel::channel_transfer::ChannelTransferOp};

wire_fixtures!(ChannelTransferOp,
    Self { channel_id: [1u8; 32].into(), inputs: [Fr::ONE.into()].into(), outputs: Outputs::empty() } => "010101010101010101010101010101010101010101010101010101010101010101010000000000000000000000000000000000000000000000000000000000000000"
);
