use lb_binary_codec::canonical::codec_fixtures;
use lb_groth16::{AdditiveGroup as _, Field as _, Fr};

use crate::mantle::{
    fixtures::ops::op_values::{TRANSFER, TRANSFER_PAYLOAD_HEX},
    notes::{InputNullifiers, NoteCommitment, NoteNullifier, OutputCommitments},
    ops::transfer::TransferOp,
};

codec_fixtures!(
    TransferOp,
    Self { inputs: InputNullifiers::default(), outputs: OutputCommitments::default(), commitment_merkle_root: Fr::ZERO, excess_value: 0 } => "000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    Self { inputs: [NoteNullifier(Fr::ONE)].into(), outputs: OutputCommitments::default(), commitment_merkle_root: Fr::ZERO, excess_value: 0 } => "0101000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    Self { inputs: InputNullifiers::default(), outputs: [NoteCommitment(Fr::ONE)].into(), commitment_merkle_root: Fr::ZERO, excess_value: 0 } => "0001010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    Self { inputs: InputNullifiers::default(), outputs: OutputCommitments::default(), commitment_merkle_root: Fr::ONE, excess_value: 2 } => "000001000000000000000000000000000000000000000000000000000000000000000200000000000000",
    TRANSFER.clone() => TRANSFER_PAYLOAD_HEX,
);
