use lb_codec::codec_fixtures;

use super::proof_values::{
    CHANNEL_MULTI_SIG, CHANNEL_MULTI_SIG_HEX, ED25519_SIG, ED25519_SIG_HEX, POC, POC_HEX,
    ZK_AND_ED25519_SIGS, ZK_AND_ED25519_SIGS_HEX, ZK_SIG, ZK_SIG_HEX,
};
use crate::mantle::{OpProof, ops::NoOpProof};

codec_fixtures!(
    OpProof,
    encode_only,
    OpProof::None(NoOpProof) => "",
    OpProof::ZkSig(ZK_SIG.clone()) => ZK_SIG_HEX,
    OpProof::Ed25519Sig(*ED25519_SIG) => ED25519_SIG_HEX,
    OpProof::ZkAndEd25519Sigs(ZK_AND_ED25519_SIGS.clone()) => ZK_AND_ED25519_SIGS_HEX,
    OpProof::PoC(POC.clone()) => POC_HEX,
    OpProof::ChannelMultiSigProof(CHANNEL_MULTI_SIG.clone()) => CHANNEL_MULTI_SIG_HEX
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
