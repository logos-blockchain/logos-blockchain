//! Owns the sequencers under test and drives them deterministically.
//!
//! Nothing runs on a background task: [`Harness::settle`] polls every
//! sequencer's `next_event` until no sequencer can make progress, running each
//! sequencer's policy and the view checker inline. The simulated node answers
//! synchronously, so a settled harness has processed every block mined so far
//! and posted every transaction the sequencers decided to post. Tests must run
//! on a paused tokio clock (`#[tokio::test(start_paused = true)]`) so SDK
//! timers only fire through [`Harness::advance`].

use std::{pin::pin, task::Poll, time::Duration};

use lb_core::{
    header::HeaderId,
    mantle::{gas::GasCost, ops::channel::ChannelId},
};
use lb_key_management_system_service::keys::{Ed25519Key, ZkKey};
use lb_zone_sdk::{
    node_types::TxHash,
    sequencer::{
        Event, FinalizedTx, FundingConfig, SequencerCheckpoint, SequencerConfig, ZoneSequencer,
    },
};
use num_bigint::BigUint;

use crate::{
    mirror::Mirror,
    node::SimNode,
    oracles::Oracles,
    policy::{PassivePolicy, Policy},
    view_checker::ViewChecker,
};

/// Polls `sequencer` once; `None` when it has nothing to do right now.
pub async fn poll_event(sequencer: &mut ZoneSequencer<SimNode>) -> Option<Event> {
    let mut fut = pin!(sequencer.next_event());
    std::future::poll_fn(|cx| {
        Poll::Ready(match fut.as_mut().poll(cx) {
            Poll::Ready(event) => Some(event),
            Poll::Pending => None,
        })
    })
    .await
}

/// Events one settle may drain before it is declared a busy loop.
const MAX_EVENTS_PER_SETTLE: usize = 10_000;

/// Resubmit interval used by the suite; fired explicitly via
/// [`Harness::advance`].
pub const RESUBMIT_INTERVAL: Duration = Duration::from_secs(3);
/// Reconnect delay used by the suite; fired explicitly via
/// [`Harness::advance`].
pub const RECONNECT_DELAY: Duration = Duration::from_secs(1);

/// The cucumber suite's sequencer configuration.
#[must_use]
pub const fn sequencer_config(funding: FundingConfig) -> SequencerConfig {
    SequencerConfig {
        resubmit_interval: RESUBMIT_INTERVAL,
        reconnect_delay: RECONNECT_DELAY,
        min_slots_remaining_in_turn: 2,
        ..SequencerConfig::new(funding)
    }
}

/// Funding config paying from `node`'s funding wallet, or a placeholder when
/// the node funds nothing.
#[must_use]
pub fn funding_config(node: &SimNode) -> FundingConfig {
    FundingConfig {
        funding_pk: node
            .funding_pk()
            .unwrap_or_else(|| ZkKey::from(BigUint::from(1u64)).to_public_key()),
        change_pk: None,
        max_tx_fee: GasCost::new(u64::MAX),
        priority_fee_percent: FundingConfig::DEFAULT_PRIORITY_FEE_PERCENT,
    }
}

/// Install a log subscriber from `RUST_LOG` once, so a failing run can be
/// replayed with the SDK's own log lines (`RUST_LOG=warn cargo test ...`).
pub fn init_tracing() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if std::env::var_os("RUST_LOG").is_none() {
            return;
        }
        drop(
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .with_test_writer()
                .try_init(),
        );
    });
}

/// Deterministic signing key from a seed.
#[must_use]
pub fn key(seed: u8) -> Ed25519Key {
    Ed25519Key::from_bytes(&[seed; 32])
}

/// One sequencer under test with its policy, checker and event log.
pub struct Seq {
    pub name: String,
    pub key: Ed25519Key,
    pub config: SequencerConfig,
    /// The node this sequencer talks to.
    pub node: SimNode,
    sequencer: Option<ZoneSequencer<SimNode>>,
    policy: Box<dyn Policy>,
    pub checker: ViewChecker,
    /// The consumer-side view, kept across restarts like a zone would.
    pub mirror: Mirror,
    /// Deposit, wallet and turn invariants against the node.
    pub oracles: Oracles,
    pub events: Vec<Event>,
}

impl Seq {
    /// The live sequencer; panics when stopped.
    pub const fn sequencer(&mut self) -> &mut ZoneSequencer<SimNode> {
        self.sequencer.as_mut().expect("sequencer is running")
    }

    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.sequencer.is_some()
    }

    #[must_use]
    pub fn checkpoint(&self) -> Option<SequencerCheckpoint> {
        self.sequencer.as_ref().and_then(ZoneSequencer::checkpoint)
    }

    /// `BlocksProcessed` events seen so far.
    pub fn block_events(&self) -> impl Iterator<Item = &Event> {
        self.events
            .iter()
            .filter(|e| matches!(e, Event::BlocksProcessed { .. }))
    }

    /// Every finalized tx reported through this sequencer's events, in order.
    pub fn finalized(&self) -> impl Iterator<Item = &FinalizedTx> {
        self.events.iter().flat_map(|e| match e {
            Event::BlocksProcessed { finalized, .. } => finalized.as_slice(),
            _ => &[],
        })
    }
}

/// Sequencers over one simulated node.
pub struct Harness {
    pub node: SimNode,
    pub channel_id: ChannelId,
    pub seqs: Vec<Seq>,
}

impl Harness {
    #[must_use]
    pub const fn new(node: SimNode, channel_id: ChannelId) -> Self {
        Self {
            node,
            channel_id,
            seqs: Vec::new(),
        }
    }

    /// Construct a sequencer on the harness node; it is not polled until the
    /// next settle.
    pub fn add(
        &mut self,
        name: &str,
        key: Ed25519Key,
        config: SequencerConfig,
        checkpoint: Option<SequencerCheckpoint>,
        policy: Box<dyn Policy>,
    ) -> usize {
        let node = self.node.clone();
        self.add_on(&node, name, key, config, checkpoint, policy)
    }

    /// Construct a sequencer talking to `node`, through a handle tagged for
    /// it so the node can tell what this sequencer's streams delivered.
    pub fn add_on(
        &mut self,
        node: &SimNode,
        name: &str,
        key: Ed25519Key,
        config: SequencerConfig,
        checkpoint: Option<SequencerCheckpoint>,
        policy: Box<dyn Policy>,
    ) -> usize {
        let node = node.tagged(self.seqs.len() as u64 + 1);
        let sequencer = ZoneSequencer::init_with_config(
            self.channel_id,
            key.clone(),
            node.clone(),
            config.clone(),
            checkpoint,
        );
        self.seqs.push(Seq {
            name: name.to_owned(),
            key,
            config,
            node,
            sequencer: Some(sequencer),
            policy,
            checker: ViewChecker::default(),
            mirror: Mirror::default(),
            oracles: Oracles::default(),
            events: Vec::new(),
        });
        self.seqs.len() - 1
    }

    /// Add a sequencer with the suite's default config and settle until it is
    /// ready.
    pub async fn start(&mut self, name: &str, key: Ed25519Key, policy: Box<dyn Policy>) -> usize {
        let config = sequencer_config(funding_config(&self.node));
        self.start_with(name, key, config, None, policy).await
    }

    pub async fn start_with(
        &mut self,
        name: &str,
        key: Ed25519Key,
        config: SequencerConfig,
        checkpoint: Option<SequencerCheckpoint>,
        policy: Box<dyn Policy>,
    ) -> usize {
        let node = self.node.clone();
        self.start_with_on(node, name, key, config, checkpoint, policy)
            .await
    }

    /// Start a sequencer talking to `node` and settle until it is ready.
    pub async fn start_with_on(
        &mut self,
        node: SimNode,
        name: &str,
        key: Ed25519Key,
        config: SequencerConfig,
        checkpoint: Option<SequencerCheckpoint>,
        policy: Box<dyn Policy>,
    ) -> usize {
        let index = self.add_on(&node, name, key, config, checkpoint, policy);
        self.settle_until_ready(index).await;
        index
    }

    /// Settle until sequencer `index` is ready, letting reconnect delays pass
    /// when the node fails it a fetch or a connection on the way.
    async fn settle_until_ready(&mut self, index: usize) {
        self.settle().await;
        for _ in 0..10 {
            if self.seqs[index].sequencer().is_ready() {
                return;
            }
            self.advance(RECONNECT_DELAY).await;
        }
        assert!(
            self.seqs[index].sequencer().is_ready(),
            "{} did not become ready",
            self.seqs[index].name
        );
    }

    /// Index of the sequencer called `name`.
    #[must_use]
    pub fn index(&self, name: &str) -> usize {
        self.seqs
            .iter()
            .position(|s| s.name == name)
            .unwrap_or_else(|| panic!("no sequencer {name}"))
    }

    pub fn seq(&mut self, index: usize) -> &mut ZoneSequencer<SimNode> {
        self.seqs[index].sequencer()
    }

    /// Replace the policy of a running sequencer.
    pub fn set_policy(&mut self, index: usize, policy: Box<dyn Policy>) {
        self.seqs[index].policy = policy;
    }

    /// Drop the sequencer instance; its log and checker stay for assertions.
    pub fn stop(&mut self, index: usize) {
        self.seqs[index].sequencer = None;
    }

    /// Start a fresh instance for slot `index` from `checkpoint`, keeping the
    /// policy, and settle until ready.
    pub async fn restart(&mut self, index: usize, checkpoint: Option<SequencerCheckpoint>) {
        let seq = &mut self.seqs[index];
        seq.oracles
            .restart_from(checkpoint.as_ref().map(|cp| cp.lib));
        seq.sequencer = Some(ZoneSequencer::init_with_config(
            self.channel_id,
            seq.key.clone(),
            seq.node.clone(),
            seq.config.clone(),
            checkpoint,
        ));
        seq.checker = ViewChecker::default();
        self.settle_until_ready(index).await;
    }

    /// Poll every running sequencer until none makes progress, then compare
    /// every mirror with the node.
    pub async fn settle(&mut self) {
        let mut idle_rounds = 0;
        let mut budget = MAX_EVENTS_PER_SETTLE;
        while idle_rounds < 3 {
            let mut progressed = false;
            for seq in &mut self.seqs {
                let Some(sequencer) = seq.sequencer.as_mut() else {
                    continue;
                };
                while let Some(event) = poll_event(sequencer).await {
                    progressed = true;
                    seq.checker.observe(&event);
                    seq.mirror.observe(&event);
                    seq.oracles.observe(&event, &seq.node, &seq.name);
                    seq.policy.on_event(sequencer, &event).await;
                    seq.events.push(event);
                    budget -= 1;
                    assert!(
                        budget > 0,
                        "{} never went idle: {MAX_EVENTS_PER_SETTLE} events in one settle, last {:?}",
                        seq.name,
                        seq.events.iter().rev().take(3).collect::<Vec<_>>()
                    );
                }
            }
            if progressed {
                idle_rounds = 0;
            } else {
                idle_rounds += 1;
                tokio::task::yield_now().await;
            }
        }
        for seq in &mut self.seqs {
            if let Some(sequencer) = seq.sequencer.as_mut() {
                let connected = sequencer.is_connected();
                seq.mirror
                    .check(&seq.node, connected, self.channel_id, &seq.name);
                seq.oracles
                    .check(sequencer, &seq.node, self.channel_id, &seq.name);
            }
        }
    }

    /// Mine one block and settle.
    pub async fn mine(&mut self) -> HeaderId {
        let id = self.node.mine();
        self.settle().await;
        id
    }

    /// Mine one empty block and settle.
    pub async fn mine_empty(&mut self) {
        let _ = self.node.mine_empty();
        self.settle().await;
    }

    pub async fn mine_n(&mut self, n: usize) {
        for _ in 0..n {
            self.mine().await;
        }
    }

    /// Mine and settle until `done` holds, at most `max_blocks` times.
    pub async fn mine_until(
        &mut self,
        max_blocks: usize,
        mut done: impl FnMut(&mut Self) -> bool,
    ) -> bool {
        for _ in 0..max_blocks {
            if done(self) {
                return true;
            }
            self.mine().await;
        }
        done(self)
    }

    /// Mine until `hash` is on the canonical chain.
    pub async fn include(&mut self, hash: TxHash, max_blocks: usize) {
        assert!(
            self.mine_until(max_blocks, |h| h.node.is_included(&hash))
                .await,
            "{hash:?} was not included within {max_blocks} blocks"
        );
    }

    /// Mine until `hash` is finalized.
    pub async fn finalize(&mut self, hash: TxHash, max_blocks: usize) {
        assert!(
            self.mine_until(max_blocks, |h| h.node.is_tx_finalized(&hash))
                .await,
            "{hash:?} was not finalized within {max_blocks} blocks"
        );
    }

    /// Mine empty blocks until LIB reaches the tip, settling after each.
    pub async fn finalize_tip(&mut self) {
        let tip = self.node.tip();
        while !self.node.is_finalized(tip) {
            self.mine_empty().await;
        }
    }

    /// Let SDK timers fire (resubmit, reconnect, turn wake-ups) and settle.
    pub async fn advance(&mut self, duration: Duration) {
        tokio::time::advance(duration).await;
        self.settle().await;
    }

    /// Finalized history as a fresh read-only sequencer replays it.
    pub fn indexer_finalized(&self) -> impl Future<Output = Vec<FinalizedTx>> + Send {
        replay_finalized(self.node.clone(), self.channel_id)
    }

    /// Every view-contract violation and mirror divergence recorded by any
    /// sequencer.
    #[must_use]
    pub fn violations(&self) -> Vec<String> {
        self.seqs
            .iter()
            .flat_map(|s| {
                s.checker
                    .violations()
                    .iter()
                    .map(move |v| format!("{}: {v}", s.name))
                    .chain(s.mirror.violations().iter().cloned())
                    .chain(s.oracles.violations().iter().cloned())
            })
            .collect()
    }

    pub fn assert_view_contract(&self) {
        let violations = self.violations();
        assert!(
            violations.is_empty(),
            "channel view contract violated:\n{}",
            violations.join("\n")
        );
    }
}

/// Replay the finalized history of `channel_id` with a fresh read-only
/// sequencer, up to its `Ready`.
pub async fn replay_finalized(node: SimNode, channel_id: ChannelId) -> Vec<FinalizedTx> {
    let mut reader = ZoneSequencer::init(
        channel_id,
        key(0xEE),
        node.clone(),
        funding_config(&node),
        None,
    );
    let mut finalized = Vec::new();
    let mut idle_rounds = 0;
    // A fetch the node fails sends the reader through a reconnect delay,
    // so idle polls let that delay pass.
    while idle_rounds < 10 {
        match poll_event(&mut reader).await {
            Some(Event::Ready) => return finalized,
            Some(Event::BlocksProcessed {
                finalized: batch, ..
            }) => finalized.extend(batch),
            Some(Event::TurnNotification { .. }) => {}
            None => {
                idle_rounds += 1;
                tokio::time::advance(RECONNECT_DELAY).await;
                tokio::task::yield_now().await;
            }
        }
    }
    panic!("indexer replay did not reach Ready");
}

/// A passive policy boxed for [`Harness::start`].
#[must_use]
pub fn passive() -> Box<dyn Policy> {
    Box::new(PassivePolicy)
}
