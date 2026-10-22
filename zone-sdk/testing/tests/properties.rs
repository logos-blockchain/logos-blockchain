//! The sampled hunt: a scenario is a list of events drawn at random from
//! everything the world can do, applied in order, and judged by the layers
//! after every one of them. The chain's shape (blocks, empty slots, forks
//! of every depth and outcome, late revivals, lagging and duplicate
//! deliveries, frozen and jumping LIB, pruning), the blocks' content (which
//! posted transactions land in what order, next to other writers'
//! inscriptions, note changes and configs), the client's condition (dropped
//! streams, node outages, restarts, failing fetches, price moves) and the
//! zone's own actions (publishes, bursts, abandoned orphans, adoptions,
//! configs, deposits, withdraws, pins, hand-built txs, stopping and starting
//! sequencers) all come from one vocabulary, [`Event`].
//!
//! The vocabulary is drawn one layer at a time, so a rung of the ladder
//! that fails names what the SDK gets wrong: only other writers'
//! inscriptions on a moving chain; plus the sequencers' own messages and
//! the choices a zone makes about them; plus config changes and posting
//! rules; plus deposits, withdraws, pins and note transfers; plus restarts,
//! outages and price moves. A failing roll is shrunk to the smallest
//! schedule that still fails, which then becomes a hand-written story.

use std::{
    collections::HashSet,
    sync::atomic::{AtomicUsize, Ordering},
};

use common::{FINALITY_DEPTH, FOREIGN, FOREIGN_2, KnownIssue, Posted, World, inscription, passive};
use lb_zone_sdk::sequencer::{FinalizedOp, FinalizedTx};
use logos_blockchain_zone_sdk_testing::{
    chains::{self, Chain, Condition, Shape},
    events::Event,
    layers::Layer,
    policy::payload_str,
    world as common,
    writers::{OP_CYCLE, WriterOp},
};
use proptest::{prelude::*, strategy::Union};

/// Gas prices every run starts at; large enough that a percent rise is a
/// different integer price.
const BASE_GAS_PRICE: u64 = 100;
/// Largest price rise an event applies: below the funding reserve
/// (`FundingConfig::DEFAULT_PRIORITY_FEE_PERCENT`), so a tx funded at the
/// base price is still paid for after the rise.
const MAX_GAS_RISE_PERCENT: u64 = 10;
/// Funding notes cover one atomic bundle at the highest price.
const FUNDING_NOTE_VALUE: u64 = 500_000;

const NAMES: [&str; 3] = ["SEQ_A", "SEQ_B", "SEQ_C"];
const WRITERS: [&str; 2] = [FOREIGN, FOREIGN_2];

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

type Arm = (u32, BoxedStrategy<Event>);

/// Payloads distinct across a run with overwhelming probability.
fn payloads(count: impl Strategy<Value = usize>) -> impl Strategy<Value = Vec<String>> {
    count
        .prop_flat_map(|n| prop::collection::vec(any::<u32>(), n))
        .prop_map(|tags| tags.into_iter().map(|t| format!("event-{t:08x}")).collect())
}

/// The zone's own actions: publishes, bursts, abandoned orphans, adoptions
/// and hand-built txs.
fn zone_arms(top: Layer, sequencers: usize) -> Vec<Arm> {
    let lineage = u32::from(top >= Layer::Lineage);
    let index = move || 0..sequencers;
    vec![
        (
            2 * lineage,
            (index(), payloads(Just(1)))
                .prop_map(|(index, p)| Event::Publish {
                    index,
                    payload: p[0].clone(),
                })
                .boxed(),
        ),
        (
            lineage,
            index().prop_map(|index| Event::Abandon { index }).boxed(),
        ),
        (
            lineage,
            (index(), payloads(1..=3usize))
                .prop_map(|(index, payloads)| Event::Burst { index, payloads })
                .boxed(),
        ),
        (
            lineage,
            (index(), prop::bool::ANY)
                .prop_map(|(index, on)| Event::Adopt { index, on })
                .boxed(),
        ),
        (
            lineage,
            (index(), payloads(1..=3usize))
                .prop_map(|(index, payloads)| Event::Custom { index, payloads })
                .boxed(),
        ),
    ]
}

/// Configs and note moves. Every running sequencer stays accredited (the
/// world adds them), so a config only varies the writers it accredits and
/// the posting rules, never both zero: that freezes the turn on the first
/// key.
fn channel_arms(top: Layer, sequencers: usize) -> Vec<Arm> {
    let configs = u32::from(top >= Layer::Configs);
    let notes = u32::from(top >= Layer::Notes);
    let index = move || 0..sequencers;
    let writer_names: Vec<String> = WRITERS.iter().map(|s| (*s).to_owned()).collect();
    vec![
        (
            configs,
            (
                index(),
                prop::sample::subsequence(writer_names, 0..=2usize),
                prop_oneof![
                    Just((0u32, 4u32)),
                    Just((0u32, 6u32)),
                    Just((3u32, 0u32)),
                    Just((3u32, 4u32)),
                    Just((5u32, 0u32)),
                    Just((5u32, 6u32)),
                ],
            )
                .prop_map(|(index, names, (timeframe, timeout))| Event::Config {
                    index,
                    names,
                    timeframe,
                    timeout,
                })
                .boxed(),
        ),
        (2 * notes, Just(Event::Deposit).boxed()),
        (
            2 * notes,
            (index(), 1..=3u64, prop::bool::ANY)
                .prop_map(move |(index, amount, unfinalized)| Event::Withdraw {
                    index,
                    amount,
                    unfinalized,
                })
                .boxed(),
        ),
        (
            notes,
            index().prop_map(|index| Event::Pin { index }).boxed(),
        ),
    ]
}

/// Outages, restarts, failing fetches, price moves, stops and starts.
fn client_arms(top: Layer, sequencers: usize) -> Vec<Arm> {
    let restarts = u32::from(top >= Layer::Restarts);
    let index = move || 0..sequencers;
    vec![
        (restarts, Just(Event::Client(Condition::Drop)).boxed()),
        (restarts, Just(Event::Client(Condition::NodeDown)).boxed()),
        (
            restarts,
            (index(), prop::bool::weighted(0.25))
                .prop_map(|(index, fresh)| Event::Client(Condition::Restart { index, fresh }))
                .boxed(),
        ),
        (
            restarts,
            (1..=2usize)
                .prop_map(|count| Event::Client(Condition::FetchFail { count }))
                .boxed(),
        ),
        (
            restarts,
            (0..=MAX_GAS_RISE_PERCENT)
                .prop_map(|percent| {
                    Event::Client(Condition::GasPrice {
                        price: BASE_GAS_PRICE * (100 + percent) / 100,
                    })
                })
                .boxed(),
        ),
        (
            restarts,
            index().prop_map(|index| Event::Stop { index }).boxed(),
        ),
        (
            restarts,
            index().prop_map(|index| Event::Start { index }).boxed(),
        ),
    ]
}

/// One event the `top` layer admits: chain moves at every rung, the zone's
/// own actions from [`Layer::Lineage`], configs from [`Layer::Configs`],
/// note moves from [`Layer::Notes`], outages and restarts from
/// [`Layer::Restarts`].
fn event(top: Layer, sequencers: usize) -> impl Strategy<Value = Event> {
    let mut arms: Vec<Arm> = vec![(
        12,
        (shape(), posted())
            .prop_map(|(shape, posted)| Event::Chain { shape, posted })
            .boxed(),
    )];
    arms.extend(zone_arms(top, sequencers));
    arms.extend(channel_arms(top, sequencers));
    arms.extend(client_arms(top, sequencers));
    // A union rejects a zero weight: the layers below `top` draw nothing.
    Union::new_weighted(arms.into_iter().filter(|(weight, _)| *weight > 0).collect())
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
    schedule: Vec<Event>,
}

impl Scenario {
    /// The ops the writers cycle through: inscriptions always, configs from
    /// [`Layer::Configs`], note transfers, withdraws and two-inscription txs
    /// from [`Layer::Notes`].
    const fn writer_ops(&self) -> &'static [WriterOp] {
        const INSCRIPTIONS: [WriterOp; 1] = [WriterOp::Inscribe];
        const WITH_CONFIGS: [WriterOp; 4] = [
            WriterOp::Inscribe,
            WriterOp::Inscribe,
            WriterOp::Inscribe,
            WriterOp::Config,
        ];
        match self.top {
            Layer::Chain | Layer::Lineage => &INSCRIPTIONS,
            Layer::Configs => &WITH_CONFIGS,
            Layer::Notes | Layer::Restarts => &OP_CYCLE,
        }
    }
}

/// Events per schedule: `SIM_EVENTS` (`min..max`, default `16..256`). The
/// long end is what reaches finalization under stacked reorgs, expired fee
/// note reservations and a drained funding wallet.
fn schedule_len() -> std::ops::Range<usize> {
    let spec = std::env::var("SIM_EVENTS").unwrap_or_else(|_| "16..256".into());
    let (min, max) = spec.split_once("..").expect("SIM_EVENTS is min..max");
    let (min, max): (usize, usize) = (min.parse().expect("min"), max.parse().expect("max"));
    assert!(min < max, "SIM_EVENTS needs min < max");
    min..max
}

/// Scenarios drawing from the layers up to `top`.
fn scenario(top: Layer) -> impl Strategy<Value = Scenario> {
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
    )
        .prop_flat_map(move |(chain, sequencers, writers, messages)| {
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
            let schedule = prop::collection::vec(event(top, sequencers), schedule_len());
            (rules, schedule).prop_map(move |((timeframe, timeout), schedule)| Scenario {
                top,
                chain: chain.clone(),
                sequencers,
                writers,
                messages,
                timeframe,
                timeout,
                schedule,
            })
        })
}

/// Bring the peer node, if any, level with the primary and vice versa.
fn sync_peer(world: &World) {
    if let Some(peer) = &world.peer {
        let _ = peer.import_from(world.node());
        let _ = world.node().import_from(peer);
        world.node().gossip_with(peer);
    }
}

/// A world with the channel open, the writers accredited and the
/// sequencers started with their planned payloads; those payloads.
async fn world_for(scenario: &Scenario) -> (World, Vec<String>) {
    // Every publish, re-fund and bundle reserves a fee note for up to ten
    // LIB blocks, or for good under a frozen LIB: the wallet grows with the
    // schedule so a long one is not starved.
    let funding_notes = 200.max(2 * scenario.schedule.len());
    let mut world = World::with_funding(scenario.chain.clone(), funding_notes, FUNDING_NOTE_VALUE);
    // Prices bind from the next block; funding reads them from the tip.
    world.set_gas_prices(BASE_GAS_PRICE);
    world.harness.mine().await;
    sync_peer(&world);
    let names = &NAMES[..scenario.sequencers];
    // The writers are always accredited: other sequencers' txs show up
    // mined in every run, as many as the schedule has events.
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
    // The notes the schedule deposits exist before the first event.
    let deposits = scenario
        .schedule
        .iter()
        .filter(|event| matches!(event, Event::Deposit))
        .count();
    if deposits > 0 {
        world.coin_split(&vec![2; deposits]).await;
    }
    let mut expected: Vec<String> = Vec::new();
    // SEQ_A was started to open the channel; its slot is reused.
    world.harness.seqs.clear();
    for name in names {
        let planned: Vec<_> = (0..scenario.messages)
            .map(|m| inscription(&format!("{name}-{m}")))
            .collect();
        expected.extend(planned.iter().map(payload_str));
        world.start_reference(name, planned, usize::MAX).await;
    }
    (world, expected)
}

async fn run(scenario: Scenario) {
    let (mut world, mut expected) = world_for(&scenario).await;
    for event in scenario.schedule.clone() {
        tracing::debug!(?event, tip = ?world.node().tip(), lib = ?world.node().lib(), "event");
        world.harness.advance(World::STEP_DURATION).await;
        world.apply_event(event).await;
        sync_peer(&world);
        world.harness.settle().await;
    }
    // Nothing finalizes under a frozen LIB, a stopped sequencer never
    // drains, and the writers would inscribe through the settle while their
    // budget lasts: the schedule is over, so only its content has to land.
    world.end_foreign_traffic();
    if world.node().is_lib_frozen() {
        world.node().thaw_lib();
        if let Some(peer) = &world.peer {
            peer.thaw_lib();
        }
    }
    for index in 0..world.harness.seqs.len() {
        world.apply_event(Event::Start { index }).await;
    }
    let settled = world.try_settle_chain(400).await;
    if let Some(issue) = world.known_issue(settled) {
        record_known_issue(issue, &scenario);
        return;
    }
    assert!(
        settled,
        "chain did not settle within 400 steps:\n{}",
        world.diagnostics()
    );

    // The layers stack: a broken view is reported before the lineage it
    // would have broken, and the lineage before the layers above it.
    if let Some(report) = world.harness.layer_report(Layer::Chain) {
        panic!("{report}\n{}", world.diagnostics());
    }
    let finalized = world.indexer_finalized().await;
    expected.extend(world.event_payloads.published.iter().cloned());
    assert_payloads_landed_once(&finalized, expected, &mut world);
    assert_one_lineage(&finalized);
    if let Some(report) = world.harness.layer_report(Layer::Restarts) {
        panic!("{report}\n{}", world.diagnostics());
    }
}

/// Every planned payload, every publish and every burst that found a turn
/// finalized exactly once; an abandoned payload, a pin or a hand-built tx
/// may land once, through a revived branch or another sequencer adopting
/// it, but never twice.
fn assert_payloads_landed_once(
    finalized: &[FinalizedTx],
    mut expected: Vec<String>,
    world: &mut World,
) {
    let payloads = common::inscription_payloads(finalized);
    let mut at_most_once: HashSet<String> =
        world.event_payloads.at_most_once.iter().cloned().collect();
    for choices in world.choices() {
        let choices = choices.lock().expect("choices");
        expected.extend(choices.published.iter().map(payload_str));
        // An adopted writer's orphan a consumer let die is the writer's to
        // re-land, beside whoever else adopted it.
        at_most_once.extend(
            choices
                .abandoned
                .iter()
                .map(payload_str)
                .filter(|payload| !WRITERS.iter().any(|writer| payload.starts_with(writer))),
        );
    }
    expected.extend(at_most_once.iter().cloned());
    for payload in &expected {
        let count = payloads.iter().filter(|p| *p == payload).count();
        let once = if at_most_once.contains(payload) {
            count <= 1
        } else {
            count == 1
        };
        assert!(
            once,
            "{payload} finalized {count} times:\n{}",
            world.diagnostics()
        );
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

/// A schedule that ran into a known SDK issue is counted and logged, not
/// failed: the hunt keeps drawing from the whole vocabulary, and whatever
/// else breaks in those regions still fails. The count is printed so a run
/// shows how often it got there (`--no-capture` or `--success-output
/// immediate`), and reaches zero once the fix lands.
fn record_known_issue(issue: KnownIssue, scenario: &Scenario) {
    static HITS: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];
    let hits = HITS[issue as usize].fetch_add(1, Ordering::Relaxed) + 1;
    eprintln!(
        "known issue {issue:?}: {hits} schedule(s) so far ({} events, {} sequencers, {} writers)",
        scenario.schedule.len(),
        scenario.sequencers,
        scenario.writers
    );
    tracing::warn!(?issue, schedule = ?scenario.schedule, "known issue hit");
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
    fn chain_shapes_keep_the_view(scenario in scenario(Layer::Chain)) {
        block_on(scenario);
    }

    #[test]
    fn own_messages_land_once(scenario in scenario(Layer::Lineage)) {
        block_on(scenario);
    }

    #[test]
    fn configs_move_the_turns(scenario in scenario(Layer::Configs)) {
        block_on(scenario);
    }

    #[test]
    fn notes_follow_the_ledger(scenario in scenario(Layer::Notes)) {
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
    fn every_schedule_finalizes_each_payload_once(scenario in scenario(Layer::Restarts)) {
        block_on(scenario);
    }
}
