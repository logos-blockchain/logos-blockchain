//! Property tests over the three things that shape what a sequencer sees:
//! the chain's shape (blocks, empty slots, forks of every depth and
//! outcome, late revivals, lagging and duplicate deliveries, frozen and
//! jumping LIB, pruning), the blocks' content (which posted transactions
//! land in what order, next to other writers' inscriptions, note changes
//! and configs) and the client's condition (dropped streams, node outages,
//! restarts, failing fetches, price moves, user deposits and withdraws).
//! Every combination must leave every [`Layer`] intact: the view contract
//! and the mirrored view, each planned payload finalized exactly once in
//! one lineage, the turn view, the wallet and the deposits, and what a
//! resumed sequencer reports.
//!
//! The vocabulary is drawn one layer at a time, so a rung of the ladder
//! that fails names what the SDK gets wrong: only other writers'
//! inscriptions on a moving chain; plus the sequencers' own messages and
//! the choices a zone makes about them (abandon an orphan, burst-publish in
//! turn, adopt another sequencer's orphans); plus config changes and
//! posting rules; plus deposits, withdraws and note transfers; plus
//! restarts, outages and price moves.

use std::{collections::HashSet, sync::Arc};

use common::{FINALITY_DEPTH, FOREIGN, FOREIGN_2, Posted, World, inscription, passive, seq_key};
use lb_zone_sdk::sequencer::{FinalizedOp, FinalizedTx};
use logos_blockchain_zone_sdk_testing::{
    chains::{self, Chain, Condition, Shape},
    consumer::{Claims, ReferenceConsumer, SharedChoices},
    layers::Layer,
    policy::payload_str,
    world as common,
    writers::{OP_CYCLE, WriterOp},
};
use proptest::{prelude::*, strategy::Union};

/// Gas prices every run starts at; large enough that a percent rise is a
/// different integer price.
const BASE_GAS_PRICE: u64 = 100;
/// Largest price rise a step applies: below the funding reserve
/// (`FundingConfig::DEFAULT_PRIORITY_FEE_PERCENT`), so a tx funded at the
/// base price is still paid for after the rise.
const MAX_GAS_RISE_PERCENT: u64 = 10;
/// Funding notes cover one atomic bundle at the highest price.
const FUNDING_NOTE_VALUE: u64 = 500_000;

/// What happens on the client's side of the step.
#[derive(Clone, Copy, Debug)]
enum Client {
    Quiet,
    /// Every block stream closes; the sequencers reconnect.
    Drop,
    /// The primary node goes down for a step: its mempool and funding
    /// reservations are lost, then it comes back.
    NodeDown,
    /// Restart sequencer `index`, from its checkpoint or, with nothing of
    /// its own pending, from scratch.
    Restart {
        index: usize,
        fresh: bool,
    },
    /// The next `count` block fetches fail.
    FetchFail {
        count: usize,
    },
    /// Gas prices move to `BASE_GAS_PRICE` plus `percent`, in force from the
    /// next block.
    GasPrice {
        percent: u64,
    },
    /// A user deposits one pre-minted note of `value` into the channel.
    Deposit {
        value: u64,
    },
    /// Sequencer `index` withdraws `amount` to the user; a sequencer without
    /// the funds or the node skips it. Spends finalized notes only, unless
    /// the scenario allows unfinalized ones.
    Withdraw {
        index: usize,
        amount: u64,
    },
    /// Sequencer `index` lets its next orphan die instead of republishing
    /// it.
    Abandon {
        index: usize,
    },
    /// Sequencer `index` publishes `count` fresh payloads at once on its
    /// next turn.
    Burst {
        index: usize,
        count: usize,
    },
    /// Sequencer `index` starts or stops republishing other sequencers'
    /// orphans.
    Adopt {
        index: usize,
        on: bool,
    },
}

#[derive(Clone, Copy, Debug)]
struct Step {
    shape: Shape,
    posted: Posted,
    client: Client,
}

fn shape() -> impl Strategy<Value = Shape> {
    let depth = 1..=FINALITY_DEPTH as usize;
    prop_oneof![
        10 => prop_oneof![6 => Just(1u64), 2 => Just(2u64), 1 => Just(3u64)]
            .prop_map(|gap| Shape::Extend { gap }),
        3 => depth.prop_flat_map(|depth| (Just(depth), 1..=depth + 1))
            .prop_map(|(depth, length)| Shape::Fork { depth, length }),
        2 => (1..=4usize).prop_map(|blocks| Shape::Revive { blocks }),
        1 => (1..=3usize).prop_map(|silent| Shape::Lagging { silent }),
        1 => Just(Shape::SplitTip),
        1 => Just(Shape::Duplicate),
        1 => Just(Shape::FreezeLib),
        1 => Just(Shape::ThawLib),
        1 => Just(Shape::Prune),
    ]
}

fn posted() -> impl Strategy<Value = Posted> {
    prop_oneof![
        6 => Just(Posted::All),
        1 => Just(Posted::None),
        2 => Just(Posted::Half),
        1 => Just(Posted::Reversed),
    ]
}

/// The client conditions the `top` layer admits: note moves from
/// [`Layer::Notes`], outages and price moves from [`Layer::Restarts`].
fn client(top: Layer) -> impl Strategy<Value = Client> {
    let lineage = u32::from(top >= Layer::Lineage);
    let notes = u32::from(top >= Layer::Notes);
    let restarts = u32::from(top >= Layer::Restarts);
    let arms: Vec<(u32, BoxedStrategy<Client>)> = vec![
        (10, Just(Client::Quiet).boxed()),
        (
            lineage,
            (0..3usize)
                .prop_map(|index| Client::Abandon { index })
                .boxed(),
        ),
        (
            lineage,
            (0..3usize, 1..=3usize)
                .prop_map(|(index, count)| Client::Burst { index, count })
                .boxed(),
        ),
        (
            lineage,
            (0..3usize, prop::bool::ANY)
                .prop_map(|(index, on)| Client::Adopt { index, on })
                .boxed(),
        ),
        (restarts, Just(Client::Drop).boxed()),
        (restarts, Just(Client::NodeDown).boxed()),
        (
            restarts,
            (0..3usize, prop::bool::weighted(0.25))
                .prop_map(|(index, fresh)| Client::Restart { index, fresh })
                .boxed(),
        ),
        (
            restarts,
            (1..=2usize)
                .prop_map(|count| Client::FetchFail { count })
                .boxed(),
        ),
        (
            restarts,
            (0..=MAX_GAS_RISE_PERCENT)
                .prop_map(|percent| Client::GasPrice { percent })
                .boxed(),
        ),
        (
            2 * notes,
            (1..=3u64)
                .prop_map(|value| Client::Deposit { value })
                .boxed(),
        ),
        (
            2 * notes,
            (0..3usize, 1..=3u64)
                .prop_map(|(index, amount)| Client::Withdraw { index, amount })
                .boxed(),
        ),
    ];
    // A union rejects a zero weight: the layers below `top` draw nothing.
    Union::new_weighted(arms.into_iter().filter(|(weight, _)| *weight > 0).collect())
}

fn step(top: Layer) -> impl Strategy<Value = Step> {
    (shape(), posted(), client(top)).prop_map(|(shape, posted, client)| Step {
        shape,
        posted,
        client,
    })
}

#[derive(Clone, Debug)]
struct Scenario {
    /// The highest layer the schedule draws from.
    top: Layer,
    chain: Chain,
    sequencers: usize,
    writers: usize,
    messages: usize,
    timeframe: u32,
    timeout: u32,
    /// What the SDK does not handle yet: withdraws may spend unfinalized
    /// notes, which wedge the sequencer when the note's deposit is reorged
    /// away for good, and the writers chain two inscriptions in one tx,
    /// which a config landing drops from the view unreported.
    known_issues: bool,
    schedule: Vec<Step>,
}

impl Scenario {
    /// The ops the writers cycle through: inscriptions always, configs from
    /// [`Layer::Configs`], note transfers and withdraws from
    /// [`Layer::Notes`]; two-inscription txs only with the known issues.
    const fn writer_ops(&self) -> &'static [WriterOp] {
        const INSCRIPTIONS: [WriterOp; 1] = [WriterOp::Inscribe];
        const WITH_CONFIGS: [WriterOp; 4] = [
            WriterOp::Inscribe,
            WriterOp::Inscribe,
            WriterOp::Inscribe,
            WriterOp::Config,
        ];
        const WITH_NOTES: [WriterOp; 7] = [
            WriterOp::Inscribe,
            WriterOp::Inscribe,
            WriterOp::Transfer,
            WriterOp::Inscribe,
            WriterOp::Withdraw,
            WriterOp::Inscribe,
            WriterOp::Config,
        ];
        match self.top {
            Layer::Chain | Layer::Lineage => &INSCRIPTIONS,
            Layer::Configs => &WITH_CONFIGS,
            Layer::Notes | Layer::Restarts if self.known_issues => &OP_CYCLE,
            Layer::Notes | Layer::Restarts => &WITH_NOTES,
        }
    }
}

/// Scenarios drawing from the layers up to `top`; `known_issues` widens the
/// schedule to what the SDK does not handle yet.
fn scenario(top: Layer, known_issues: bool) -> impl Strategy<Value = Scenario> {
    let messages = if top >= Layer::Lineage {
        1..=4usize
    } else {
        0..=0usize
    };
    (
        prop_oneof![3 => Just(chains::linear()), 1 => Just(chains::partitions())],
        1..=3usize,
        1..=2usize,
        messages,
        prop::collection::vec(step(top), 8..32),
    )
        .prop_flat_map(move |(chain, sequencers, writers, messages, schedule)| {
            let rules = if top >= Layer::Configs {
                let timeframe = if sequencers == 1 {
                    prop_oneof![Just(0u32), Just(3u32)].boxed()
                } else {
                    prop_oneof![Just(3u32), Just(5u32)].boxed()
                };
                let timeout = prop_oneof![Just(0u32), Just(4u32), Just(6u32)].boxed();
                (timeframe, timeout).boxed()
            } else {
                Just((3u32, 0u32)).boxed()
            };
            rules.prop_map(move |(timeframe, timeout)| Scenario {
                top,
                chain: chain.clone(),
                sequencers,
                writers,
                messages,
                timeframe,
                timeout,
                known_issues,
                schedule: schedule.clone(),
            })
        })
}

const NAMES: [&str; 3] = ["SEQ_A", "SEQ_B", "SEQ_C"];
const WRITERS: [&str; 2] = [FOREIGN, FOREIGN_2];

/// Bring the peer node, if any, level with the primary and vice versa.
fn sync_peer(world: &World) {
    if let Some(peer) = &world.peer {
        let _ = peer.import_from(world.node());
        let _ = world.node().import_from(peer);
        world.node().gossip_with(peer);
    }
}

fn running(world: &World, index: usize) -> bool {
    index < world.harness.seqs.len() && world.harness.seqs[index].is_running()
}

async fn apply_client(
    world: &mut World,
    client: Client,
    counter: usize,
    scenario: &Scenario,
    choices: &[SharedChoices],
) {
    match client {
        Client::Quiet => {}
        Client::Drop => world.apply_condition(Condition::Drop).await,
        Client::NodeDown => world.apply_condition(Condition::NodeDown).await,
        Client::Restart { index, fresh } => {
            world
                .apply_condition(Condition::Restart { index, fresh })
                .await;
        }
        Client::FetchFail { count } => {
            world.apply_condition(Condition::FetchFail { count }).await;
        }
        Client::GasPrice { percent } => {
            world.set_gas_prices(BASE_GAS_PRICE * (100 + percent) / 100);
        }
        Client::Deposit { .. } => {
            if !world.user_utxos.is_empty() {
                let utxo = world.user_utxos.remove(0);
                // The user wallet may be out of unreserved fee notes.
                drop(
                    world
                        .try_deposit(&[utxo], &format!("deposit {counter}"))
                        .await,
                );
            }
        }
        Client::Withdraw { index, amount } => {
            if running(world, index) {
                // Insufficient channel funds or a dead node: nothing to withdraw.
                let _ = world
                    .withdraw(
                        index,
                        amount,
                        scenario.known_issues,
                        &format!("withdraw {counter}"),
                    )
                    .await;
            }
        }
        Client::Abandon { index } => {
            if let Some(choices) = choices.get(index) {
                choices.lock().expect("choices").abandon_next();
            }
        }
        Client::Burst { index, count } => {
            if let Some(choices) = choices.get(index) {
                let payloads = (0..count)
                    .map(|k| inscription(&format!("{}-burst-{counter}-{k}", NAMES[index])))
                    .collect();
                choices.lock().expect("choices").burst(payloads);
            }
        }
        Client::Adopt { index, on } => {
            if let Some(choices) = choices.get(index) {
                choices.lock().expect("choices").adopt(on);
            }
        }
    }
}

async fn apply(
    world: &mut World,
    step: Step,
    counter: usize,
    scenario: &Scenario,
    choices: &[SharedChoices],
) {
    tracing::debug!(counter, ?step, tip = ?world.node().tip(), lib = ?world.node().lib(), "step");
    world.harness.advance(World::STEP_DURATION).await;
    apply_client(world, step.client, counter, scenario, choices).await;
    // During a split only the network itself may mine, so the merge stays
    // within what the primary can reorg; plain growth on two nodes is the
    // chain's own gossip step.
    if world.partitioned()
        || (world.chain.is_partitions() && matches!(step.shape, Shape::Extend { .. }))
    {
        world.step().await;
    } else {
        world.apply_shape(step.shape, step.posted).await;
    }
    sync_peer(world);
    world.harness.settle().await;
}

async fn run(scenario: Scenario) {
    let mut world = World::with_funding(scenario.chain.clone(), 200, FUNDING_NOTE_VALUE);
    // Prices bind from the next block; funding reads them from the tip.
    world.set_gas_prices(BASE_GAS_PRICE);
    world.harness.mine().await;
    sync_peer(&world);
    let names = &NAMES[..scenario.sequencers];
    // The writers are always accredited: other sequencers' txs show up
    // mined in every run, as many as the schedule has steps.
    let writers = &WRITERS[..scenario.writers];
    let mut accredited: Vec<&str> = names[1..].to_vec();
    accredited.extend(writers);
    let a = world.start("SEQ_A", passive()).await;
    world
        .config_finalized(a, &accredited, scenario.timeframe, scenario.timeout)
        .await;
    world.stop(a);
    for writer in writers {
        world.add_writer_with(writer, scenario.writer_ops());
    }
    world.enable_foreign_traffic(scenario.schedule.len());
    // The notes the schedule deposits exist before the first step.
    let deposits: Vec<u64> = scenario
        .schedule
        .iter()
        .filter_map(|step| match step.client {
            Client::Deposit { value } => Some(value),
            _ => None,
        })
        .collect();
    if !deposits.is_empty() {
        world.coin_split(&deposits).await;
    }
    let mut expected: Vec<String> = Vec::new();
    let claims = Claims::default();
    let choices: Vec<SharedChoices> = names.iter().map(|_| SharedChoices::default()).collect();
    for (i, name) in names.iter().enumerate() {
        let planned: Vec<_> = (0..scenario.messages)
            .map(|m| inscription(&format!("{name}-{m}")))
            .collect();
        expected.extend(planned.iter().map(payload_str));
        let config = world.config_with_depth(usize::MAX);
        let policy = Box::new(
            ReferenceConsumer::new(&seq_key(name), planned)
                .with_choices(Arc::clone(&choices[i]), Arc::clone(&claims)),
        );
        if i == 0 && world.harness.seqs.len() == 1 {
            world.harness.set_policy(0, policy);
            world.restart(0, None).await;
        } else {
            world
                .start_as_with(name, seq_key(name), config, policy)
                .await;
        }
    }

    for (counter, step) in scenario.schedule.clone().into_iter().enumerate() {
        apply(&mut world, step, counter, &scenario, &choices).await;
    }
    // Nothing finalizes under a frozen LIB.
    if world.node().is_lib_frozen() {
        world.node().thaw_lib();
        if let Some(peer) = &world.peer {
            peer.thaw_lib();
        }
    }
    world.settle_chain(400).await;

    // The layers stack: a broken view is reported before the lineage it
    // would have broken, and the lineage before the layers above it.
    world.harness.assert_layers_through(Layer::Chain);
    let finalized = world.indexer_finalized().await;
    assert_payloads_landed_once(&finalized, expected, &choices);
    assert_one_lineage(&finalized);
    world.harness.assert_layers();
}

/// Every planned payload, and every burst that found a turn, finalized
/// exactly once; an abandoned payload may still land once, through a
/// revived branch or another sequencer adopting it, but never twice.
fn assert_payloads_landed_once(
    finalized: &[FinalizedTx],
    mut expected: Vec<String>,
    choices: &[SharedChoices],
) {
    let payloads = common::inscription_payloads(finalized);
    let mut abandoned: HashSet<String> = HashSet::new();
    for choices in choices {
        let choices = choices.lock().expect("choices");
        expected.extend(choices.published.iter().map(payload_str));
        abandoned.extend(choices.abandoned.iter().map(payload_str));
    }
    for payload in &expected {
        let count = payloads.iter().filter(|p| *p == payload).count();
        if abandoned.contains(payload) {
            assert!(count <= 1, "abandoned {payload} finalized {count} times");
        } else {
            assert_eq!(count, 1, "{payload} finalized {count} times");
        }
    }
}

/// Finalized inscriptions form one chain: each parent is the previous
/// message, and every message id appears once.
fn assert_one_lineage(finalized: &[FinalizedTx]) {
    let mut previous = None;
    let mut seen = HashSet::new();
    for op in finalized.iter().flat_map(|tx| tx.ops.iter()) {
        let FinalizedOp::Inscription(info) = op else {
            continue;
        };
        if let Some(previous) = previous {
            assert_eq!(
                info.parent_msg,
                previous,
                "lineage broken at {}",
                payload_str(&info.payload)
            );
        }
        assert!(seen.insert(info.this_msg), "message id repeated");
        previous = Some(info.this_msg);
    }
}

fn block_on(scenario: Scenario) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("runtime");
    runtime.block_on(run(scenario));
}

// The ladder: each rung draws from one more layer than the one below, so
// the lowest failing rung names the layer the SDK gets wrong.
proptest! {
    #![proptest_config(ProptestConfig {
        cases: 24,
        max_shrink_iters: 200,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    #[ignore = "sampled hunt: run locally or on a schedule"]
    fn chain_shapes_keep_the_view(scenario in scenario(Layer::Chain, false)) {
        block_on(scenario);
    }

    #[test]
    #[ignore = "sampled hunt: run locally or on a schedule"]
    fn own_messages_land_once(scenario in scenario(Layer::Lineage, false)) {
        block_on(scenario);
    }

    #[test]
    #[ignore = "sampled hunt: run locally or on a schedule"]
    fn configs_move_the_turns(scenario in scenario(Layer::Configs, false)) {
        block_on(scenario);
    }

    #[test]
    #[ignore = "sampled hunt: run locally or on a schedule"]
    fn notes_follow_the_ledger(scenario in scenario(Layer::Notes, false)) {
        block_on(scenario);
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 48,
        max_shrink_iters: 200,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    #[ignore = "sampled hunt: run locally or on a schedule"]
    fn every_schedule_finalizes_each_payload_once(scenario in scenario(Layer::Restarts, false)) {
        block_on(scenario);
    }

    /// The same property over the known issues: withdraws from unfinalized
    /// notes, which wedge the sequencer when the note's deposit is reorged
    /// away for good, and two-inscription txs, which a config landing drops
    /// from the view unreported.
    #[test]
    #[ignore = "known issues: dead-input withdraws resubmitted forever; config shed skips custom txs"]
    fn every_schedule_with_known_issues_finalizes_each_payload_once(
        scenario in scenario(Layer::Restarts, true)
    ) {
        block_on(scenario);
    }
}
