use lb_codec::codec_fixtures;
use lb_groth16::CompressedGroth16Proof;
use lb_key_management_system_keys::keys::{Ed25519Signature, ZkSignature};

use crate::mantle::{OpProof, ops::NoOpProof};

codec_fixtures!(
    OpProof,
    encode_only,
    OpProof::None(NoOpProof) => "",
    OpProof::ZkSig(ZkSignature::new(CompressedGroth16Proof::from_bytes(&[1u8; 128]))) => "0101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101",
    OpProof::Ed25519Sig(Ed25519Signature::from_bytes(&[1u8; 64])) => "01010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101"
);

#[cfg(test)]
mod tests {
    use lb_codec::BinaryEncode as _;

    use crate::mantle::{OpProof, transactions::Ops};

    #[test]
    fn proof_decodes_for_every_operation() {
        for op in &Ops::sample() {
            let proof = op.sample_proof();
            let encoded = proof.encode();

            let (rest, decoded) = OpProof::decode_for_op(&encoded, op)
                .unwrap_or_else(|error| panic!("{} failed to decode: {error:?}", op.as_str()));

            assert!(rest.is_empty(), "{} left trailing bytes", op.as_str());
            assert_eq!(decoded, proof, "{} did not round-trip", op.as_str());
        }
    }
}
