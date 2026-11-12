use std::error::Error;

use lb_groth16::{Groth16Proof, groth16_batch_verify};

use crate::{ZkSignProof, ZkSignVerifierInputs, verification_key};

#[derive(Debug)]
pub enum VerifyError {
    Expansion,
    ProofVerify(Box<dyn Error>),
}

/// Verify a proof against its public inputs.
///
/// # Errors
/// Returns an error if the compressed proof cannot be expanded or the
/// underlying Groth16 verification cannot be carried out.
pub fn verify(
    proof: &ZkSignProof,
    public_inputs: &ZkSignVerifierInputs,
) -> Result<bool, VerifyError> {
    let expanded_proof = Groth16Proof::try_from(proof).map_err(|_| VerifyError::Expansion)?;
    lb_groth16::groth16_verify(
        verification_key::ZKSIGN_VK.as_ref(),
        &expanded_proof,
        &public_inputs.as_inputs(),
    )
    .map_err(|e| VerifyError::ProofVerify(Box::new(e)))
}

pub fn batch_verify(
    proofs_and_inputs: &[(ZkSignProof, ZkSignVerifierInputs)],
) -> Result<bool, VerifyError> {
    let inputs: Vec<Vec<_>> = proofs_and_inputs
        .iter()
        .map(|(_, pi)| pi.as_inputs().to_vec())
        .collect();

    let expanded_proofs: Vec<Groth16Proof> = proofs_and_inputs
        .iter()
        .map(|(p, _)| Groth16Proof::try_from(p).map_err(|_| VerifyError::Expansion))
        .collect::<Result<Vec<_>, _>>()?; // short-circuits on first failure

    Ok(groth16_batch_verify(
        verification_key::ZKSIGN_VK.as_ref(),
        &expanded_proofs,
        &inputs,
    ))
}

#[cfg(test)]
mod tests {
    use lb_groth16::{Fr, fr_from_bytes};
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    struct Fixture {
        proof: Vec<u8>,
        inputs: Vec<Vec<u8>>,
    }

    fn fixture() -> (ZkSignProof, ZkSignVerifierInputs) {
        let fixture: Fixture =
            serde_json::from_str(include_str!("test_data/signature.json")).unwrap();
        let proof = ZkSignProof::from_bytes(&fixture.proof.try_into().unwrap());
        let inputs: [Fr; 33] = fixture
            .inputs
            .iter()
            .map(|bytes| fr_from_bytes(bytes).unwrap())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let [public_keys @ .., msg] = inputs;
        (
            proof,
            ZkSignVerifierInputs {
                public_keys: public_keys.map(Into::into),
                msg: msg.into(),
            },
        )
    }

    #[test]
    fn verifies_pinned_signature_fixture() {
        let (proof, inputs) = fixture();
        assert!(verify(&proof, &inputs).unwrap());
        assert!(batch_verify(&[(proof, inputs.clone()), (proof, inputs)]).unwrap());
    }

    #[test]
    fn rejects_fixture_with_different_message() {
        let (proof, mut inputs) = fixture();
        inputs.msg = Fr::from(43u64).into();
        assert!(!verify(&proof, &inputs).unwrap());
        assert!(!batch_verify(&[(proof, inputs)]).unwrap());
    }
}
