/// Fallback Gossipsub wire-message safety ceiling for topics without an
/// explicit per-topic envelope limit.
pub const MAX_WIRE_MESSAGE_SIZE: usize = 16 * 1024 * 1024;
