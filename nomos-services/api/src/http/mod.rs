pub type DynError = Box<dyn std::error::Error + Send + Sync + 'static>;
pub mod consensus;
pub mod da;
pub mod libp2p;
pub mod mantle;
pub mod mempool;
pub mod sdp;
pub mod storage;
pub mod wallet;
