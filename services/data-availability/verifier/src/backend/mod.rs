pub mod kzgrs;
pub mod trigger;
pub mod tx;

pub use lb_core::da::DaVerifier;

pub trait VerifierBackend: DaVerifier {
    type Settings;
    fn new(settings: Self::Settings) -> Self;
}

pub trait TxVerifierBackend {
    type Settings;
    type Tx;
    type BlobId;
    type Error;
    fn new(settings: Self::Settings) -> Self;

    fn verify(&self, tx: &Self::Tx) -> Result<(), Self::Error>;
    fn blob_id(&self, tx: &Self::Tx) -> Result<Self::BlobId, Self::Error>;
}
