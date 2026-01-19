use std::ops::Div as _;

use ark_ff::{BigInteger as _, PrimeField as _};
use ark_poly::EvaluationDomain as _;
use lb_kzgrs::{
    BYTES_PER_FIELD_ELEMENT, Commitment, Evaluations, KzgRsError, Polynomial,
    PolynomialEvaluationDomain, Proof, ProvingKey, bdfg_proving, commit_polynomial,
    common::bytes_to_polynomial_unchecked, encode, fk20::Toeplitz1Cache,
};
#[cfg(feature = "parallel")]
use rayon::iter::{IntoParallelRefIterator as _, ParallelIterator as _};

use crate::{
    common::{Chunk, ChunksMatrix, Row, share::DaShare},
    kzg_keys::PROVING_KEY,
};

#[derive(Clone)]
pub struct DaEncoderParams {
    column_count: usize,
    toeplitz1cache: Option<Toeplitz1Cache>,
    proving_key: ProvingKey,
}

impl DaEncoderParams {
    pub const MAX_BLS12_381_ENCODING_CHUNK_SIZE: usize = 31;

    #[must_use]
    pub fn new(column_count: usize, with_cache: bool, proving_key: ProvingKey) -> Self {
        let toeplitz1cache =
            with_cache.then(|| Toeplitz1Cache::with_size(&proving_key, column_count));
        Self {
            column_count,
            toeplitz1cache,
            proving_key,
        }
    }

    pub fn default_with(column_count: usize) -> Self {
        Self {
            column_count,
            toeplitz1cache: None,
            proving_key: PROVING_KEY.clone(),
        }
    }
}

#[derive(Clone)]
pub struct EncodedData {
    pub data: Vec<u8>,
    pub chunked_data: ChunksMatrix,
    pub extended_data: ChunksMatrix,
    pub row_commitments: Vec<Commitment>,
    pub combined_column_proofs: Vec<Proof>,
}

impl EncodedData {
    /// Returns a `DaShare` for the given index.
    /// If the index is out of bounds, returns `None`.
    #[must_use]
    pub fn to_da_share(&self, index: usize) -> Option<DaShare> {
        let column = self.extended_data.columns().nth(index)?;
        Some(DaShare {
            column,
            share_idx: index.try_into().unwrap(),
            combined_column_proof: self.combined_column_proofs[index],
            rows_commitments: self.row_commitments.clone(),
        })
    }

    #[must_use]
    pub const fn iter(&self) -> EncodedDataIterator<'_> {
        EncodedDataIterator::new(self)
    }
}

impl<'a> IntoIterator for &'a EncodedData {
    type Item = DaShare;
    type IntoIter = EncodedDataIterator<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub struct OwnedEncodedDataIterator {
    encoded_data: EncodedData,
    next_index: usize,
}

impl Iterator for OwnedEncodedDataIterator {
    type Item = DaShare;

    fn next(&mut self) -> Option<Self::Item> {
        let next_da_share = self.encoded_data.to_da_share(self.next_index)?;
        self.next_index += 1;
        Some(next_da_share)
    }
}

pub struct EncodedDataIterator<'a> {
    encoded_data: &'a EncodedData,
    next_index: usize,
}

impl<'a> EncodedDataIterator<'a> {
    #[must_use]
    pub const fn new(encoded_data: &'a EncodedData) -> Self {
        Self {
            encoded_data,
            next_index: 0,
        }
    }
}

impl Iterator for EncodedDataIterator<'_> {
    type Item = DaShare;

    fn next(&mut self) -> Option<Self::Item> {
        let next_da_share = self.encoded_data.to_da_share(self.next_index)?;
        self.next_index += 1;
        Some(next_da_share)
    }
}

pub struct DaEncoder {
    params: DaEncoderParams,
}

impl DaEncoder {
    #[must_use]
    pub const fn new(settings: DaEncoderParams) -> Self {
        Self { params: settings }
    }

    fn chunkify(&self, data: &[u8]) -> ChunksMatrix {
        let chunk_size =
            // column count is divided by two, as later on rows are encoded to twice the size
            self.params.column_count.div(2) * DaEncoderParams::MAX_BLS12_381_ENCODING_CHUNK_SIZE;
        data.chunks(chunk_size)
            .map(|d| {
                d.chunks(DaEncoderParams::MAX_BLS12_381_ENCODING_CHUNK_SIZE)
                    .map(|chunk| {
                        let mut buff = [0u8; BYTES_PER_FIELD_ELEMENT];
                        buff[..DaEncoderParams::MAX_BLS12_381_ENCODING_CHUNK_SIZE]
                            .copy_from_slice(chunk);
                        Chunk::from(buff.as_slice())
                    })
                    .collect()
            })
            .collect()
    }

    #[expect(clippy::type_complexity, reason = "TODO: Address this at some point.")]
    fn compute_kzg_row_commitments(
        proving_key: &ProvingKey,
        matrix: &ChunksMatrix,
        polynomial_evaluation_domain: PolynomialEvaluationDomain,
    ) -> Result<Vec<((Evaluations, Polynomial), Commitment)>, KzgRsError> {
        {
            #[cfg(not(feature = "parallel"))]
            {
                matrix.rows()
            }
            #[cfg(feature = "parallel")]
            {
                matrix.par_rows()
            }
        }
        .map(|r| {
            // Using the unchecked version here. Because during the process of chunkifiying
            // we already make sure to have the chunks of proper elements.
            // Also, after rs encoding, we are sure all `Fr` elements already fits within
            // modulus.
            let (evals, poly) = bytes_to_polynomial_unchecked::<BYTES_PER_FIELD_ELEMENT>(
                r.as_bytes().as_ref(),
                polynomial_evaluation_domain,
            );
            commit_polynomial(&poly, proving_key).map(|commitment| ((evals, poly), commitment))
        })
        .collect()
    }

    fn rs_encode_row(
        row: &Polynomial,
        polynomial_evaluation_domain: PolynomialEvaluationDomain,
    ) -> Evaluations {
        encode(row, polynomial_evaluation_domain)
    }

    fn rs_encode_rows(
        rows: &[Polynomial],
        polynomial_evaluation_domain: PolynomialEvaluationDomain,
    ) -> Vec<Evaluations> {
        {
            #[cfg(not(feature = "parallel"))]
            {
                rows.iter()
            }
            #[cfg(feature = "parallel")]
            {
                rows.par_iter()
            }
        }
        .map(|poly| Self::rs_encode_row(poly, polynomial_evaluation_domain))
        .collect()
    }

    fn evals_to_chunk_matrix(evals: &[Evaluations]) -> ChunksMatrix {
        ChunksMatrix(
            evals
                .iter()
                .map(|eval| {
                    Row(eval
                        .evals
                        .iter()
                        .map(|point| Chunk(point.into_bigint().to_bytes_le()))
                        .collect())
                })
                .collect(),
        )
    }
}

impl lb_core::da::DaEncoder for DaEncoder {
    type EncodedData = EncodedData;
    type Error = KzgRsError;

    fn encode(&self, data: &[u8]) -> Result<EncodedData, KzgRsError> {
        let proving_key = &self.params.proving_key;
        let chunked_data = self.chunkify(data);
        let row_domain = PolynomialEvaluationDomain::new(self.params.column_count)
            .expect("Domain should be able to build");
        let (row_polynomials, row_commitments): (Vec<_>, Vec<_>) =
            Self::compute_kzg_row_commitments(proving_key, &chunked_data, row_domain)?
                .into_iter()
                .unzip();
        let (_, row_polynomials): (Vec<_>, Vec<_>) = row_polynomials.into_iter().unzip();
        let encoded_evaluations = Self::rs_encode_rows(&row_polynomials, row_domain);
        let extended_data = Self::evals_to_chunk_matrix(&encoded_evaluations);
        let combined_column_proofs = bdfg_proving::generate_combined_proof(
            &encoded_evaluations,
            &row_commitments,
            row_domain,
            &self.params.proving_key,
            self.params.toeplitz1cache.as_ref(),
        );

        Ok(EncodedData {
            data: data.to_vec(),
            chunked_data,
            extended_data,
            row_commitments,
            combined_column_proofs,
        })
    }
}

#[cfg(test)]
pub mod test {
    use std::{ops::Div as _, sync::LazyLock};

    use ark_ff::PrimeField as _;
    use ark_poly::{EvaluationDomain as _, GeneralEvaluationDomain};
    use itertools::izip;
    use lb_core::da::DaEncoder as _;
    use lb_kzgrs::{
        BYTES_PER_FIELD_ELEMENT, FieldElement, PolynomialEvaluationDomain,
        common::bytes_to_polynomial_unchecked, decode_unchecked,
    };
    use rand::RngCore as _;

    use crate::{
        common::Chunk,
        encoder::{DaEncoder, DaEncoderParams},
        kzg_keys::{PROVING_KEY, VERIFICATION_KEY},
    };

    pub static DOMAIN_SIZE: usize = 16;
    pub static PARAMS: LazyLock<DaEncoderParams> =
        LazyLock::new(|| DaEncoderParams::default_with(DOMAIN_SIZE));
    pub static ENCODER: LazyLock<DaEncoder> = LazyLock::new(|| DaEncoder::new(PARAMS.clone()));

    #[must_use]
    pub fn rand_data(elements_count: usize) -> Vec<u8> {
        let mut buff = vec![0; elements_count * DaEncoderParams::MAX_BLS12_381_ENCODING_CHUNK_SIZE];
        rand::thread_rng().fill_bytes(&mut buff);
        buff
    }

    #[test]
    fn test_chunkify() {
        let params = DaEncoderParams::default_with(2);
        let elements = 10usize;
        let data = rand_data(elements);
        let encoder = DaEncoder::new(params.clone());
        let matrix = encoder.chunkify(&data);
        assert_eq!(matrix.len(), elements.div(params.column_count.div(2)));
        for row in matrix.rows() {
            assert_eq!(row.len(), params.column_count.div(2));
            assert_eq!(row.0[0].len(), BYTES_PER_FIELD_ELEMENT);
        }
    }

    #[test]
    fn test_compute_row_kzg_commitments() {
        let data = rand_data(32);
        let domain = GeneralEvaluationDomain::new(DOMAIN_SIZE).unwrap();
        let matrix = ENCODER.chunkify(data.as_ref());
        let commitments_data =
            DaEncoder::compute_kzg_row_commitments(&PROVING_KEY, &matrix, domain).unwrap();
        assert_eq!(commitments_data.len(), matrix.len());
    }

    #[test]
    fn test_evals_to_chunk_matrix() {
        let data = rand_data(32);
        let matrix = ENCODER.chunkify(data.as_ref());
        let domain = PolynomialEvaluationDomain::new(DOMAIN_SIZE).unwrap();
        let (poly_data, _): (Vec<_>, Vec<_>) =
            DaEncoder::compute_kzg_row_commitments(&PROVING_KEY, &matrix, domain)
                .unwrap()
                .into_iter()
                .unzip();
        let (_, poly_data): (Vec<_>, Vec<_>) = poly_data.into_iter().unzip();
        let extended_rows = DaEncoder::rs_encode_rows(&poly_data, domain);
        let extended_matrix = DaEncoder::evals_to_chunk_matrix(&extended_rows);
        for (r1, r2) in izip!(matrix.iter(), extended_matrix.iter()) {
            for (c1, c2) in izip!(r1.iter(), r2.iter()) {
                assert_eq!(c1, c2);
            }
        }
    }

    #[test]
    fn test_rs_encode_rows() {
        let data = rand_data(32);
        let domain = GeneralEvaluationDomain::new(DOMAIN_SIZE).unwrap();
        let matrix = ENCODER.chunkify(data.as_ref());
        let (poly_data, _): (Vec<_>, Vec<_>) =
            DaEncoder::compute_kzg_row_commitments(&PROVING_KEY, &matrix, domain)
                .unwrap()
                .into_iter()
                .unzip();
        let (evals, polynomials): (Vec<_>, Vec<_>) = poly_data.into_iter().unzip();
        let extended_rows = DaEncoder::rs_encode_rows(&polynomials, domain);
        // check encoding went well, original evaluation points vs extended ones
        for (e1, e2) in izip!(evals.iter(), extended_rows.iter()) {
            for (c1, c2) in izip!(&e1.evals, &e2.evals) {
                assert_eq!(c1, c2);
            }
        }
        let extended_matrix = DaEncoder::evals_to_chunk_matrix(&extended_rows);
        for (r1, r2, evals) in izip!(matrix.iter(), extended_matrix.iter(), extended_rows) {
            assert_eq!(r1.len(), r2.len().div(2));
            for (c1, c2) in izip!(r1.iter(), r2.iter()) {
                assert_eq!(c1, c2);
            }
            let points: Vec<_> = evals.evals.iter().copied().map(Some).collect();
            // using same domain here as size is the same
            let poly_2 = decode_unchecked(r1.len(), &points, domain, domain);
            let (poly_1, _) = bytes_to_polynomial_unchecked::<BYTES_PER_FIELD_ELEMENT>(
                r1.as_bytes().as_ref(),
                domain,
            );
            assert_eq!(poly_1, poly_2);
        }
    }

    #[test]
    fn test_full_encode_flow() {
        let data = rand_data(32);
        let domain = GeneralEvaluationDomain::new(DOMAIN_SIZE).unwrap();
        let encoding_data = ENCODER.encode(&data).unwrap();
        assert_eq!(encoding_data.data, data);
        assert_eq!(encoding_data.row_commitments.len(), 4);
        assert_eq!(encoding_data.combined_column_proofs.len(), 16);
        for (column, proof, idx) in izip!(
            encoding_data.extended_data.columns(),
            encoding_data.combined_column_proofs,
            0usize..
        ) {
            let column: Vec<FieldElement> = column
                .iter()
                .map(|Chunk(b)| FieldElement::from_le_bytes_mod_order(b))
                .collect();
            assert!(lb_kzgrs::bdfg_proving::verify_column(
                idx,
                &column,
                &encoding_data.row_commitments,
                &proof,
                domain,
                &VERIFICATION_KEY,
            ));
        }
    }

    #[test]
    fn test_encoded_data_iterator() {
        let encoder = &ENCODER;
        let data = vec![
            49u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8,
            0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8,
        ];
        let encoded_data = encoder.encode(&data).unwrap();

        let shares = encoded_data.iter();
        assert_eq!(shares.count(), 16);

        let shares = encoded_data.iter();
        assert_eq!(shares.count(), 16);
    }

    #[test]
    #[should_panic(expected = "polynomial_degree.is_power_of_two()")]
    fn test_encode_zeros() {
        // 837 zeros is not arbitrary, bug discovered on offsite 2025/04/22
        // Encoding only zeroes is not allowed in Logos blockchain DA network.

        let data = [0; 837];
        ENCODER.encode(&data).unwrap();
    }

    #[test]
    fn test_encode_mostly_zeros() {
        // 837 zeros is not arbitrary, bug discovered on offsite 2025/04/22
        let mut data = [0; 837];
        data[0] = 1;
        ENCODER.encode(&data).unwrap();

        let mut data = [0; 837];
        data[836] = 1;
        ENCODER.encode(&data).unwrap();
    }
}
