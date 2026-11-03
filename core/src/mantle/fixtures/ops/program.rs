use lb_binary_codec::canonical::codec_fixtures;

use super::op_values::{PROGRAM_REGISTER, PROGRAM_REGISTER_PAYLOAD_HEX};
use crate::mantle::ops::program::ProgramRegisterOp;

// `fixtures/programs.rs` pins the codec of `ProgramVerificationKey`, the same
// bytes as this payload.
codec_fixtures!(
    ProgramRegisterOp,
    PROGRAM_REGISTER.clone() => PROGRAM_REGISTER_PAYLOAD_HEX,
);
