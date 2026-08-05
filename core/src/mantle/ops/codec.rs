use lb_codec::{BinaryDecodeExt as _, BinaryEncode as _, DecodeError};
use lb_key_management_system_keys::keys::{Ed25519Signature, ZkSignature};

use crate::{
    mantle::{
        Op, OpProof,
        ops::{NoOpProof, ZkAndEd25519Proof},
        transactions::OpProofs,
    },
    proofs::{
        channel_multi_sig_proof::ChannelMultiSigProof, leader_claim_proof::Groth16LeaderClaimProof,
    },
};

pub fn decode_ops_proofs<'a>(
    input: &'a [u8],
    ops: &[Op],
) -> Result<(&'a [u8], OpProofs), DecodeError> {
    let mut remaining = input;
    let mut proofs = Vec::with_capacity(ops.len());

    for op in ops {
        let (new_remaining, proof) = decode_op_proof(remaining, op)?;
        proofs.push(proof);
        remaining = new_remaining;
    }
    let ops_proofs = OpProofs::try_from(proofs).map_err(|_| {
        DecodeError::length_out_of_bounds::<OpProofs>(ops.len(), OpProofs::MIN, OpProofs::MAX)
    })?;

    Ok((remaining, ops_proofs))
}

fn decode_op_proof<'a>(input: &'a [u8], op: &Op) -> Result<(&'a [u8], OpProof), DecodeError> {
    match op {
        // Ed25519SigProof = Ed25519Signature
        Op::ChannelInscribe(_) => {
            Ed25519Signature::decode(input).map(|(rest, sig)| (rest, OpProof::Ed25519Sig(sig)))
        }

        // ZkAndEd25519SigsProof = ZkSignature Ed25519Signature
        Op::SDPDeclare(_) => {
            let (input, zk_sig) = ZkSignature::decode(input)?;
            let (input, ed25519_sig) = Ed25519Signature::decode(input)?;
            let proof = ZkAndEd25519Proof {
                zk_sig,
                ed25519_sig,
            };
            Ok((input, OpProof::ZkAndEd25519Sigs(proof)))
        }

        // ZkSigProof = ZkSignature
        Op::SDPWithdraw(_) | Op::SDPActive(_) | Op::Transfer(_) | Op::ChannelDeposit(_) => {
            ZkSignature::decode(input).map(|(rest, sig)| (rest, OpProof::ZkSig(sig)))
        }

        // ProofOfClaimProof = Groth16
        Op::LeaderClaim(_) => {
            Groth16LeaderClaimProof::decode(input).map(|(rest, poc)| (rest, OpProof::PoC(poc)))
        }

        // ChannelMultiSigProof — also used by ChannelConfig (threshold sigs)
        Op::ChannelWithdraw(_) | Op::ChannelTransfer(_) | Op::ChannelConfig(_) => {
            ChannelMultiSigProof::decode(input)
                .map(|(rest, proof)| (rest, OpProof::ChannelMultiSigProof(proof)))
        }
        Op::ClaimPowReward(_) => Ok((input, OpProof::None(NoOpProof))),
    }
}

fn encode_op_proof(proof: &OpProof, op: &Op) -> Vec<u8> {
    if proof_matches(proof, op) {
        match proof {
            OpProof::Ed25519Sig(sig) => sig.encode_to_vec(),
            OpProof::ChannelMultiSigProof(proof) => proof.encode_to_vec(),
            OpProof::ZkAndEd25519Sigs(proof) => {
                let mut bytes = proof.zk_sig.encode_to_vec();
                bytes.extend(proof.ed25519_sig.encode_to_vec());
                bytes
            }
            OpProof::ZkSig(sig) => sig.encode_to_vec(),
            OpProof::PoC(poc) => poc.encode_to_vec(),
            OpProof::None(pow) => pow.encode_to_vec(),
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
                Op::ChannelWithdraw(_) | Op::ChannelTransfer(_) | Op::ChannelConfig(_)
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
    use lb_groth16::{COMPRESSED_PROOF_SIZE, CompressedGroth16Proof};
    use lb_key_management_system_keys::keys::ZkPublicKey;
    use num_bigint::BigUint;

    use crate::{
        mantle::{
            Op, OpProof,
            ops::{
                codec::{decode_op_proof, encode_op_proof},
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
        assert_eq!(encoded.len(), COMPRESSED_PROOF_SIZE);

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
