//! A scriptable [`ProofsVerifier`] for tests.

use core::cell::RefCell;
use std::rc::Rc;

use lb_blend_proofs::{
    quota::{ProofOfQuota, VerifiedProofOfQuota},
    selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use lb_key_management_system_keys::keys::Ed25519PublicKey;

use crate::{crypto::proofs::PoQVerificationInputsMinusSigningKey, encap::ProofsVerifier};

/// Decides whether a proof of quota is accepted, given the public inputs the
/// verifier was created with, the proof, and the signing key.
pub type ProofOfQuotaScript =
    Rc<dyn Fn(&PoQVerificationInputsMinusSigningKey, &ProofOfQuota, &Ed25519PublicKey) -> bool>;

/// Decides whether a proof of selection is accepted, given the public inputs
/// the verifier was created with, the proof, and the verification inputs.
pub type ProofOfSelectionScript =
    Rc<dyn Fn(&PoQVerificationInputsMinusSigningKey, &ProofOfSelection, &VerifyInputs) -> bool>;

/// How many times each verification method has been called on the current
/// thread since the script was last set or the counts last reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VerificationCounts {
    pub proof_of_quota: usize,
    pub proof_of_selection: usize,
}

/// The error a [`ScriptedProofsVerifier`] returns for a proof its script
/// rejects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("proof rejected by the test script")]
pub struct Rejected;

struct Script {
    proof_of_quota: ProofOfQuotaScript,
    proof_of_selection: ProofOfSelectionScript,
    counts: VerificationCounts,
}

impl Script {
    fn new(accept_proof_of_quota: bool, accept_proof_of_selection: bool) -> Self {
        Self {
            proof_of_quota: Rc::new(move |_, _, _| accept_proof_of_quota),
            proof_of_selection: Rc::new(move |_, _, _| accept_proof_of_selection),
            counts: VerificationCounts::default(),
        }
    }
}

thread_local! {
    static SCRIPT: RefCell<Script> = RefCell::new(Script::new(true, true));
}

/// A [`ProofsVerifier`] whose outcomes are scripted per thread and whose calls
/// are counted, replacing the one-off always-accept, always-reject and
/// reject-one-kind doubles.
///
/// The script lives in a thread-local: every `#[test]` runs on its own thread,
/// so a script set at the start of a test is private to that test, and a fresh
/// thread accepts every proof until told otherwise. A `#[tokio::test]` on a
/// current-thread runtime behaves the same; on a multi-thread runtime the
/// worker threads see the default script.
///
/// An instance holds the public inputs it was created with through
/// [`ProofsVerifier::new`] and hands them to the script, so a script can
/// depend on the epoch the verifier belongs to. Tests that need an instance
/// by value use [`Default`], which carries the default public inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptedProofsVerifier {
    public_inputs: PoQVerificationInputsMinusSigningKey,
}

impl Default for ScriptedProofsVerifier {
    fn default() -> Self {
        Self::new(PoQVerificationInputsMinusSigningKey::default())
    }
}

impl ScriptedProofsVerifier {
    /// Accepts every proof (the default), and resets the counts.
    pub fn accept_all() {
        Self::set_script(Script::new(true, true));
    }

    /// Rejects every proof, and resets the counts.
    pub fn reject_all() {
        Self::set_script(Script::new(false, false));
    }

    /// Rejects every proof of quota and accepts every proof of selection, and
    /// resets the counts.
    pub fn reject_proofs_of_quota() {
        Self::set_script(Script::new(false, true));
    }

    /// Accepts every proof of quota and rejects every proof of selection, and
    /// resets the counts.
    pub fn reject_proofs_of_selection() {
        Self::set_script(Script::new(true, false));
    }

    /// Decides proofs of quota with `script`, leaving the proof-of-selection
    /// script and the counts as they are.
    pub fn script_proof_of_quota(
        script: impl Fn(&PoQVerificationInputsMinusSigningKey, &ProofOfQuota, &Ed25519PublicKey) -> bool
        + 'static,
    ) {
        SCRIPT.with_borrow_mut(|current| current.proof_of_quota = Rc::new(script));
    }

    /// Decides proofs of selection with `script`, leaving the proof-of-quota
    /// script and the counts as they are.
    pub fn script_proof_of_selection(
        script: impl Fn(&PoQVerificationInputsMinusSigningKey, &ProofOfSelection, &VerifyInputs) -> bool
        + 'static,
    ) {
        SCRIPT.with_borrow_mut(|current| current.proof_of_selection = Rc::new(script));
    }

    /// The number of verification calls made on this thread since the script
    /// was last set or the counts last reset.
    #[must_use]
    pub fn verification_counts() -> VerificationCounts {
        SCRIPT.with_borrow(|current| current.counts)
    }

    /// Sets both counts back to zero without touching the script.
    pub fn reset_verification_counts() {
        SCRIPT.with_borrow_mut(|current| current.counts = VerificationCounts::default());
    }

    /// The public inputs this instance was created with.
    #[must_use]
    pub const fn public_inputs(&self) -> &PoQVerificationInputsMinusSigningKey {
        &self.public_inputs
    }

    fn set_script(script: Script) {
        SCRIPT.with_borrow_mut(|current| *current = script);
    }
}

impl ProofsVerifier for ScriptedProofsVerifier {
    type Error = Rejected;

    fn new(public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
        Self { public_inputs }
    }

    fn verify_proof_of_quota(
        &self,
        proof: ProofOfQuota,
        signing_key: &Ed25519PublicKey,
    ) -> Result<VerifiedProofOfQuota, Self::Error> {
        let accepted = SCRIPT.with_borrow_mut(|current| {
            current.counts.proof_of_quota += 1;
            (current.proof_of_quota)(&self.public_inputs, &proof, signing_key)
        });
        accepted
            .then(|| VerifiedProofOfQuota::from_proof_of_quota_unchecked(proof))
            .ok_or(Rejected)
    }

    fn verify_proof_of_selection(
        &self,
        proof: ProofOfSelection,
        inputs: &VerifyInputs,
    ) -> Result<VerifiedProofOfSelection, Self::Error> {
        let accepted = SCRIPT.with_borrow_mut(|current| {
            current.counts.proof_of_selection += 1;
            (current.proof_of_selection)(&self.public_inputs, &proof, inputs)
        });
        accepted
            .then(|| VerifiedProofOfSelection::from_proof_of_selection_unchecked(proof))
            .ok_or(Rejected)
    }
}
