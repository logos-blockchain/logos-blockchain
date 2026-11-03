use lb_binary_codec::canonical::codec_fixtures;
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::ZkPublicKey;

use crate::proofs::leader_proof::Groth16LeaderProof;

// Layout: `proof (128B) || entropy_contribution (32B LE) ||
// reward_key (32B LE)`, 192 bytes.
//
// Every field gets a distinct byte pattern so that a field transposition in
// another implementation cannot be masked by shared bytes.
codec_fixtures!(
    Groth16LeaderProof,
    Self::from_parts(
        lb_pol::PoLProof::from_bytes(&[0x22u8; _]),
        Fr::from(0x5555u64),
        ZkPublicKey::new(Fr::from(0x4444u64)),
    ) => "222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222255550000000000000000000000000000000000000000000000000000000000004444000000000000000000000000000000000000000000000000000000000000"
);
