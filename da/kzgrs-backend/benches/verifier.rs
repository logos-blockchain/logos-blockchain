use std::hint::black_box;

use divan::{Bencher, counter::BytesCount};
use lb_core::da::{DaEncoder as _, blob::Share as _};
use logos_blockchain_kzgrs_backend::{
    common::{Chunk, share::DaShare},
    encoder::{DaEncoder, DaEncoderParams},
    kzg_keys::{PROVING_KEY, VERIFICATION_KEY},
};
use rand::{RngCore as _, thread_rng};

fn main() {
    divan::main();
}

const KB: usize = 1024;

#[must_use]
pub fn rand_data(elements_count: usize) -> Vec<u8> {
    let mut buff = vec![0; elements_count * DaEncoderParams::MAX_BLS12_381_ENCODING_CHUNK_SIZE];
    thread_rng().fill_bytes(&mut buff);
    buff
}

#[divan::bench(consts = [32, 64, 128, 256, 512, 1024], args = [128, 256, 512, 1024, 2048, 4096], sample_count = 1, sample_size = 30
)]
fn verify<const SIZE: usize>(bencher: Bencher, column_size: usize) {
    bencher
        .with_inputs(|| {
            let params = DaEncoderParams::new(column_size, true, PROVING_KEY.clone());

            let encoder = DaEncoder::new(params);
            let data = rand_data(SIZE * KB / DaEncoderParams::MAX_BLS12_381_ENCODING_CHUNK_SIZE);
            let encoded_data = encoder.encode(&data).unwrap();
            let verifier = logos_blockchain_kzgrs_backend::verifier::DaVerifier {
                verification_key: VERIFICATION_KEY.clone(),
            };
            let da_share = DaShare {
                column: encoded_data.extended_data.columns().next().unwrap(),
                share_idx: 0,
                combined_column_proof: encoded_data.combined_column_proofs[0],
                rows_commitments: encoded_data.row_commitments,
            };
            let (light_share, commitments) = da_share.into_share_and_commitments();
            (verifier, light_share, commitments)
        })
        .input_counter(|(_, light_share, _)| {
            BytesCount::new(light_share.column.iter().map(Chunk::len).sum::<usize>())
        })
        .bench_values(|(verifier, light_share, commitments)| {
            black_box(verifier.verify(&light_share, &commitments, column_size))
        });
}
