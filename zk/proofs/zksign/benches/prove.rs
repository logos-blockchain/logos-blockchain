use lb_groth16::Fr;
use lb_poseidon2::{Digest as _, Poseidon2Bn254Hasher};
use logos_blockchain_zksign::{ZkSignPrivateKeysData, ZkSignWitnessInputs, prove};
use num_bigint::BigUint;

fn main() {
    divan::main();
}

#[divan::bench]
fn bench_prove(bencher: divan::Bencher) {
    let sk_arr: [Fr; 32] = core::array::from_fn(|i| BigUint::from(i as u64 + 1).into());
    let msg_hash = Poseidon2Bn254Hasher::digest(&[BigUint::from_bytes_le(b"foo_bar").into()]);

    bencher
        .with_inputs(|| {
            ZkSignWitnessInputs::from_witness_data_and_message_hash(
                ZkSignPrivateKeysData::from(sk_arr),
                msg_hash,
            )
        })
        .bench_values(|inputs| divan::black_box(prove(inputs).unwrap()));
}
