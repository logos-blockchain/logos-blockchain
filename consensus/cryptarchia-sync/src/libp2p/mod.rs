pub mod behaviour;
mod downloader;
pub mod errors;
pub mod messages;
mod packing;
pub mod provider;
mod utils;

#[expect(
    clippy::redundant_pub_crate,
    reason = "The bound is intentionally crate-visible for wire message implementations."
)]
pub(crate) const MAX_MSG_LEN: usize = 16 * 1024 * 1024;
