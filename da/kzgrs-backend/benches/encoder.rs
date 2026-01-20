use std::hint::black_box;

use divan::{Bencher, counter::BytesCount};
use logos_blockchain_kzgrs_backend::{
    encoder::{DaEncoder, DaEncoderParams},
    kzg_keys::PROVING_KEY,
};
use rand::RngCore as _;

fn main() {
    divan::main();
}

const MB: usize = 1024;

#[must_use]
pub fn rand_data(elements_count: usize) -> Vec<u8> {
    let mut buff = vec![0; elements_count * DaEncoderParams::MAX_BLS12_381_ENCODING_CHUNK_SIZE];
    rand::thread_rng().fill_bytes(&mut buff);
    buff
}

#[divan::bench(consts = [32, 64, 128, 256, 512, 1024], args = [128, 256, 512, 1024, 2048, 4096], sample_count = 1, sample_size = 30
)]
fn encode<const SIZE: usize>(bencher: Bencher, column_size: usize) {
    bencher
        .with_inputs(|| {
            let params = DaEncoderParams::new(column_size, true, PROVING_KEY.clone());
            (
                DaEncoder::new(params),
                rand_data(SIZE * MB / DaEncoderParams::MAX_BLS12_381_ENCODING_CHUNK_SIZE),
            )
        })
        .input_counter(|(_, buff)| BytesCount::new(buff.len()))
        .bench_refs(|(encoder, buff)| black_box(lb_core::da::DaEncoder::encode(encoder, buff)));
}
