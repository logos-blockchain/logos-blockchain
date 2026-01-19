use std::{io::Cursor, ops::Mul as _};

use ark_bls12_381::{Fr, G1Affine, G1Projective};
use ark_ec::CurveGroup as _;
use ark_ff::{Field as _, PrimeField as _};
use ark_poly::EvaluationDomain as _;
use ark_poly_commit::kzg10::Commitment as KzgCommitment;
use ark_serialize::CanonicalSerialize as _;
use blake2::{
    Blake2bVar,
    digest::{Update as _, VariableOutput as _},
};
#[cfg(feature = "parallel")]
use rayon::iter::{IntoParallelIterator as _, ParallelIterator as _};

use super::{
    Commitment, Evaluations, PolynomialEvaluationDomain, Proof, ProvingKey, VerificationKey, kzg,
};
use crate::fk20::{Toeplitz1Cache, fk20_batch_generate_elements_proofs};

const ROW_HASH_SIZE: usize = 31;

/// Generate a hash of the row commitments using the `Blake2bVar` hashing
/// algorithm.
///
/// This function hashes a list of commitments into a constant-size (31 bytes)
/// hash vector. The hashing process involves serializing each commitment in an
/// uncompressed format and feeding the serialized data into the `Blake2bVar`
/// hasher.
///
/// # Arguments
///
/// * `commitments` - A slice of commitments to be hashed.
///
/// # Returns
///
/// A `Vec<u8>` representing the 31-byte hash of the input commitments.
///
/// # Panics
///
/// This function will panic if:
/// - The hasher fails to be constructed.
/// - Any commitment fails to serialize properly.
/// - The hash finalization process fails.
#[must_use]
pub fn generate_row_commitments_hash(commitments: &[Commitment]) -> Vec<u8> {
    let mut hasher = Blake2bVar::new(ROW_HASH_SIZE).expect("Hasher should be able to build");
    // add dst for hashing
    hasher.update(b"DA_V1");
    for c in commitments {
        let mut buffer = Cursor::new(Vec::new());
        c.serialize_uncompressed(&mut buffer)
            .expect("serialization");
        hasher.update(&buffer.into_inner());
    }
    let mut buffer = [0; ROW_HASH_SIZE];
    hasher
        .finalize_variable(&mut buffer)
        .expect("Hashing should succeed");
    buffer.to_vec()
}

/// Computes an aggregated polynomial from a set of row polynomials (in
/// evaluation form) and a hash of the aggregated commitments.
///
/// This function takes multiple polynomials (in evaluation form) and combines
/// them into a single polynomial by applying a linear combination based on a
/// scalar `h`, derived from the provided hash of aggregated commitments. This
/// process allows combining multiple row polynomials into a single polynomial
/// that commits to the entire expanded data.
///
/// # Arguments
///
/// * `polynomials` - A slice of `Evaluations` representing the row polynomials
///   in evaluation form.
/// * `aggregated_commitments_hash` - A byte slice representing the hash of
///   aggregated commitments (used as the scalar `h`).
/// * `domain` - The evaluation domain of the row polynomials (column count of
///   the encoded data matrix).
///
/// # Returns
///
/// An `Evaluations`, polynomial in evaluation aggregated from the linear
/// combination of the row polynomials.
///
/// # Panics
///
/// This function will panic if:
/// - The evaluation domain size does not match the size of the polynomials.
/// - Arithmetic over the field operations fails.
///
/// # Parallelism
///
/// If the `parallel` feature is enabled, this function will perform parallel
/// computations to improve performance during the evaluation process.
#[must_use]
pub fn compute_combined_polynomial(
    polynomials: &[Evaluations],
    aggregated_commitments_hash: &[u8],
    domain: PolynomialEvaluationDomain,
) -> Evaluations {
    let h = Fr::from_le_bytes_mod_order(aggregated_commitments_hash);
    let h_roots = compute_h_roots(h, polynomials.len());
    let evals: Vec<Fr> = {
        {
            #[cfg(not(feature = "parallel"))]
            {
                0..domain.size()
            }
            #[cfg(feature = "parallel")]
            {
                (0..domain.size()).into_par_iter()
            }
        }
        .map(|column| {
            polynomials
                .iter()
                .enumerate()
                .map(|(i, poly)| poly.evals[column].mul(h_roots[i]))
                .sum()
        })
        .collect()
    };
    Evaluations::from_vec_and_domain(evals, domain)
}

/// Generates an aggregated proof for a set of row polynomials and their
/// commitments.
///
/// This function computes an aggregated polynomial by combining row polynomials
/// (in evaluation form) using a hash derived from the commitments. Then, it
/// interpolates the aggregated polynomial and generates proofs for its entries
/// using the FK20 algorithm.
///
/// # Arguments
///
/// * `polynomials` - A slice of `Evaluations` representing the row polynomials
///   in evaluation form.
/// * `commitments` - A slice of `Commitment` corresponding to the row
///   polynomials.
/// * `domain` - The evaluation domain of the polynomials, defining their
///   dimensionality.
/// * `global_parameters` - Reference to the global KZG parameters used for
///   proof generation.
/// * `toeplitz1cache` - Optional cache for optimizing the Toeplitz
///   multiplication step.
///
/// # Returns
///
/// A `Vec<Proof>` containing the aggregated proofs, one for each row.
///
/// # Panics
///
/// This function will panic if:
/// - The row polynomial serialization fails during commitment hashing.
/// - The proof generation routines encounter invalid inputs or fail due to
///   arithmetic operations.
///
/// # Parallelism
///
/// If the `parallel` feature is enabled, parts of the computation may utilize
/// parallelism to enhance performance.
#[must_use]
pub fn generate_combined_proof(
    polynomials: &[Evaluations],
    commitments: &[Commitment],
    domain: PolynomialEvaluationDomain,
    global_parameters: &ProvingKey,
    toeplitz1cache: Option<&Toeplitz1Cache>,
) -> Vec<Proof> {
    let rows_commitments_hash = generate_row_commitments_hash(commitments);
    let aggregated_poly_evals =
        compute_combined_polynomial(polynomials, &rows_commitments_hash, domain);
    let aggregated_polynomial = aggregated_poly_evals.interpolate();
    fk20_batch_generate_elements_proofs(&aggregated_polynomial, global_parameters, toeplitz1cache)
}

/// Verifies a single column against its aggregated proof and row commitments.
///
/// This function aggregates the column elements using a hash derived from the
/// row commitments, computes an aggregated commitment, and verifies the
/// correctness of the provided proof for the column.
///
/// # Arguments
///
/// * `column_idx` - The index of the column being verified.
/// * `column` - A slice containing the elements of the column being verified.
/// * `row_commitments` - A slice containing the commitments for all rows.
/// * `column_proof` - A reference to the proof corresponding to the column.
/// * `domain` - The evaluation domain of the data matrix (defining the
///   dimensions).
/// * `verification_key` - KZG parameters used for proof verification.
///
/// # Returns
///
/// A boolean indicating whether the proof is valid (`true`) or not (`false`)
/// for the given column.
///
/// # Panics
///
/// This function will panic if:
/// - Row commitment hashing fails.
/// - Arithmetic operations (e.g., field multiplications, aggregations) fail.
#[must_use]
pub fn verify_column(
    column_idx: usize,
    column: &[Fr],
    row_commitments: &[Commitment],
    column_proof: &Proof,
    domain: PolynomialEvaluationDomain,
    verification_key: &VerificationKey,
) -> bool {
    let row_commitments_hash = generate_row_commitments_hash(row_commitments);
    let h = Fr::from_le_bytes_mod_order(&row_commitments_hash);
    let h_roots = compute_h_roots(h, column.len());
    let aggregated_elements: Fr = column
        .iter()
        .enumerate()
        .map(|(i, x)| x.mul(&h_roots[i]))
        .sum();
    let bases_agg_commit: Vec<G1Affine> = row_commitments.iter().map(|c| c.0).collect();
    let aggregated_commitments: G1Projective =
        ark_ec::VariableBaseMSM::msm(&bases_agg_commit, &h_roots).unwrap();
    let commitment = KzgCommitment(aggregated_commitments.into_affine());
    kzg::verify_element_proof(
        column_idx,
        &aggregated_elements,
        &commitment,
        column_proof,
        domain,
        verification_key,
    )
}

fn compute_h_roots(h: Fr, size: usize) -> Vec<Fr> {
    std::iter::successors(Some(Fr::ONE), |x| Some(h * x))
        .take(size)
        .collect()
}
