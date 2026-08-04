use lb_codec::codec_fixtures;
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::Ed25519PublicKey;

use crate::{mantle::ops::leader_claim::VoucherCm, proofs::leader_proof::Groth16LeaderProof};

// Layout: `proof (128B) || entropy_contribution (32B LE) || leader_key (32B)
// || voucher_cm (32B LE)` — 224 bytes.
//
// Every field gets a distinct byte pattern so that a field transposition in
// another implementation cannot be masked by shared bytes.
codec_fixtures!(
    Groth16LeaderProof,
    Self::from_parts(
        lb_pol::PoLProof::from_bytes(&[0x22u8; _]),
        Fr::from(0x5555u64),
        Ed25519PublicKey::from_bytes(&[0x33u8; _]).unwrap(),
        VoucherCm::from(Fr::from(0x4444u64)),
    ) => "2222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222555500000000000000000000000000000000000000000000000000000000000033333333333333333333333333333333333333333333333333333333333333334444000000000000000000000000000000000000000000000000000000000000"
);
