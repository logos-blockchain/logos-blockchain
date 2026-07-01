// ==============================================================================
// Leader Operation Decoders
// ==============================================================================

use nom::IResult;

use crate::mantle::{
    Note,
    encoding::{
        decode_field_element, decode_zk_public_key, encode_byte, encode_field_element,
        encode_uint64,
    },
    ledger::{BoundedInputs, BoundedOutputs, Inputs, Outputs},
    nom::NomDecode as _,
    ops::{
        leader_claim::{LeaderClaimOp, RewardsRoot, VoucherNullifier},
        transfer::TransferOp,
    },
};

pub fn decode_leader_claim(input: &[u8]) -> IResult<&[u8], LeaderClaimOp> {
    // LeaderClaim = RewardsRoot VoucherNullifier
    let (input, rewards_root_fr) = decode_field_element(input)?;
    let (input, voucher_nullifier_fr) = decode_field_element(input)?;
    let (input, pk) = decode_zk_public_key(input)?;

    Ok((
        input,
        LeaderClaimOp {
            rewards_root: RewardsRoot::from(rewards_root_fr),
            voucher_nullifier: VoucherNullifier::from(voucher_nullifier_fr),
            pk,
        },
    ))
}

// ==============================================================================
// Transfer Decoders
// ==============================================================================

pub fn decode_inputs(input: &[u8]) -> IResult<&[u8], Inputs> {
    let (input, bounded_inputs) = BoundedInputs::decode(input)?;

    Ok((input, Inputs::new(bounded_inputs)))
}

pub fn decode_outputs(input: &[u8]) -> IResult<&[u8], Outputs> {
    let (input, bounded_outputs) = BoundedOutputs::decode(input)?;

    Ok((input, Outputs::new(bounded_outputs)))
}

pub fn decode_transfer(input: &[u8]) -> IResult<&[u8], TransferOp> {
    // Transfer = Inputs Outputs
    let (input, inputs) = decode_inputs(input)?;
    let (input, outputs) = decode_outputs(input)?;

    Ok((input, TransferOp::new(inputs, outputs)))
}

/// Encode leader operations
#[must_use]
pub fn encode_leader_claim(op: &LeaderClaimOp) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_field_element(&op.rewards_root.into()));
    bytes.extend(encode_field_element(&op.voucher_nullifier.into()));
    bytes.extend(encode_field_element(op.pk.as_fr()));
    bytes
}

/// Encode transfer operation
fn encode_note(note: &Note) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_uint64(note.value));
    bytes.extend(encode_field_element(note.pk.as_fr()));
    bytes
}

pub fn encode_inputs(inputs: &BoundedInputs) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_byte(inputs.len() as u8));
    for input in inputs {
        bytes.extend(encode_field_element(input.as_ref()));
    }
    bytes
}

pub fn encode_outputs(outputs: &BoundedOutputs) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_byte(outputs.len() as u8));
    for output in outputs {
        bytes.extend(encode_note(output));
    }
    bytes
}

#[must_use]
pub fn encode_transfer_op(op: &TransferOp) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_inputs(op.inputs.as_ref()));
    bytes.extend(encode_outputs(op.outputs.as_ref()));
    bytes
}
