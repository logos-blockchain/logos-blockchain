use std::fmt;

use ark_bls12_381::fr::Fr;
use ark_ff::Zero as _;
use ark_poly::{
    EvaluationDomain as _, domain::general::GeneralEvaluationDomain,
    evaluations::univariate::Evaluations, univariate::DensePolynomial,
};
use blst::BLST_ERROR;
use num_bigint::BigUint;
use thiserror::Error;

use crate::{BYTES_PER_FIELD_ELEMENT, FieldElement};

#[derive(Error, Debug)]
pub struct BlstError(pub BLST_ERROR);

impl fmt::Display for BlstError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.0 {
            BLST_ERROR::BLST_SUCCESS => write!(f, "Operation successful"),
            BLST_ERROR::BLST_BAD_ENCODING => write!(f, "Bad encoding"),
            BLST_ERROR::BLST_POINT_NOT_ON_CURVE => write!(f, "Point not on curve"),
            BLST_ERROR::BLST_POINT_NOT_IN_GROUP => write!(f, "Point not in group"),
            BLST_ERROR::BLST_AGGR_TYPE_MISMATCH => write!(f, "Aggregate type mismatch"),
            BLST_ERROR::BLST_VERIFY_FAIL => write!(f, "Verification failed"),
            BLST_ERROR::BLST_PK_IS_INFINITY => write!(f, "Public key is infinity"),
            BLST_ERROR::BLST_BAD_SCALAR => write!(f, "Bad scalar value"),
        }
    }
}

impl From<BLST_ERROR> for KzgRsError {
    fn from(err: BLST_ERROR) -> Self {
        Self::BlstError(BlstError(err))
    }
}

#[derive(Error, Debug)]
pub enum KzgRsError {
    #[error(
        "Data isn't properly padded, data len must match modulus {expected_modulus} but it is {current_size}"
    )]
    UnpaddedDataError {
        expected_modulus: usize,
        current_size: usize,
    },
    #[error("ChunkSize should be <= 32 (bytes), got {0}")]
    ChunkSizeTooBig(usize),
    #[error("Not enough attestations, required {required} but received {received}")]
    NotEnoughAttestations { required: usize, received: usize },
    #[error(
        "Mismatch between number of attestations ({attestations_count}) and number of signers ({signers_count})"
    )]
    AttestationSignersMismatch {
        attestations_count: usize,
        signers_count: usize,
    },
    #[error(transparent)]
    PolyCommitError(#[from] ark_poly_commit::Error),
    #[error("BLST error: {0}")]
    BlstError(BlstError),
    #[error("Denominator polynomial cannot be zero")]
    DivisionByZeroPolynomial,
}

/// Transform chunks of bytes (of size `CHUNK_SIZE`) into `Fr` which are
/// considered evaluations of a polynomial.
pub fn bytes_to_evaluations<const CHUNK_SIZE: usize>(
    data: &[u8],
    domain: GeneralEvaluationDomain<Fr>,
) -> Evaluations<Fr> {
    assert!((data.len() % CHUNK_SIZE).is_zero());
    Evaluations::from_vec_and_domain(
        data.chunks(CHUNK_SIZE)
            .map(
                // use little endian for convenience as shortening 1 byte (<32 supported)
                // do not matter in this endianness
                field_element_from_bytes_le,
            )
            .collect(),
        domain,
    )
}

/// Transform chunks of bytes (of size `CHUNK_SIZE`) into `Fr` which are
/// considered evaluations of a polynomial.
///
/// Then use FFT to transform that polynomial into coefficient form.
/// `CHUNK_SIZE` needs to be 31 (bytes) or less, otherwise it cannot be encoded.
/// The input data need to be padded, so it fits in a len modulus of
/// `CHUNK_SIZE`. Returns the polynomial in evaluation form and in coefficient
/// form
pub fn bytes_to_polynomial<const CHUNK_SIZE: usize>(
    data: &[u8],
    domain: GeneralEvaluationDomain<Fr>,
) -> Result<(Evaluations<Fr>, DensePolynomial<Fr>), KzgRsError> {
    if CHUNK_SIZE > BYTES_PER_FIELD_ELEMENT {
        return Err(KzgRsError::ChunkSizeTooBig(CHUNK_SIZE));
    }
    if !data.len().is_multiple_of(CHUNK_SIZE) {
        return Err(KzgRsError::UnpaddedDataError {
            expected_modulus: CHUNK_SIZE,
            current_size: data.len(),
        });
    }
    Ok(bytes_to_polynomial_unchecked::<CHUNK_SIZE>(data, domain))
}

/// Transform chunks of bytes (of size `CHUNK_SIZE`) into `Fr` which are
/// considered evaluations of a polynomial.
///
/// Then use FFT to transform that polynomial into coefficient form. No extra
/// checks are done for the caller. Caller need to ensure that `CHUNK_SIZE` is
/// not bigger than the underlying `Fr` element can be decoded from.
#[must_use]
pub fn bytes_to_polynomial_unchecked<const CHUNK_SIZE: usize>(
    data: &[u8],
    domain: GeneralEvaluationDomain<Fr>,
) -> (Evaluations<Fr>, DensePolynomial<Fr>) {
    let evals = bytes_to_evaluations::<CHUNK_SIZE>(data, domain);
    let coefficients = evals.interpolate_by_ref();
    (evals, coefficients)
}

/// Transform arbitrary bytes into a field element.
///
/// This transformation is bounds unchecked, it's up to the caller to know if
/// data fits within the bls modulus.
/// Data len cannot be higher than `BYTES_PER_FIELD_ELEMENT`
#[must_use]
pub fn field_element_from_bytes_le(b: &[u8]) -> FieldElement {
    assert!(b.len() <= BYTES_PER_FIELD_ELEMENT);
    FieldElement::from(BigUint::from_bytes_le(b))
}

#[must_use]
pub fn compute_roots_of_unity(size: usize) -> Vec<Fr> {
    let domain = GeneralEvaluationDomain::new(size).unwrap();
    domain.elements().take(size).collect()
}

#[cfg(test)]
mod test {
    use std::sync::LazyLock;

    use ark_bls12_381::fr::Fr;
    use ark_ff::{BigInteger as _, PrimeField as _};
    use ark_poly::{EvaluationDomain as _, GeneralEvaluationDomain, Polynomial as _};
    use blst::BLST_ERROR;
    use rand::{Fill as _, thread_rng};

    use super::{
        BlstError, KzgRsError, bytes_to_evaluations, bytes_to_polynomial, compute_roots_of_unity,
    };

    const CHUNK_SIZE: usize = 31;
    static DOMAIN: LazyLock<GeneralEvaluationDomain<Fr>> =
        LazyLock::new(|| GeneralEvaluationDomain::new(128).unwrap());
    #[test]
    fn encode_random_polynomial() {
        const N: usize = 100;
        let mut bytes: [u8; CHUNK_SIZE * N] = [0; CHUNK_SIZE * N];
        let mut rng = thread_rng();
        bytes.try_fill(&mut rng).unwrap();
        let evals = bytes_to_evaluations::<31>(&bytes, *DOMAIN);
        let (_, poly) = bytes_to_polynomial::<31>(&bytes, *DOMAIN).unwrap();
        for i in 0..N {
            let eval_point = DOMAIN.element(i);
            let point = poly.evaluate(&eval_point);
            // check point is the same
            assert_eq!(evals[i], point);
            // check point bytes are the same
            assert_eq!(
                &bytes[CHUNK_SIZE * i..CHUNK_SIZE * i + CHUNK_SIZE],
                &point.into_bigint().to_bytes_le()[..CHUNK_SIZE]
            );
        }
    }

    #[test]
    fn encode_chunk_size_too_big() {
        assert!(matches!(
            bytes_to_polynomial::<33>(&[], *DOMAIN),
            Err(KzgRsError::ChunkSizeTooBig(33))
        ));
    }

    #[test]
    fn encode_not_padded_data() {
        assert!(matches!(
            bytes_to_polynomial::<31>(&[0; 12], *DOMAIN),
            Err(KzgRsError::UnpaddedDataError {
                expected_modulus: 31,
                current_size: 12
            })
        ));
    }

    #[test]
    fn blst_error_conversions() {
        assert_eq!(
            format!("{}", BlstError(BLST_ERROR::BLST_SUCCESS)),
            "Operation successful"
        );
        assert_eq!(
            format!("{}", BlstError(BLST_ERROR::BLST_BAD_ENCODING)),
            "Bad encoding"
        );
        assert_eq!(
            format!("{}", BlstError(BLST_ERROR::BLST_POINT_NOT_ON_CURVE)),
            "Point not on curve"
        );
        assert_eq!(
            format!("{}", BlstError(BLST_ERROR::BLST_POINT_NOT_IN_GROUP)),
            "Point not in group"
        );
        assert_eq!(
            format!("{}", BlstError(BLST_ERROR::BLST_AGGR_TYPE_MISMATCH)),
            "Aggregate type mismatch"
        );
        assert_eq!(
            format!("{}", BlstError(BLST_ERROR::BLST_VERIFY_FAIL)),
            "Verification failed"
        );
        assert_eq!(
            format!("{}", BlstError(BLST_ERROR::BLST_PK_IS_INFINITY)),
            "Public key is infinity"
        );
        assert_eq!(
            format!("{}", BlstError(BLST_ERROR::BLST_BAD_SCALAR)),
            "Bad scalar value"
        );

        assert_eq!(
            format!("{}", KzgRsError::from(BLST_ERROR::BLST_SUCCESS)),
            "BLST error: Operation successful"
        );
    }

    #[test]
    fn test_compute_roots_of_unity() {
        let roots = compute_roots_of_unity(10);

        let domain: GeneralEvaluationDomain<Fr> = GeneralEvaluationDomain::new(10).unwrap();
        let frs: Vec<Fr> = domain.elements().take(10).collect();

        assert_eq!(roots, frs);
    }
}
