pub mod behaviour;
mod downloader;
pub mod errors;
pub mod messages;
mod packing;
pub mod provider;
mod utils;

pub const MAX_MSG_LEN: usize = 16 * 1024 * 1024;
