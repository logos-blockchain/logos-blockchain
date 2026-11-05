/// The size the test eras pad payload bodies to: the largest block proposal of
/// an era that allows four uncles.
pub const PAYLOAD_BODY_SIZE: u16 = 18_187;

pub mod crypto;
pub mod dispatcher;
pub mod epoch;
pub mod era;
pub mod membership;
pub mod mocks;

mod libp2p;
pub use self::libp2p::*;
