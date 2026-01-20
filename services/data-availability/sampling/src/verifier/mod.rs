use lb_core::da::DaVerifier;

pub mod kzgrs;

pub trait VerifierBackend: DaVerifier {
    type Settings;
    fn new(settings: Self::Settings) -> Self;
}
