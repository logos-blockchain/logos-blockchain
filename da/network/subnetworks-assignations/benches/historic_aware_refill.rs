use std::collections::BTreeSet;

use divan::{Bencher, black_box, counter::ItemsCount};
use lb_utils::blake_rng::BlakeRng;
use logos_blockchain_subnetworks_assignations::versions::history_aware_refill::assignations::HistoryAwareRefill;
use rand::{RngCore as _, SeedableRng as _, prelude::IteratorRandom as _, thread_rng};

fn main() {
    divan::main();
}

type BenchId = [u8; 32];
const REPLICATION_FACTOR: usize = 5;
#[divan::bench(args = [100, 1000, 10000, 100_000], sample_count = 10, sample_size = 10)]
fn compute_static_size_subnetwork_assignations(bencher: Bencher, network_size: usize) {
    bencher
        .with_inputs(|| {
            let mut rng = BlakeRng::from_seed([33; 64].into());
            let nodes: Vec<BenchId> = std::iter::repeat_with(|| {
                let mut buff = [0u8; 32];
                rng.fill_bytes(&mut buff);
                buff
            })
            .take(network_size)
            .collect();

            let previous_nodes: Vec<BTreeSet<BenchId>> =
                std::iter::repeat_with(BTreeSet::<BenchId>::new)
                    .take(network_size)
                    .collect();
            (rng, nodes, previous_nodes)
        })
        .input_counter(move |_| ItemsCount::new(network_size))
        .bench_values(|(mut rng, nodes, previous_nodes)| {
            black_box(move || {
                HistoryAwareRefill::calculate_subnetwork_assignations(
                    &nodes,
                    previous_nodes,
                    REPLICATION_FACTOR,
                    &mut rng,
                )
            })
        });
}

#[divan::bench(args = [(100, 1000), (1000, 10000), (10000, 100_000)], sample_count = 10, sample_size = 10)]
fn compute_growing_size_subnetwork_assignations(
    bencher: Bencher,
    (network_size, growing_size): (usize, usize),
) {
    bencher
        .with_inputs(|| {
            let mut blake_rng = BlakeRng::from_seed([33; 64].into());
            let nodes: Vec<BenchId> = std::iter::repeat_with(|| {
                let mut buff = [0u8; 32];
                blake_rng.fill_bytes(&mut buff);
                buff
            })
            .take(network_size)
            .collect();

            let previous_nodes: Vec<BTreeSet<BenchId>> =
                std::iter::repeat_with(BTreeSet::<BenchId>::new)
                    .take(network_size)
                    .collect();

            let assignations = HistoryAwareRefill::calculate_subnetwork_assignations(
                &nodes,
                previous_nodes,
                REPLICATION_FACTOR,
                &mut blake_rng,
            );

            let new_nodes: Vec<_> = nodes
                .iter()
                .copied()
                .chain(
                    std::iter::repeat_with(|| {
                        let mut rng = thread_rng();
                        let mut buff = [0u8; 32];
                        rng.fill_bytes(&mut buff);
                        buff
                    })
                    .take(growing_size - network_size),
                )
                .collect();

            (blake_rng, new_nodes, assignations)
        })
        .input_counter(move |_| ItemsCount::new(growing_size))
        .bench_values(|(mut rng, nodes, previous_nodes)| {
            black_box(move || {
                HistoryAwareRefill::calculate_subnetwork_assignations(
                    &nodes,
                    previous_nodes,
                    REPLICATION_FACTOR,
                    &mut rng,
                )
            })
        });
}

#[divan::bench(args = [(100, 1000), (1000, 10000), (10000, 100_000)], sample_count = 10, sample_size = 10)]
fn compute_shrinking_size_subnetwork_assignations(
    bencher: Bencher,
    (shrinking_size, network_size): (usize, usize),
) {
    bencher
        .with_inputs(|| {
            let mut rng = BlakeRng::from_seed([33; 64].into());
            let nodes: Vec<BenchId> = std::iter::repeat_with(|| {
                let mut buff = [0u8; 32];
                rng.fill_bytes(&mut buff);
                buff
            })
            .take(network_size)
            .collect();

            let previous_nodes: Vec<BTreeSet<BenchId>> =
                std::iter::repeat_with(BTreeSet::<BenchId>::new)
                    .take(network_size)
                    .collect();

            let assignations = HistoryAwareRefill::calculate_subnetwork_assignations(
                &nodes,
                previous_nodes,
                REPLICATION_FACTOR,
                &mut rng,
            );

            let new_nodes: Vec<_> = nodes
                .iter()
                .copied()
                .choose_multiple(&mut rng, shrinking_size);

            (rng, new_nodes, assignations)
        })
        .input_counter(move |_| ItemsCount::new(shrinking_size))
        .bench_values(|(mut rng, nodes, previous_nodes)| {
            black_box(move || {
                HistoryAwareRefill::calculate_subnetwork_assignations(
                    &nodes,
                    previous_nodes,
                    REPLICATION_FACTOR,
                    &mut rng,
                )
            })
        });
}
