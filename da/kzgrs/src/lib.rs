pub mod common;
pub mod fk20;
pub mod kzg;
pub mod proving_key;

pub mod bdfg_proving;
pub mod rs;

use ark_bls12_381::{Bls12_381, Fr};
use ark_poly::{GeneralEvaluationDomain, univariate::DensePolynomial};
use ark_poly_commit::{kzg10, kzg10::VerifierKey, sonic_pc::UniversalParams};
pub use common::{KzgRsError, bytes_to_evaluations, bytes_to_polynomial};
pub use kzg::{commit_polynomial, generate_element_proof, verify_element_proof};
pub use proving_key::{
    proving_key_from_file, proving_key_from_randomness, verification_key_proving_key,
};
pub use rs::{decode_unchecked, encode};

pub type Commitment = kzg10::Commitment<Bls12_381>;
pub type Proof = kzg10::Proof<Bls12_381>;
pub type FieldElement = Fr;
pub type Polynomial = DensePolynomial<Fr>;
pub type Evaluations = ark_poly::Evaluations<Fr>;
pub type PolynomialEvaluationDomain = GeneralEvaluationDomain<Fr>;

pub type ProvingKey = UniversalParams<Bls12_381>;

pub type VerificationKey = VerifierKey<Bls12_381>;

pub const BYTES_PER_FIELD_ELEMENT: usize = size_of::<Fr>();
