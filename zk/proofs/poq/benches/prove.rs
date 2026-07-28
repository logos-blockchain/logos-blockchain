//! Proof of Quota generation benchmarks.
//!
//! Run with `cargo bench -p logos-blockchain-poq --bench prove`.

use logos_blockchain_poq::prove;

mod common;

/// Key index the generation benchmarks prove for. Any index within the fixture
/// quota behaves the same, the circuit is fixed size.
const KEY_INDEX: u64 = 6;

fn main() {
    divan::main();
}

#[divan::bench]
fn bench_prove_core_node(bencher: divan::Bencher) {
    let inputs = common::core_node_inputs(KEY_INDEX);
    bencher
        .with_inputs(|| inputs.clone())
        .bench_values(|inputs| divan::black_box(prove(inputs).unwrap()));
}

#[divan::bench]
fn bench_prove_leader(bencher: divan::Bencher) {
    let inputs = common::leader_inputs(KEY_INDEX);
    bencher
        .with_inputs(|| inputs.clone())
        .bench_values(|inputs| divan::black_box(prove(inputs).unwrap()));
}
