#[cfg(feature = "libp2p")]
pub mod libp2p;

pub trait TryFrom: Sized {
    type Error: std::error::Error;

    fn try_from_provider_id(provider_id: &[u8]) -> Result<Self, Self::Error>;
}
