use lb_binary_codec::canonical::codec_fixtures;
use lb_groth16::Fr;

use crate::{mantle::notes::NoteCommitment, proofs::leader_proof::Groth16LeaderProof};

// Layout: `proof (128B) || entropy_contribution (32B LE) ||
// reward_commitment (32B LE) || reward_value (8B LE)`, 200 bytes.
//
// Every field gets a distinct byte pattern so that a field transposition in
// another implementation cannot be masked by shared bytes.
codec_fixtures!(
    Groth16LeaderProof,
    Self::from_parts(
        lb_pol::PoLProof::from_bytes(&[0x22u8; _]),
        Fr::from(0x5555u64),
        NoteCommitment(Fr::from(0x4444u64)),
        0x6666,
    ) => "2222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222555500000000000000000000000000000000000000000000000000000000000044440000000000000000000000000000000000000000000000000000000000006666000000000000"
);
