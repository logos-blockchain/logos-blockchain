use core::fmt;

use lb_core::da::{DaVerifier, blob::Share};
use lb_kzgrs_backend::{
    common::share::DaShare,
    kzg_keys::{proving_key_from_file, verification_key_proving_key},
    verifier::DaVerifier as LogosBlockchainKzgrsVerifier,
};
use serde::{Deserialize, Serialize};

use super::VerifierBackend;

#[derive(Debug)]
pub enum KzgrsDaVerifierError {
    VerificationError,
}

impl fmt::Display for KzgrsDaVerifierError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Self::VerificationError => write!(f, "Verification failed"),
        }
    }
}

impl std::error::Error for KzgrsDaVerifierError {}

#[derive(Clone)]
pub struct KzgrsDaVerifier {
    verifier: LogosBlockchainKzgrsVerifier,
    domain_size: usize,
}

impl VerifierBackend for KzgrsDaVerifier {
    type Settings = KzgrsDaVerifierSettings;

    fn new(settings: Self::Settings) -> Self {
        let verification_key = verification_key_proving_key(
            &proving_key_from_file(&settings.global_params_path)
                .expect("Global parameters has to be loaded from file"),
        );

        let verifier = LogosBlockchainKzgrsVerifier::new(verification_key);
        Self {
            verifier,
            domain_size: settings.domain_size,
        }
    }
}

impl DaVerifier for KzgrsDaVerifier {
    type DaShare = DaShare;
    type Error = KzgrsDaVerifierError;

    fn verify(
        &self,
        commitments: &<Self::DaShare as Share>::SharesCommitments,
        light_share: &<Self::DaShare as Share>::LightShare,
    ) -> Result<(), Self::Error> {
        // TODO: Prepare the domain depending the size, if fixed, so fixed domain, if
        // not it needs to come with some metadata.
        self.verifier
            .verify(light_share, commitments, self.domain_size)
            .then_some(())
            .ok_or(KzgrsDaVerifierError::VerificationError)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KzgrsDaVerifierSettings {
    pub global_params_path: String,
    pub domain_size: usize,
}
