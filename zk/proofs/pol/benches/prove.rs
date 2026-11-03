use logos_blockchain_pol::{PolWitnessInputs, prove};

fn main() {
    divan::main();
}

fn make_inputs() -> PolWitnessInputs {
    logos_blockchain_pol::test_utils::sample_witness_data().into()
}

#[divan::bench]
fn bench_prove(bencher: divan::Bencher) {
    let inputs = make_inputs();
    bencher
        .with_inputs(|| inputs.clone())
        .bench_values(|inputs| divan::black_box(prove(inputs).unwrap()));
}
