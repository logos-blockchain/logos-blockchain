pub const INITIALIZATION: &[u8] = b"BlendInitialization";
pub const HEADER: &[u8] = b"BlendHeader";
pub const PAYLOAD: &[u8] = b"BlendPayload";
/// Domain for the non-reconstructable filler of the blending headers that are
/// never used, i.e. when a message is encapsulated fewer than `ß_max` times.
pub const RANDOM: &[u8] = b"BlendRandom";
