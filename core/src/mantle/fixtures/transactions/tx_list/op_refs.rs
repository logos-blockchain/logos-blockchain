use std::sync::LazyLock;

use ark_ff::AdditiveGroup as _;
use lb_codec::codec_fixtures;
use lb_groth16::Fr;

use crate::mantle::{
    NoteId, OpRef, ledger::Outputs, ops::transfer::TransferOp, transactions::OpRefs,
};

static TRANSFER: LazyLock<TransferOp> = LazyLock::new(|| TransferOp {
    inputs: [NoteId(Fr::ZERO)].into(),
    outputs: Outputs::empty(),
});

// The one-op bytes must stay identical to [`Ops`]' one-op fixture:
// `as_signing` hashes the borrowed column while the owned one is what
// round-trips, so a tx hash depends on the two agreeing. Nothing enforces
// it — edit one side only and both tests still pass.
codec_fixtures!(
    OpRefs<'_>,
    encode_only,
    Self::empty() => "00",
    Self::from([OpRef::Transfer(&TRANSFER)]) => "010001000000000000000000000000000000000000000000000000000000000000000000"
);
