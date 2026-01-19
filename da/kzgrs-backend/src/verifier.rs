use ark_ff::PrimeField as _;
use ark_poly::EvaluationDomain as _;
use lb_kzgrs::{FieldElement, PolynomialEvaluationDomain, VerificationKey};

use crate::common::{
    Chunk,
    share::{DaLightShare, DaSharesCommitments},
};

#[derive(Clone)]
pub struct DaVerifier {
    pub verification_key: VerificationKey,
}

impl DaVerifier {
    #[must_use]
    pub const fn new(verification_key: VerificationKey) -> Self {
        Self { verification_key }
    }

    #[must_use]
    pub fn verify(
        &self,
        share: &DaLightShare,
        commitments: &DaSharesCommitments,
        rows_domain_size: usize,
    ) -> bool {
        let column: Vec<FieldElement> = share
            .column
            .iter()
            .map(|Chunk(b)| FieldElement::from_le_bytes_mod_order(b))
            .collect();
        let rows_domain = PolynomialEvaluationDomain::new(rows_domain_size)
            .expect("Domain should be able to build");
        lb_kzgrs::bdfg_proving::verify_column(
            share.share_idx as usize,
            &column,
            &commitments.rows_commitments,
            &share.combined_column_proof,
            rows_domain,
            &self.verification_key,
        )
    }
}

#[cfg(test)]
mod test {
    #[cfg(target_arch = "x86_64")]
    use std::hint::black_box;

    use lb_core::da::{DaEncoder as _, blob::Share as _};

    use crate::{
        encoder::{DaEncoder, DaEncoderParams, test::rand_data},
        kzg_keys::VERIFICATION_KEY,
        verifier::DaVerifier,
    };

    #[test]
    fn test_verify() {
        let encoder = DaEncoder::new(DaEncoderParams::default_with(2));
        let data = rand_data(32);
        let domain_size = 2usize;
        let verifier = DaVerifier::new(VERIFICATION_KEY.clone());
        let encoded_data = encoder.encode(&data).unwrap();
        for share in &encoded_data {
            let (light_share, commitments) = share.into_share_and_commitments();
            assert!(verifier.verify(&light_share, &commitments, domain_size));
        }
    }

    #[cfg(target_arch = "x86_64")]
    mod utils {
        use crate::encoder::DaEncoderParams;

        pub struct Configuration {
            pub label: String,
            pub elements_count: usize,
        }

        impl Configuration {
            pub const fn new(label: String, elements_count: usize) -> Self {
                Self {
                    label,
                    elements_count,
                }
            }

            pub fn from_elements_count(elements_count: usize) -> Self {
                let label = format!("{elements_count}KiB");
                let elements = elements_for_kib(elements_count);
                Self::new(label, elements)
            }
        }

        /// Returns the number of elements needed to generate `kib` KiB of data
        /// before encoding. Assumes each element has size
        /// [`DaEncoderParams::MAX_BLS12_381_ENCODING_CHUNK_SIZE`] (currently 31
        /// bytes), as used by [`rand_data`](crate::encoder::test::rand_data).
        ///
        /// If either constant or function changes, this calculation must be
        /// updated.
        pub fn elements_for_kib(kib: usize) -> usize {
            let bytes = kib * 1024;
            bytes / DaEncoderParams::MAX_BLS12_381_ENCODING_CHUNK_SIZE
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[expect(
        clippy::undocumented_unsafe_blocks,
        reason = "This test is just to measure cpu and should be run manually"
    )]
    fn bench_verify_cycles(iters: u64, configuration: &utils::Configuration) {
        let domain_size = 2048usize;
        let encoder = DaEncoder::new(DaEncoderParams::default_with(domain_size));
        let data = rand_data(configuration.elements_count);
        let verifier = DaVerifier::new(VERIFICATION_KEY.clone());
        let encoded_data = encoder.encode(&data).unwrap();

        let share = encoded_data.iter().next().unwrap();
        let (light_share, commitments) = share.into_share_and_commitments();
        let t0 = unsafe { core::arch::x86_64::_rdtsc() };
        for _ in 0..iters {
            black_box(verifier.verify(&light_share, &commitments, domain_size));
        }
        let t1 = unsafe { core::arch::x86_64::_rdtsc() };

        let cycles_diff = t1 - t0;
        let cycles_per_run = (t1 - t0) / iters;

        let header = format!(
            "=========== kzgrs-da-verify [{}] ===========",
            configuration.label
        );
        let footer = "=".repeat(header.len());
        println!("{header}",);
        println!("  - iterations        : {iters:>20}");
        println!(
            "  - elements          : {:>20}",
            configuration.elements_count
        );
        println!("  - cycles total      : {cycles_diff:>20}");
        println!("  - cycles per run    : {cycles_per_run:>20}");
        println!("{footer}\n");
    }

    // TODO: Remove this when we have the proper benches in the proofs
    #[cfg(target_arch = "x86_64")]
    #[ignore = "This test is just for calculation the cycles for the above set of proofs. This will be moved to the pertinent proof in the future."]
    #[test]
    fn test_verify_cycles() {
        let iters = 1000u64;

        let configurations = [
            utils::Configuration::from_elements_count(32),
            utils::Configuration::from_elements_count(256),
            utils::Configuration::from_elements_count(1024),
        ];

        for configuration in &configurations {
            bench_verify_cycles(iters, configuration);
        }
    }
}
