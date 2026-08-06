use ark_ff::AdditiveGroup as _;
use lb_codec::codec_fixtures;
use lb_groth16::Fr;

use crate::mantle::{
    NoteId, Op, RawMantleTx,
    ledger::Outputs,
    ops::transfer::TransferOp,
    transactions::{
        Ops,
        hash::{TxHash, TxHashPrefix},
    },
};

codec_fixtures!(RawMantleTx,
    Self(Ops::empty()) => "00",
    Self([Op::Transfer(TransferOp { inputs: [NoteId(Fr::ZERO)].into(), outputs: Outputs::empty() })].into()) => "010001000000000000000000000000000000000000000000000000000000000000000000"
);

codec_fixtures!(
    TxHash,
    Self([0u8; 32]) => "0000000000000000000000000000000000000000000000000000000000000000",
    Self([1u8; 32]) => "0101010101010101010101010101010101010101010101010101010101010101"
);

codec_fixtures!(
    TxHashPrefix,
    Self([0u8; 8]) => "0000000000000000",
    Self([1u8; 8]) => "0101010101010101"
);
