use std::{borrow::Cow, ops::Mul as _};

use ark_bls12_381::{Bls12_381, Fr, G1Affine};
use ark_ec::{bls12::G1Prepared, pairing::Pairing as _};
use ark_poly::{
    DenseUVPolynomial as _, EvaluationDomain as _, GeneralEvaluationDomain,
    univariate::DensePolynomial,
};
use ark_poly_commit::kzg10::{Commitment, KZG10, Powers, Proof, UniversalParams, VerifierKey};
use num_traits::{One as _, Zero as _};

use crate::common::KzgRsError;

/// Commit to a polynomial where each of the evaluations are over `w(i)` for the
/// degree of the polynomial being omega (`w`) the root of unity (2^x).
pub fn commit_polynomial(
    polynomial: &DensePolynomial<Fr>,
    global_parameters: &UniversalParams<Bls12_381>,
) -> Result<Commitment<Bls12_381>, KzgRsError> {
    let roots_of_unity = Powers {
        powers_of_g: Cow::Borrowed(&global_parameters.powers_of_g),
        powers_of_gamma_g: Cow::Owned(vec![]),
    };
    KZG10::commit(&roots_of_unity, polynomial, None, None)
        .map_err(KzgRsError::PolyCommitError)
        .map(|(commitment, _)| commitment)
}

/// Compute a witness polynomial in that satisfies `witness(x) = (f(x)-v)/(x-u)`
pub fn generate_element_proof(
    element_index: usize,
    polynomial: &DensePolynomial<Fr>,
    proving_key: &UniversalParams<Bls12_381>,
    domain: GeneralEvaluationDomain<Fr>,
) -> Result<Proof<Bls12_381>, KzgRsError> {
    let u = domain.element(element_index);
    if u.is_zero() {
        return Err(KzgRsError::DivisionByZeroPolynomial);
    }
    let x_u = DensePolynomial::<Fr>::from_coefficients_vec(vec![-u, Fr::one()]);
    let witness_polynomial: DensePolynomial<_> = polynomial / &x_u;
    let proof = commit_polynomial(&witness_polynomial, proving_key)?;
    let proof = Proof {
        w: proof.0,
        random_v: None,
    };
    Ok(proof)
}

/// Verify proof for a single element
#[must_use]
pub fn verify_element_proof(
    element_index: usize,
    element: &Fr,
    commitment: &Commitment<Bls12_381>,
    proof: &Proof<Bls12_381>,
    domain: GeneralEvaluationDomain<Fr>,
    verification_key: &VerifierKey<Bls12_381>,
) -> bool {
    let u = domain.element(element_index);
    let v = element;
    let commitment_check_g1 = verification_key.g.mul(v) - commitment.0 - proof.w.mul(u);
    let qap = Bls12_381::multi_miller_loop(
        [
            <G1Affine as Into<G1Prepared<_>>>::into(proof.w),
            commitment_check_g1.into(),
        ],
        [
            verification_key.prepared_beta_h.clone(),
            verification_key.prepared_h.clone(),
        ],
    );
    let test = Bls12_381::final_exponentiation(qap).expect("Malformed Fr elements");
    test.is_zero()
}

#[cfg(test)]
mod test {
    use std::sync::LazyLock;

    use ark_bls12_381::{Bls12_381, Fr};
    use ark_poly::{
        DenseUVPolynomial as _, EvaluationDomain as _, GeneralEvaluationDomain,
        univariate::DensePolynomial,
    };
    use ark_poly_commit::kzg10::{KZG10, UniversalParams, VerifierKey};
    use rand::{Fill as _, SeedableRng as _, thread_rng};
    use rayon::{
        iter::{IndexedParallelIterator as _, ParallelIterator as _},
        prelude::IntoParallelRefIterator as _,
    };

    use crate::{
        common::bytes_to_polynomial,
        kzg::{commit_polynomial, generate_element_proof, verify_element_proof},
        proving_key::verification_key_proving_key,
    };

    const COEFFICIENTS_SIZE: usize = 16;
    static PROVING_KEY: LazyLock<UniversalParams<Bls12_381>> = LazyLock::new(|| {
        let mut rng = rand::rngs::StdRng::seed_from_u64(1998);
        KZG10::<Bls12_381, DensePolynomial<Fr>>::setup(COEFFICIENTS_SIZE - 1, true, &mut rng)
            .unwrap()
    });
    static VERIFICATION_KEY: LazyLock<VerifierKey<Bls12_381>> = LazyLock::new(|| {
        let mut rng = rand::rngs::StdRng::seed_from_u64(1998);
        let proving_key =
            KZG10::<Bls12_381, DensePolynomial<Fr>>::setup(COEFFICIENTS_SIZE - 1, true, &mut rng)
                .unwrap();
        verification_key_proving_key(&proving_key)
    });

    static DOMAIN: LazyLock<GeneralEvaluationDomain<Fr>> =
        LazyLock::new(|| GeneralEvaluationDomain::new(COEFFICIENTS_SIZE).unwrap());
    #[test]
    fn test_poly_commit() {
        let poly = DensePolynomial::from_coefficients_vec((0..10).map(Fr::from).collect());
        assert!(commit_polynomial(&poly, &PROVING_KEY).is_ok());
    }

    #[test]
    fn generate_proof_and_validate() {
        let mut bytes: [u8; 310] = [0; 310];
        let mut rng = thread_rng();
        bytes.try_fill(&mut rng).unwrap();
        let (eval, poly) = bytes_to_polynomial::<31>(&bytes, *DOMAIN).unwrap();
        let commitment = commit_polynomial(&poly, &PROVING_KEY).unwrap();
        let proofs: Vec<_> = (0..10)
            .map(|i| generate_element_proof(i, &poly, &PROVING_KEY, *DOMAIN).unwrap())
            .collect();

        eval.evals
            .par_iter()
            .zip(proofs.par_iter())
            .enumerate()
            .for_each(|(i, (element, proof))| {
                for ii in i..10 {
                    if ii == i {
                        // verifying works
                        assert!(verify_element_proof(
                            ii,
                            element,
                            &commitment,
                            proof,
                            *DOMAIN,
                            &VERIFICATION_KEY
                        ));
                    } else {
                        // Verification should fail for other points
                        assert!(!verify_element_proof(
                            ii,
                            element,
                            &commitment,
                            proof,
                            *DOMAIN,
                            &VERIFICATION_KEY
                        ));
                    }
                }
            });
    }
}
