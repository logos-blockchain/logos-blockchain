use std::error::Error;

use ark_bls12_381::{Bls12_381, fr::Fr};
use ark_poly::polynomial::univariate::DensePolynomial;
use ark_poly_commit::kzg10::{KZG10, UniversalParams};
use ark_serialize::CanonicalDeserialize as _;
use rand::Rng;

use super::{ProvingKey, VerificationKey};

pub fn proving_key_from_randomness<R: Rng>(rng: &mut R) -> ProvingKey {
    KZG10::<Bls12_381, DensePolynomial<Fr>>::setup(8192, true, rng).unwrap()
}

pub fn proving_key_from_file(file_path: &str) -> Result<ProvingKey, Box<dyn Error>> {
    let serialized_data = std::fs::read(file_path)?;

    let params =
        UniversalParams::<Bls12_381>::deserialize_uncompressed_unchecked(&*serialized_data)?;
    Ok(params)
}

#[must_use]
pub fn verification_key_proving_key(proving_key: &ProvingKey) -> VerificationKey {
    VerificationKey {
        g: proving_key.powers_of_g[0],
        gamma_g: proving_key.powers_of_gamma_g[&0],
        h: proving_key.h,
        beta_h: proving_key.beta_h,
        prepared_h: proving_key.prepared_h.clone(),
        prepared_beta_h: proving_key.prepared_beta_h.clone(),
    }
}
