use lb_binary_codec::canonical::codec_fixtures;

use super::proof_values::{CHANNEL_INSCRIBE_PROOF, CHANNEL_INSCRIBE_PROOF_HEX};
use crate::mantle::ops::ChannelInscribeProof;

codec_fixtures!(
    ChannelInscribeProof,
    context = vec![crate::mantle::ops::StepProofShape::User],
    CHANNEL_INSCRIBE_PROOF.clone() => CHANNEL_INSCRIBE_PROOF_HEX,
);
