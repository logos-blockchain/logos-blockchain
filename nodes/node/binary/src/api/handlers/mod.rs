#[macro_export]
macro_rules! make_request_and_return_response {
    ($cond:expr) => {{ $crate::api::errors::json_response($cond.await) }};
}

pub mod blend;
pub mod blocks;
pub mod channel;
pub mod cryptarchia;
pub mod leader;
pub mod mempool;
pub mod network;
pub mod node;
pub mod pow;
pub mod sdp;
pub mod time;
pub mod wallet;
