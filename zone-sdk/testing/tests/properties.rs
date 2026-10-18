//! Property tests over the three things that shape what a sequencer sees:
//! the chain's shape (blocks, empty slots, forks of every depth and
//! outcome, late revivals, lagging and duplicate deliveries, frozen and
//! jumping LIB, pruning), the blocks' content (which posted transactions
//! land in what order, next to other writers' inscriptions, note changes
//! and configs) and the client's condition (dropped streams, node outages,
//! restarts, failing fetches, price moves, user deposits and withdraws).
//! Every combination must leave the view contract, the mirrored view and the
//! oracles intact and every planned payload finalized exactly once, in one
//! lineage.

use std::collections::HashSet;

use common::{
    FINALITY_DEPTH, FOREIGN, FOREIGN_2, Network, Posted, World, inscription, passive, seq_key,
};
use lb_core::mantle::{Note, NoteId, ledger::Outputs, transactions::GasPrices};
use lb_zone_sdk::sequencer::{FinalizedOp, WithdrawArg, WithdrawInputs};
use logos_blockchain_zone_sdk_testing::{
    harness::RECONNECT_DELAY,
    policy::{RepublishLineagePolicy, payload_str},
    world as common,
};
use proptest::prelude::*;

/// Gas prices every run starts at; large enough that a percent rise is a
/// different integer price.
const BASE_GAS_PRICE: u64 = 100;
/// Largest price rise a step applies: below the funding reserve
/// (`FundingConfig::DEFAULT_PRIORITY_FEE_PERCENT`), so a tx funded at the
/// base price is still paid for after the rise.
const MAX_GAS_RISE_PERCENT: u64 = 10;
/// Funding notes cover one atomic bundle at the highest price.
const FUNDING_NOTE_VALUE: u64 = 500_000;

/// How the chain moves in one step.
#[derive(Clone, Copy, Debug)]
enum Shape {
    /// One block on the tip, `gap` slots later; a gap above one leaves
    /// empty slots.
    Extend { gap: u64 },
    /// A branch from `depth` blocks below the tip with `length` blocks:
    /// longer than `depth` it wins, equal it ties and loses, shorter it
    /// stays dormant.
    Fork { depth: usize, length: usize },
    /// Extend the dormant branch by `blocks`, maybe overtaking the tip.
    Revive { blocks: usize },
    /// `silent` blocks the node never announces, then one it does.
    Lagging { silent: usize },
    /// The tip moves silently and a competing block is announced instead.
    SplitTip,
    /// The tip's event delivered again.
    Duplicate,
    /// LIB stops advancing, as under the bootstrap fork-choice rule.
    FreezeLib,
    /// LIB catches up with the tip at once.
    ThawLib,
    /// Forks below LIB are forgotten; their blocks can no longer be fetched.
    Prune,
}

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

fn client() -> impl Strategy<Value = Client> {
    prop_oneof![
        10 => Just(Client::Quiet),
        1 => Just(Client::Drop),
        1 => Just(Client::NodeDown),
        1 => (0..3usize, prop::bool::weighted(0.25))
            .prop_map(|(index, fresh)| Client::Restart { index, fresh }),
        1 => (1..=2usize).prop_map(|count| Client::FetchFail { count }),
        1 => (0..=MAX_GAS_RISE_PERCENT).prop_map(|percent| Client::GasPrice { percent }),
        2 => (1..=3u64).prop_map(|value| Client::Deposit { value }),
        2 => (0..3usize, 1..=3u64).prop_map(|(index, amount)| Client::Withdraw { index, amount }),
    ]
}

fn step() -> impl Strategy<Value = Step> {
    (shape(), posted(), client()).prop_map(|(shape, posted, client)| Step {
        shape,
        posted,
        client,
    })
}

#[derive(Clone, Debug)]
struct Scenario {
    network: Network,
    sequencers: usize,
    writers: usize,
    messages: usize,
    timeframe: u32,
    timeout: u32,
    /// Dead-end and dormant fork blocks take posted transactions (so a
    /// deposit can land only off-chain) and withdraws may spend unfinalized
    /// notes: the two things the SDK does not handle yet.
    known_issues: bool,
    schedule: Vec<Step>,
}

/// `known_issues` widens the schedule to what the SDK does not handle yet.
fn scenario(known_issues: bool) -> impl Strategy<Value = Scenario> {
    (
        prop_oneof![3 => Just(Network::Linear), 1 => Just(Network::Partitions)],
        1..=3usize,
        1..=2usize,
        1..=4usize,
        prop::collection::vec(step(), 8..32),
    )
        .prop_flat_map(move |(network, sequencers, writers, messages, schedule)| {
            let timeframe = if sequencers == 1 {
                prop_oneof![Just(0u32), Just(3u32)].boxed()
            } else {
                prop_oneof![Just(3u32), Just(5u32)].boxed()
            };
            let timeout = prop_oneof![Just(0u32), Just(4u32), Just(6u32)];
            (timeframe, timeout).prop_map(move |(timeframe, timeout)| Scenario {
                network,
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

fn set_gas_prices(world: &World, price: u64) {
    let prices = GasPrices::new(price, price);
    world.node().set_gas_prices(prices.clone());
    if let Some(peer) = &world.peer {
        peer.set_gas_prices(prices);
    }
}

/// Finalized channel notes covering `amount`, largest first.
fn finalized_inputs(world: &mut World, index: usize, amount: u64) -> Option<Vec<NoteId>> {
    let mut notes = world.wallet(index).finalized;
    notes.sort_by(|a, b| b.value.cmp(&a.value).then(a.note_id.cmp(&b.note_id)));
    let mut covered = 0;
    let mut inputs = Vec::new();
    for note in notes {
        if covered >= amount {
            break;
        }
        covered += note.value;
        inputs.push(note.note_id);
    }
    (covered >= amount).then_some(inputs)
}

fn running(world: &World, index: usize) -> bool {
    index < world.harness.seqs.len() && world.harness.seqs[index].is_running()
}

async fn apply_client(world: &mut World, client: Client, counter: usize, scenario: &Scenario) {
    match client {
        Client::Quiet => {}
        Client::Drop => {
            world.node().drop_streams();
            if let Some(peer) = &world.peer {
                peer.drop_streams();
            }
            world.harness.settle().await;
            world.harness.advance(RECONNECT_DELAY).await;
        }
        Client::NodeDown => {
            world.node().node_down();
            world.harness.settle().await;
            world.harness.advance(World::STEP_DURATION).await;
            world.node().node_up();
            world.harness.advance(RECONNECT_DELAY).await;
        }
        Client::Restart { index, fresh } => {
            if running(world, index) {
                // A fresh start forgets pending publishes; the zone would
                // re-plan them, which this schedule does not model.
                let checkpoint = if fresh && world.own_pending(index).is_empty() {
                    None
                } else {
                    world.harness.seqs[index].checkpoint()
                };
                world.restart(index, checkpoint).await;
            }
        }
        Client::FetchFail { count } => {
            world.node().fail_next_fetches(count);
        }
        Client::GasPrice { percent } => {
            set_gas_prices(world, BASE_GAS_PRICE * (100 + percent) / 100);
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
                let inputs = if scenario.known_issues {
                    Some(WithdrawInputs::Auto)
                } else {
                    finalized_inputs(world, index, amount).map(WithdrawInputs::Explicit)
                };
                let Some(inputs) = inputs else {
                    return;
                };
                let outputs = Outputs::new([Note::new(amount, world.user_pk())]);
                // Insufficient channel funds or a dead node: nothing to withdraw.
                drop(
                    world
                        .harness
                        .seq(index)
                        .handle()
                        .publish_atomic_withdraw(
                            inscription(&format!("withdraw {counter}")),
                            vec![WithdrawArg { outputs }],
                            inputs,
                        )
                        .await,
                );
                world.harness.settle().await;
            }
        }
    }
}

async fn apply_shape(world: &mut World, shape: Shape, posted: Posted, scenario: &Scenario) {
    // During a split only the network itself may mine, so the merge stays
    // within what the primary can reorg.
    if world.partitioned() {
        world.step().await;
        return;
    }
    let node = world.node().clone();
    // A fork that will not win may only carry posted transactions when the
    // run is about the known deposit issue.
    let losing = |posted| {
        if scenario.known_issues {
            posted
        } else {
            Posted::None
        }
    };
    match shape {
        Shape::Extend { gap } => {
            if world.network == Network::Partitions {
                world.step().await;
            } else {
                let _ = world.extend(gap, posted).await;
            }
        }
        Shape::Fork { depth, length } => {
            if node.canonical_chain().len() <= depth + 1 {
                let _ = world.extend(1, posted).await;
            } else {
                let posted = if length > depth {
                    posted
                } else {
                    losing(posted)
                };
                let _ = world.fork_with(depth, length, posted).await;
            }
        }
        Shape::Revive { blocks } => {
            let tip_height = node.block(node.tip()).map_or(0, |block| block.height);
            let overtakes = world
                .dormant
                .and_then(|dormant| node.block(dormant.tip))
                .is_some_and(|branch| branch.height + blocks as u64 > tip_height);
            let posted = if overtakes { posted } else { losing(posted) };
            if !world.revive(blocks, posted).await {
                let _ = world.extend(1, posted).await;
            }
        }
        Shape::Lagging { silent } => world.lagging(silent, posted).await,
        Shape::SplitTip => world.split_tip(losing(posted)).await,
        Shape::Duplicate => {
            node.reannounce(node.tip());
            world.harness.settle().await;
        }
        Shape::FreezeLib => {
            node.freeze_lib();
            if let Some(peer) = &world.peer {
                peer.freeze_lib();
            }
        }
        Shape::ThawLib => {
            node.thaw_lib();
            if let Some(peer) = &world.peer {
                peer.thaw_lib();
            }
            world.harness.settle().await;
        }
        Shape::Prune => {
            let _ = node.prune_forks();
        }
    }
}

async fn apply(world: &mut World, step: Step, counter: usize, scenario: &Scenario) {
    world.harness.advance(World::STEP_DURATION).await;
    apply_client(world, step.client, counter, scenario).await;
    apply_shape(world, step.shape, step.posted, scenario).await;
    sync_peer(world);
    world.harness.settle().await;
}

async fn run(scenario: Scenario) {
    let mut world = World::with_funding(scenario.network, 200, FUNDING_NOTE_VALUE);
    // Prices bind from the next block; funding reads them from the tip.
    set_gas_prices(&world, BASE_GAS_PRICE);
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
        world.add_writer(writer);
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
    for (i, name) in names.iter().enumerate() {
        let planned: Vec<_> = (0..scenario.messages)
            .map(|m| inscription(&format!("{name}-{m}")))
            .collect();
        expected.extend(planned.iter().map(payload_str));
        let config = world.config_with_depth(usize::MAX);
        let policy = RepublishLineagePolicy::new(planned);
        if i == 0 && world.harness.seqs.len() == 1 {
            world.harness.set_policy(0, Box::new(policy));
            world.restart(0, None).await;
        } else {
            world
                .start_as_with(name, seq_key(name), config, Box::new(policy))
                .await;
        }
    }

    // On two nodes a block carrying a deposit reaches the other node as a
    // competing block whenever the nodes mined the same height, and its
    // deposit is then reported from a non-canonical block: the known issue,
    // left to the known-issues property.
    let skip_deposits = scenario.network == Network::Partitions && !scenario.known_issues;
    for (counter, step) in scenario.schedule.clone().into_iter().enumerate() {
        let step = match step.client {
            Client::Deposit { .. } if skip_deposits => Step {
                client: Client::Quiet,
                ..step
            },
            _ => step,
        };
        apply(&mut world, step, counter, &scenario).await;
    }
    // Nothing finalizes under a frozen LIB.
    if world.node().is_lib_frozen() {
        world.node().thaw_lib();
        if let Some(peer) = &world.peer {
            peer.thaw_lib();
        }
    }
    world.settle_chain(400).await;

    let finalized = world.indexer_finalized().await;
    let payloads = common::inscription_payloads(&finalized);
    let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
    common::assert_exactly_once_any_order(&payloads, &expected);

    // Finalized inscriptions form one chain: each parent is the previous
    // message, and every message id appears once.
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
    world.harness.assert_view_contract();
}

fn block_on(scenario: Scenario) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("runtime");
    runtime.block_on(run(scenario));
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 48,
        max_shrink_iters: 200,
        ..ProptestConfig::default()
    })]

    #[test]
    fn every_schedule_finalizes_each_payload_once(scenario in scenario(false)) {
        block_on(scenario);
    }

    /// The same property over the known issues: dead-end forks, whose
    /// deposits are reported as if canonical, and withdraws from unfinalized
    /// notes, which wedge the sequencer when the note's deposit is reorged
    /// away for good.
    #[test]
    #[ignore = "known issues: fork deposits reported as canonical; dead-input withdraws resubmitted forever"]
    fn every_schedule_with_known_issues_finalizes_each_payload_once(
        scenario in scenario(true)
    ) {
        block_on(scenario);
    }
}
