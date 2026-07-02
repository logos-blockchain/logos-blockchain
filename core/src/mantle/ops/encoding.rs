use nom::{IResult, Parser as _, combinator::map};

use crate::{
    mantle::{
        Note, Op, OpProof,
        codec::{
            crypto::{
                decode_ed25519_signature, decode_field_element, decode_groth16,
                decode_zk_public_key, decode_zk_signature, encode_ed25519_signature,
                encode_field_element, encode_zk_signature,
            },
            primitives::{encode_byte, encode_uint64},
        },
        ledger::{BoundedInputs, BoundedOutputs, Inputs, Outputs},
        nom::NomDecode as _,
        ops::{
            leader_claim::{LeaderClaimOp, RewardsRoot, VoucherNullifier},
            transfer::TransferOp,
        },
    },
    proofs::{
        channel_multi_sig_proof::encoding::{
            decode_channel_multi_sig_proof, encode_channel_multi_sig_proof,
        },
        leader_claim_proof::{Groth16LeaderClaimProof, encoding::encode_poc},
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

// Proofs

pub fn decode_ops_proofs<'a>(input: &'a [u8], ops: &[Op]) -> IResult<&'a [u8], Vec<OpProof>> {
    let mut remaining = input;
    let mut proofs = Vec::with_capacity(ops.len());

    for op in ops {
        let (new_remaining, proof) = decode_op_proof(remaining, op)?;
        proofs.push(proof);
        remaining = new_remaining;
    }

    Ok((remaining, proofs))
}

pub fn decode_op_proof<'a>(input: &'a [u8], op: &Op) -> IResult<&'a [u8], OpProof> {
    match op {
        // Ed25519SigProof = Ed25519Signature
        Op::ChannelInscribe(_) => map(decode_ed25519_signature, OpProof::Ed25519Sig).parse(input),

        // ZkAndEd25519SigsProof = ZkSignature Ed25519Signature
        Op::SDPDeclare(_) => {
            let (input, zk_sig) = decode_zk_signature(input)?;
            let (input, ed25519_sig) = decode_ed25519_signature(input)?;
            Ok((
                input,
                OpProof::ZkAndEd25519Sigs {
                    zk_sig,
                    ed25519_sig,
                },
            ))
        }

        // ZkSigProof = ZkSignature
        Op::SDPWithdraw(_) | Op::SDPActive(_) | Op::Transfer(_) | Op::ChannelDeposit(_) => {
            map(decode_zk_signature, OpProof::ZkSig).parse(input)
        }

        // ProofOfClaimProof = Groth16
        Op::LeaderClaim(_) => map(decode_groth16, |proof| {
            OpProof::PoC(Groth16LeaderClaimProof::new(proof))
        })
        .parse(input),

        // ChannelMultiSigProof — also used by ChannelConfig (threshold sigs)
        Op::ChannelWithdraw(_) | Op::ChannelConfig(_) => map(
            decode_channel_multi_sig_proof,
            OpProof::ChannelMultiSigProof,
        )
        .parse(input),
    }
}

fn encode_op_proof(proof: &OpProof, op: &Op) -> Vec<u8> {
    if proof_matches(proof, op) {
        match proof {
            OpProof::Ed25519Sig(sig) => encode_ed25519_signature(sig),
            OpProof::ChannelMultiSigProof(proof) => encode_channel_multi_sig_proof(proof),
            OpProof::ZkAndEd25519Sigs {
                zk_sig,
                ed25519_sig,
            } => {
                let mut bytes = encode_zk_signature(zk_sig);
                bytes.extend(encode_ed25519_signature(ed25519_sig));
                bytes
            }
            OpProof::ZkSig(sig) => encode_zk_signature(sig),
            OpProof::PoC(poc) => encode_poc(poc),
        }
    } else {
        panic!("Mismatch between proof type and operation type");
    }
}

pub fn encode_ops_proofs(proofs: &[OpProof], ops: &[Op]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (proof, op) in proofs.iter().zip(ops.iter()) {
        bytes.extend(encode_op_proof(proof, op));
    }
    bytes
}

// Check if proofs correspond to ops
#[must_use]
pub const fn proof_matches(proof: &OpProof, op: &Op) -> bool {
    matches!(
        (proof, op),
        (OpProof::Ed25519Sig(_), Op::ChannelInscribe(_))
            | (
                OpProof::ChannelMultiSigProof(_),
                Op::ChannelWithdraw(_) | Op::ChannelConfig(_)
            )
            | (OpProof::ZkAndEd25519Sigs { .. }, Op::SDPDeclare(_))
            | (
                OpProof::ZkSig(_),
                Op::SDPWithdraw(_) | Op::SDPActive(_) | Op::Transfer(_) | Op::ChannelDeposit(_),
            )
            | (OpProof::PoC(_), Op::LeaderClaim(_))
    )
}

#[cfg(test)]
mod tests {
    use lb_groth16::CompressedGroth16Proof;
    use lb_key_management_system_keys::keys::ZkPublicKey;
    use num_bigint::BigUint;

    use crate::{
        mantle::{
            Op, OpProof,
            codec::crypto::GROTH16_BYTES,
            ops::{
                encoding::{decode_op_proof, encode_op_proof},
                leader_claim::{LeaderClaimOp, RewardsRoot, VoucherNullifier},
            },
        },
        proofs::leader_claim_proof::Groth16LeaderClaimProof,
    };

    #[test]
    fn test_encode_decode_leader_claim_op_proof() {
        let proof_bytes: [u8; 128] = core::array::from_fn(|i| i as u8);
        let poc_proof =
            Groth16LeaderClaimProof::new(CompressedGroth16Proof::from_bytes(&proof_bytes));

        let leader_claim_op = LeaderClaimOp {
            rewards_root: RewardsRoot::default(),
            voucher_nullifier: VoucherNullifier::default(),
            pk: ZkPublicKey::from(BigUint::from(0u64)),
        };
        let op = Op::LeaderClaim(leader_claim_op);

        let encoded = encode_op_proof(&OpProof::PoC(poc_proof), &op);
        assert_eq!(encoded.len(), GROTH16_BYTES);

        let (remaining, decoded) = decode_op_proof(&encoded, &op).unwrap();
        assert!(remaining.is_empty());
        assert_eq!(
            decoded,
            OpProof::PoC(Groth16LeaderClaimProof::new(
                CompressedGroth16Proof::from_bytes(&proof_bytes),
            ))
        );
    }
}
