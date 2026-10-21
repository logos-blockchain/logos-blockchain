//! Scenario world: a funded simulated node, a channel owned by `SEQ_A`'s key,
//! three named sequencer keys and a network behaviour applied on every step.

use std::{fmt::Write as _, time::Duration};

use lb_core::{
    header::HeaderId,
    mantle::{
        Note, NoteId, SignedOps, Utxo,
        ledger::{Outputs, verification_mode::StandardMode},
        ops::channel::{ChannelId, VerifiedChannelKeys, deposit::DepositOp, inscribe::Inscription},
        traits::Hashable,
        transactions::states::Unverified,
    },
};
use lb_key_management_system_service::keys::{Ed25519Key, ZkKey, ZkPublicKey};
use lb_zone_sdk::{
    node_types::TxHash,
    sequencer::{
        ChannelWalletView, FinalizedOp, FinalizedTx, PublishResult, SequencerChannelView,
        SequencerCheckpoint, SequencerConfig, WithdrawArg, WithdrawInputs, ZoneSequencer,
    },
};
use num_bigint::BigUint;

pub use crate::harness::passive;
use crate::{
    Harness, SimNode,
    chains::{Chain, Condition, Expect, Move, Shape},
    consumer::ReferenceConsumer,
    harness::{self, RECONNECT_DELAY, funding_config, key},
    node::{BlockSpec, TxSelection},
    policy::{Policy, make_inscription, payload_str},
    txs::{self, TxError},
    writers::{self, Writer, WriterOp},
};

pub const FINALITY_DEPTH: u64 = 3;

/// A submitted deposit: its tx hash, the op and the channel notes it creates.
pub type Deposit = (TxHash, DepositOp, Vec<Utxo>);

/// Fee notes minted for the user wallet; larger than any deposited note.
pub const USER_FEE_NOTES: usize = 8;
pub const USER_FEE_NOTE_VALUE: u64 = 1_000_000;

pub struct World {
    pub harness: Harness,
    /// The second node of the two-node chain.
    pub peer: Option<SimNode>,
    pub chain: Chain,
    pub user_key: ZkKey,
    pub user_utxos: Vec<Utxo>,
    /// Transactions the user submitted; re-submitted while waiting for one
    /// the node evicted, like a user retrying.
    user_txs: Vec<SignedOps<Unverified, StandardMode>>,
    /// Accredited sequencers no [`Harness`] runs; see
    /// [`Self::enable_foreign_traffic`].
    pub writers: Vec<Writer>,
    /// Transactions the writers may still put into blocks.
    foreign_budget: usize,
    /// The accredited list of the last config this world submitted; what a
    /// writer's config rotates.
    pub channel_keys: Option<VerifiedChannelKeys>,
    /// The tip of the last fork that did not win, with the canonical block
    /// it left from; [`Self::revive`] extends it.
    pub dormant: Option<Dormant>,
    steps: usize,
}

/// A branch that lost or never caught up.
#[derive(Clone, Copy, Debug)]
pub struct Dormant {
    pub tip: HeaderId,
    pub fork_point: HeaderId,
}

/// Which posted (mempool) transactions a block takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Posted {
    /// Every valid one, in posting order.
    All,
    /// None.
    None,
    /// The first half, in posting order, like a leader short of space.
    Half,
    /// All of them, in reverse order, like a leader ordering differently.
    Reversed,
}

/// The first writer's name; a second one is [`FOREIGN_2`].
pub const FOREIGN: &str = "FOREIGN";
pub const FOREIGN_2: &str = "FOREIGN_2";

#[must_use]
pub fn seq_key(name: &str) -> Ed25519Key {
    match name {
        "SEQ_A" => key(1),
        "SEQ_B" => key(2),
        "SEQ_C" => key(3),
        FOREIGN => key(4),
        FOREIGN_2 => key(5),
        other => panic!("unknown sequencer {other}"),
    }
}

/// The reference consumer for sequencer `name`, with nothing planned.
#[must_use]
pub fn reference(name: &str) -> Box<dyn Policy> {
    ReferenceConsumer::boxed(&seq_key(name), Vec::new())
}

impl World {
    /// A node with a funded funding wallet and one mined block, no sequencers.
    #[must_use]
    pub fn new(chain: Chain) -> Self {
        Self::with_funding(chain, 30, 1000)
    }

    #[must_use]
    pub fn with_funding(chain: Chain, notes: usize, value: u64) -> Self {
        Self::with_funding_values(chain, &vec![value; notes])
    }

    /// A node whose funding wallet holds exactly these notes.
    #[must_use]
    pub fn with_funding_values(chain: Chain, values: &[u64]) -> Self {
        harness::init_tracing();
        let node = SimNode::new(FINALITY_DEPTH);
        let funding_key = ZkKey::from(BigUint::from(7u64));
        node.set_funding_wallet(funding_key.to_public_key());
        for &value in values {
            let _ = node.mint(value, funding_key.to_public_key());
        }
        // The user wallet pays its own deposit fees from notes larger than
        // any note a scenario deposits, so funding never takes those.
        let user_key = ZkKey::from(BigUint::from(9u64));
        for _ in 0..USER_FEE_NOTES {
            let _ = node.mint(USER_FEE_NOTE_VALUE, user_key.to_public_key());
        }
        let _ = node.mine();
        let peer = chain.is_partitions().then(|| {
            let peer = SimNode::new(FINALITY_DEPTH);
            let _ = peer.import_from(&node);
            let peer_key = ZkKey::from(BigUint::from(8u64));
            peer.set_funding_wallet(peer_key.to_public_key());
            for &value in values {
                let _ = peer.mint(value, peer_key.to_public_key());
            }
            let _ = peer.mine();
            let _ = node.import_from(&peer);
            peer
        });
        let channel_id = ChannelId::from(seq_key("SEQ_A").public_key().to_bytes());
        Self {
            harness: Harness::new(node, channel_id),
            peer,
            chain,
            user_key,
            user_utxos: Vec::new(),
            user_txs: Vec::new(),
            writers: Vec::new(),
            foreign_budget: 0,
            channel_keys: None,
            dormant: None,
            steps: 0,
        }
    }

    #[must_use]
    pub const fn node(&self) -> &SimNode {
        &self.harness.node
    }

    #[must_use]
    pub const fn channel_id(&self) -> ChannelId {
        self.harness.channel_id
    }

    #[must_use]
    pub fn funding_pk(&self) -> ZkPublicKey {
        self.node().funding_pk().expect("funding wallet set")
    }

    #[must_use]
    pub fn user_pk(&self) -> ZkPublicKey {
        self.user_key.to_public_key()
    }

    /// Give the user wallet notes of `values`; mined in the next step.
    pub async fn coin_split(&mut self, values: &[u64]) {
        for &value in values {
            let utxo = self.node().mint(value, self.user_pk());
            self.user_utxos.push(utxo);
        }
        self.harness.mine().await;
    }

    #[must_use]
    pub fn default_config(&self) -> SequencerConfig {
        harness::sequencer_config(funding_config(self.node()))
    }

    #[must_use]
    pub fn config_with_depth(&self, depth: usize) -> SequencerConfig {
        SequencerConfig {
            max_pending_publish_depth: depth,
            ..self.default_config()
        }
    }

    /// The node the next started sequencer talks to: the peer for every
    /// other sequencer on the two-node chain.
    #[must_use]
    pub fn next_node(&self) -> SimNode {
        match &self.peer {
            Some(peer) if self.harness.seqs.len() % 2 == 1 => peer.clone(),
            _ => self.node().clone(),
        }
    }

    /// `config` paying from `node`'s funding wallet.
    fn config_on(node: &SimNode, config: &SequencerConfig) -> SequencerConfig {
        SequencerConfig {
            funding: funding_config(node),
            ..config.clone()
        }
    }

    pub async fn start(&mut self, name: &str, policy: Box<dyn Policy>) -> usize {
        let config = self.default_config();
        self.start_as_with(name, seq_key(name), config, policy)
            .await
    }

    pub async fn start_with(
        &mut self,
        name: &str,
        config: SequencerConfig,
        policy: Box<dyn Policy>,
    ) -> usize {
        self.start_as_with(name, seq_key(name), config, policy)
            .await
    }

    /// Start `name` with a policy built from the live sequencer (for policies
    /// that subscribe to its watches).
    pub async fn start_then(
        &mut self,
        name: &str,
        key: Ed25519Key,
        config: SequencerConfig,
        make_policy: impl FnOnce(&ZoneSequencer<SimNode>, &SimNode) -> Box<dyn Policy>,
    ) -> usize {
        let node = self.next_node();
        let config = Self::config_on(&node, &config);
        let index = self
            .harness
            .add_on(&node, name, key, config, None, passive());
        let policy = make_policy(self.harness.seq(index), &node);
        self.harness.set_policy(index, policy);
        self.harness.settle().await;
        assert!(
            self.harness.seq(index).is_ready(),
            "{name} did not become ready"
        );
        index
    }

    /// Start `name` signing with another sequencer's key.
    pub async fn start_as(
        &mut self,
        name: &str,
        key: Ed25519Key,
        policy: Box<dyn Policy>,
    ) -> usize {
        let config = self.default_config();
        self.start_as_with(name, key, config, policy).await
    }

    pub async fn start_as_with(
        &mut self,
        name: &str,
        key: Ed25519Key,
        config: SequencerConfig,
        policy: Box<dyn Policy>,
    ) -> usize {
        let node = self.next_node();
        let config = Self::config_on(&node, &config);
        self.harness
            .start_with_on(node, name, key, config, None, policy)
            .await
    }

    pub fn stop(&mut self, index: usize) {
        self.harness.stop(index);
    }

    pub async fn restart(&mut self, index: usize, checkpoint: Option<SequencerCheckpoint>) {
        self.harness.restart(index, checkpoint).await;
    }

    #[must_use]
    pub fn checkpoint(&self, index: usize) -> SequencerCheckpoint {
        self.harness.seqs[index]
            .checkpoint()
            .expect("sequencer has a checkpoint")
    }

    pub fn view(&mut self, index: usize) -> SequencerChannelView {
        self.harness
            .seq(index)
            .subscribe_channel_view()
            .borrow()
            .clone()
    }

    pub fn wallet(&mut self, index: usize) -> ChannelWalletView {
        self.harness.seq(index).channel_wallet()
    }

    pub fn pending_count(&mut self, index: usize) -> usize {
        self.view(index).pending_publish_txs
    }

    pub fn our_turn(&mut self, index: usize) -> bool {
        let view = self.view(index);
        view.our_turn_to_write
            && view.authorized_key_index.is_some()
            && view.authorized_key_index == view.own_key_index
    }

    /// Step until it is `index`'s turn to write.
    pub async fn wait_turn(&mut self, index: usize) {
        let done = self.step_until(40, |w| w.our_turn(index)).await;
        let name = self.harness.seqs[index].name.clone();
        assert!(done, "{name} never got the turn:\n{}", self.diagnostics());
    }

    /// Step until `index` is not in turn.
    pub async fn wait_not_turn(&mut self, index: usize) {
        assert!(
            self.step_until(40, |w| !w.our_turn(index)).await,
            "{} never lost the turn",
            self.harness.seqs[index].name
        );
    }

    /// Submit a channel config from `index`: keys are the submitter followed
    /// by `names`, de-duplicated.
    pub async fn submit_config(
        &mut self,
        index: usize,
        names: &[&str],
        timeframe: u32,
        timeout: u32,
    ) -> TxHash {
        let submitter = self.harness.seqs[index].key.public_key();
        let mut keys = vec![submitter];
        for name in names {
            let key = seq_key(name).public_key();
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        let keys = VerifiedChannelKeys::try_from(keys).expect("keys");
        self.channel_keys = Some(keys.clone());
        let ((result, _), _) = self
            .harness
            .seq(index)
            .handle()
            .channel_config(keys, timeframe.into(), timeout.into(), 1, 1)
            .await
            .expect("config accepted");
        self.harness.settle().await;
        result.inscription_id()
    }

    /// Submit a config and step until it is finalized.
    pub async fn config_finalized(
        &mut self,
        index: usize,
        names: &[&str],
        timeframe: u32,
        timeout: u32,
    ) -> TxHash {
        let hash = self.submit_config(index, names, timeframe, timeout).await;
        self.include(hash).await;
        self.finalize_tx(hash).await;
        hash
    }

    /// Add an accredited writer named `name` (its key is [`seq_key`]'s)
    /// whose transactions appear in the blocks the network mines during its
    /// turns, up to the traffic budget. Like a sequencer, it puts an orphaned
    /// transaction of its own back into a block of its turn while the chain
    /// would still accept it. Its transactions enter blocks directly, never
    /// the mempool, so the sequencers under test only ever see them mined, in
    /// plain blocks, fork blocks and both partition nodes alike.
    pub fn add_writer(&mut self, name: &str) {
        self.add_writer_with(name, &writers::OP_CYCLE);
    }

    /// [`Self::add_writer`] with the writer cycling through `ops` only.
    pub fn add_writer_with(&mut self, name: &str, ops: &'static [WriterOp]) {
        let recipient = ZkKey::from(BigUint::from(20 + self.writers.len() as u64)).to_public_key();
        let mut writer = Writer::new(name, seq_key(name), recipient);
        writer.ops = ops;
        self.writers.push(writer);
    }

    /// Let the writers put up to `posts` new transactions into blocks; adds
    /// the [`FOREIGN`] writer when there is none.
    pub fn enable_foreign_traffic(&mut self, posts: usize) {
        if self.writers.is_empty() {
            self.add_writer(FOREIGN);
        }
        self.foreign_budget = posts;
    }

    /// The writers' transactions for a block on `parent` at `slot`: the one
    /// in turn re-lands an orphan or, with budget left, builds its next op.
    async fn foreign_txs(
        &mut self,
        node: &SimNode,
        parent: HeaderId,
        slot: u64,
    ) -> Vec<SignedOps<Unverified, StandardMode>> {
        if self.writers.is_empty() {
            return Vec::new();
        }
        let channel_id = self.channel_id();
        let Some(ledger) = node.block(parent).map(|block| block.ledger) else {
            return Vec::new();
        };
        let Some(state) = ledger.channel(&channel_id).cloned() else {
            return Vec::new();
        };
        let Some(funding_pk) = node.funding_pk() else {
            return Vec::new();
        };
        let Some(keys) = self.channel_keys.clone() else {
            return Vec::new();
        };
        let prices = node.gas_prices();
        let budget = &mut self.foreign_budget;
        for writer in &mut self.writers {
            if !writer.in_turn(&state, slot.into()) {
                continue;
            }
            if let Some(orphan) = writer.reland(&ledger, &prices, slot.into(), |tx| {
                node.is_included_at(parent, &tx.hash())
            }) {
                return vec![orphan];
            }
            let mut priced = ledger.clone();
            priced.set_gas_prices(prices.clone());
            if let Some(rebuilt) = writer
                .rebuild(node, &priced, channel_id, funding_pk, slot.into(), |tx| {
                    node.is_included_at(parent, &tx.hash())
                })
                .await
            {
                return vec![rebuilt];
            }
            if *budget == 0 {
                return Vec::new();
            }
            match writer
                .build(node, &ledger, channel_id, funding_pk, &keys)
                .await
            {
                Ok(tx) => {
                    *budget -= 1;
                    return vec![tx];
                }
                Err(error) => {
                    tracing::warn!(writer = %writer.name, %error, "writer could not build its transaction");
                    return Vec::new();
                }
            }
        }
        Vec::new()
    }

    /// Mine one block on `node`'s tip with the mempool and the writers'
    /// traffic, then settle.
    async fn mine_on(&mut self, node: &SimNode) {
        let parent = node.tip();
        let slot = Self::tip_slot_after(node, 1);
        let _ = self
            .mine_block_on(node, parent, slot, Posted::All, true)
            .await;
    }

    /// The slot of the next block on `node`'s tip, `gap` slots after it.
    fn tip_slot_after(node: &SimNode, gap: u64) -> u64 {
        node.tip_slot().into_inner() + gap
    }

    fn selection(node: &SimNode, posted: Posted) -> TxSelection {
        let hashes: Vec<TxHash> = node.mempool().iter().map(Hashable::hash).collect();
        match posted {
            Posted::All => TxSelection::Mempool,
            Posted::None => TxSelection::None,
            Posted::Half => {
                let half = hashes.len().div_ceil(2);
                TxSelection::Only(hashes.into_iter().take(half).collect())
            }
            Posted::Reversed => TxSelection::Ordered(hashes.into_iter().rev().collect()),
        }
    }

    /// Mine one block on `parent` at `slot` with the chosen posted
    /// transactions and the writers' traffic, announced or not, then settle.
    pub async fn mine_block_on(
        &mut self,
        node: &SimNode,
        parent: HeaderId,
        slot: u64,
        posted: Posted,
        announce: bool,
    ) -> HeaderId {
        let extra_txs = self.foreign_txs(node, parent, slot).await;
        let id = node.mine_block(BlockSpec {
            parent: Some(parent),
            slot: Some(slot.into()),
            txs: Self::selection(node, posted),
            extra_txs,
            force_canonical: false,
            announce,
        });
        tracing::debug!(
            block = ?id,
            ?parent,
            slot,
            announce,
            txs = ?node.block(id).map(|b| b.txs.iter().map(Hashable::hash).collect::<Vec<_>>()),
            tip = ?node.tip(),
            lib = ?node.lib(),
            "mined"
        );
        self.harness.settle().await;
        id
    }

    /// One block on the primary's tip, `gap` slots later (`gap > 1` leaves
    /// empty slots), taking `posted`.
    pub async fn extend(&mut self, gap: u64, posted: Posted) -> HeaderId {
        let node = self.node().clone();
        let parent = node.tip();
        let slot = Self::tip_slot_after(&node, gap);
        self.mine_block_on(&node, parent, slot, posted, true).await
    }

    /// Mine `silent` blocks the node never announces, then one it does: the
    /// stream a lagging subscriber sees, whose next event's parent it has
    /// never heard of.
    pub async fn lagging(&mut self, silent: usize, posted: Posted) {
        let node = self.node().clone();
        for _ in 0..silent {
            let parent = node.tip();
            let slot = Self::tip_slot_after(&node, 1);
            let _ = self.mine_block_on(&node, parent, slot, posted, false).await;
        }
        self.extend(1, posted).await;
    }

    /// Move the tip silently and announce a competing block on the old tip
    /// instead: an event whose block is not the tip it names, so the SDK
    /// has to backfill the tip's branch.
    pub async fn split_tip(&mut self, posted: Posted) {
        let node = self.node().clone();
        let old_tip = node.tip();
        let slot = Self::tip_slot_after(&node, 1);
        let _ = self
            .mine_block_on(&node, old_tip, slot, posted, false)
            .await;
        let fork_point = old_tip;
        let tip = self
            .mine_block_on(&node, old_tip, slot, Posted::None, true)
            .await;
        self.dormant = Some(Dormant { tip, fork_point });
    }

    /// Whether every node could switch to a branch leaving the canonical
    /// chain at `fork_point`: each knows the block and has not finalized
    /// anything above it. A node never reorgs below its LIB, so a branch
    /// one of them cannot follow would split the network for good.
    #[must_use]
    pub fn reorg_allowed(&self, fork_point: HeaderId) -> bool {
        std::iter::once(self.node())
            .chain(self.peer.as_ref())
            .all(|node| {
                node.block(fork_point).is_some()
                    && (!node.is_finalized(fork_point) || fork_point == node.lib())
            })
    }

    /// Branch off `depth` blocks below the tip with `length` blocks, the
    /// last one taking `posted`; longer than `depth` it wins, equal it ties
    /// and loses, shorter it stays dormant. Extends the tip instead when a
    /// node could not follow the branch. Fork blocks take the slots of
    /// the blocks they replace, so a fork moves the slot grid by at most one
    /// like a plain block: a fork that skipped slots would recur in a fixed
    /// phase of the posting turns and could orphan one sequencer's window on
    /// every cycle. Returns the branch tip.
    pub async fn fork_with(&mut self, depth: usize, length: usize, posted: Posted) -> HeaderId {
        let node = self.node().clone();
        let fork_point = node.ancestor_of_tip(depth);
        if !self.reorg_allowed(fork_point) {
            return self.extend(1, posted).await;
        }
        let mut parent = fork_point;
        let parent_slot = node.block(parent).expect("fork parent").slot.into_inner();
        for i in 0..length {
            let slot = parent_slot + 1 + i as u64;
            let last = i + 1 == length;
            parent = self
                .mine_block_on(
                    &node,
                    parent,
                    slot,
                    if last { posted } else { Posted::None },
                    true,
                )
                .await;
        }
        if length <= depth {
            self.dormant = Some(Dormant {
                tip: parent,
                fork_point,
            });
        }
        parent
    }

    /// Orphan the top `depth` blocks with a fork of `depth + 1` blocks from
    /// their parent; the last one takes the mempool.
    pub async fn fork(&mut self, depth: usize) {
        let _ = self.fork_with(depth, depth + 1, Posted::All).await;
    }

    /// Extend the dormant branch by `blocks`; it may overtake the tip and
    /// reorg the chain long after it forked. Nothing happens without a
    /// dormant branch, when the node pruned it, or when the reorg would
    /// reach below LIB.
    pub async fn revive(&mut self, blocks: usize, posted: Posted) -> bool {
        let node = self.node().clone();
        let Some(Dormant { tip, .. }) = self.dormant else {
            return false;
        };
        let Some(branch_tip) = node.block(tip) else {
            self.dormant = None;
            return false;
        };
        // Later reorgs may have moved where the branch leaves today's chain.
        let Some(fork_point) = node.fork_point_of(tip) else {
            self.dormant = None;
            return false;
        };
        if !self.reorg_allowed(fork_point) {
            self.dormant = None;
            return false;
        }
        let mut parent = tip;
        let mut slot = branch_tip.slot.into_inner();
        for i in 0..blocks {
            slot += 1;
            let last = i + 1 == blocks;
            parent = self
                .mine_block_on(
                    &node,
                    parent,
                    slot,
                    if last { posted } else { Posted::None },
                    true,
                )
                .await;
        }
        if node.tip() == parent {
            self.dormant = None;
        } else {
            self.dormant = Some(Dormant {
                tip: parent,
                fork_point,
            });
        }
        true
    }

    /// Wall-clock time one step takes. Not a divisor of the SDK's timers so
    /// their phase drifts against the slot grid like on a live node; an exact
    /// multiple phase-locks a timer that only acts inside a turn window.
    pub const STEP_DURATION: Duration = Duration::from_millis(1100);

    /// Advance the chain by one step of the catalogue entry. Each step also
    /// advances the paused clock by [`Self::STEP_DURATION`] so the SDK's
    /// periodic timers fire at the cadence they would on a live node.
    pub async fn step(&mut self) {
        let n = self.steps;
        self.steps += 1;
        self.harness.advance(Self::STEP_DURATION).await;
        match self.chain.step(n) {
            Some(mv) => self.apply(mv).await,
            None => self.partition_step().await,
        }
    }

    /// Whether the two nodes of the two-node chain are currently split:
    /// extra blocks mined now would widen the divergence past what the
    /// merge can reorg.
    #[must_use]
    pub const fn partitioned(&self) -> bool {
        self.peer.is_some() && matches!(self.steps % 11, 4 | 5)
    }

    /// One step of the two-node chain: gossip and mine on one node, the
    /// other imports; during the split both mine alone and the peer gets one
    /// block ahead, so the merge reorgs the primary.
    pub(crate) async fn partition_step(&mut self) {
        let primary = self.node().clone();
        let peer = self.peer.clone().expect("partition network has a peer");
        match self.steps % 11 {
            4 | 5 => {
                self.mine_on(&primary).await;
                self.mine_on(&peer).await;
                if self.steps % 11 == 5 {
                    self.mine_on(&peer).await;
                }
            }
            6 => {
                let _ = primary.import_from(&peer);
                let _ = peer.import_from(&primary);
                primary.gossip_with(&peer);
                self.harness.settle().await;
            }
            phase => {
                primary.gossip_with(&peer);
                if phase % 3 == 0 {
                    self.mine_on(&peer).await;
                    let _ = primary.import_from(&peer);
                } else {
                    self.mine_on(&primary).await;
                    let _ = peer.import_from(&primary);
                }
                self.harness.settle().await;
            }
        }
    }

    pub async fn steps(&mut self, n: usize) {
        for _ in 0..n {
            self.step().await;
        }
    }

    /// Step until `done`, at most `max` times.
    pub async fn step_until(
        &mut self,
        max: usize,
        mut done: impl FnMut(&mut Self) -> bool,
    ) -> bool {
        for _ in 0..max {
            if done(self) {
                return true;
            }
            self.step().await;
        }
        done(self)
    }

    pub async fn include(&mut self, hash: TxHash) {
        self.include_within(hash, 30).await;
    }

    /// Step until `hash` is on the canonical chain, at most `max_steps` times.
    pub async fn include_within(&mut self, hash: TxHash, max_steps: usize) {
        let done = self
            .step_until(max_steps, |w| {
                w.resubmit_user_tx(hash);
                w.node().is_included(&hash)
            })
            .await;
        assert!(done, "{hash:?} not included:\n{}", self.diagnostics());
    }

    /// Whether an inscription carrying `payload` is on the canonical chain:
    /// what a story waits for when a re-fund may change the tx hash.
    #[must_use]
    pub fn is_payload_included(&self, payload: &str) -> bool {
        let channel_id = self.channel_id();
        self.node().canonical_txs().iter().any(|tx| {
            lb_zone_sdk::sequencer::channel_inscriptions(tx, channel_id)
                .iter()
                .any(|info| payload_str(&info.payload) == payload)
        })
    }

    /// The inscription payloads sequencer `index` still has pending.
    #[must_use]
    pub fn pending_payloads(&self, index: usize) -> Vec<String> {
        let channel_id = self.channel_id();
        self.harness.seqs[index]
            .checkpoint()
            .map(|cp| {
                cp.pending_txs
                    .iter()
                    .flat_map(|(_, tx)| {
                        lb_zone_sdk::sequencer::channel_inscriptions(tx, channel_id)
                            .into_iter()
                            .map(|info| payload_str(&info.payload))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Put a user's transaction the node evicted back into the mempool.
    fn resubmit_user_tx(&self, hash: TxHash) {
        let Some(tx) = self.user_txs.iter().find(|tx| tx.hash() == hash) else {
            return;
        };
        let node = self.node();
        if !node.is_included(&hash) && !node.mempool().iter().any(|t| t.hash() == hash) {
            node.submit(tx.clone());
        }
    }

    pub async fn finalize_tx(&mut self, hash: TxHash) {
        let done = self
            .step_until(40, |w| {
                w.resubmit_user_tx(hash);
                w.node().is_tx_finalized(&hash)
            })
            .await;
        assert!(done, "{hash:?} not finalized:\n{}", self.diagnostics());
    }

    /// Step until every hash is finalized.
    pub async fn finalize_all(&mut self, hashes: &[TxHash]) {
        for hash in hashes {
            self.finalize_tx(*hash).await;
        }
    }

    /// The pending transactions of sequencer `index` that carry an
    /// inscription signed with its own key. Other writers' entries it tracks
    /// for re-posting stay pending until their lineage slot is taken, which
    /// an idle channel never does, so they do not count.
    #[must_use]
    pub fn own_pending(&self, index: usize) -> Vec<TxHash> {
        let seq = &self.harness.seqs[index];
        let own = seq.key.public_key().into_unverified();
        let channel_id = self.channel_id();
        seq.checkpoint()
            .map(|cp| {
                cp.pending_txs
                    .iter()
                    .filter(|(_, tx)| {
                        lb_zone_sdk::sequencer::channel_inscriptions(tx, channel_id)
                            .iter()
                            .any(|info| info.signer == Some(own))
                    })
                    .map(|(hash, _)| *hash)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Step until the chain is quiet: nothing in the mempool, nothing of its
    /// own pending in any sequencer and every canonical transaction
    /// finalized. Bounded so a stuck scenario fails instead of looping.
    pub async fn settle_chain(&mut self, max_steps: usize) {
        let done = self
            .step_until(max_steps, |w| {
                w.node().mempool().is_empty()
                    && w.node().unfinalized_txs().is_empty()
                    && w.peer
                        .as_ref()
                        .is_none_or(|p| p.mempool().is_empty() && p.tip() == w.node().tip())
                    && (0..w.harness.seqs.len()).all(|index| w.own_pending(index).is_empty())
            })
            .await;
        assert!(
            done,
            "chain did not settle within {max_steps} steps:\n{}",
            self.diagnostics()
        );
    }

    /// Where every transaction is, for a failing assertion.
    pub fn diagnostics(&mut self) -> String {
        let mut out = String::new();
        let node = self.node().clone();
        let _ = writeln!(
            out,
            "tip slot {:?}, lib {:?}, mempool {}",
            node.tip_slot(),
            node.lib(),
            node.mempool().len()
        );
        if let Some(state) = node.channel_state_at_tip(self.channel_id()) {
            let _ = writeln!(out, "channel: {state:?}");
        }
        if let Some(peer) = &self.peer {
            let _ = writeln!(
                out,
                "peer tip slot {:?} (same tip: {}), mempool {}",
                peer.tip_slot(),
                peer.tip() == node.tip(),
                peer.mempool().len()
            );
        }
        for writer in &self.writers {
            let included = writer
                .posted_hashes()
                .filter(|hash| node.is_included(hash))
                .count();
            let _ = writeln!(
                out,
                "{}: {} posted, {included} canonical, {} budget left",
                writer.name,
                writer.posted.len(),
                self.foreign_budget
            );
        }
        for rejection in node.rejected().iter().rev().take(10) {
            let _ = writeln!(
                out,
                "rejected {:?}: {}",
                rejection.tx_hash, rejection.reason
            );
        }
        let channel_id = self.channel_id();
        for index in 0..self.harness.seqs.len() {
            let name = self.harness.seqs[index].name.clone();
            if !self.harness.seqs[index].is_running() {
                let _ = writeln!(out, "{name}: stopped");
                continue;
            }
            let view = self.view(index);
            let checkpoint = self.checkpoint(index);
            let pending: Vec<String> = checkpoint
                .pending_txs
                .iter()
                .map(|(hash, tx)| {
                    let payloads: Vec<String> =
                        lb_zone_sdk::sequencer::channel_inscriptions(tx, channel_id)
                            .into_iter()
                            .map(|i| payload_str(&i.payload))
                            .collect();
                    format!("{hash:?} {payloads:?} included={}", node.is_included(hash))
                })
                .collect();
            let _ = writeln!(
                out,
                "{name}: own {:?} authorized {:?} turn {} pending {}\n  {}",
                view.own_key_index,
                view.authorized_key_index,
                view.our_turn_to_write,
                view.pending_publish_txs,
                pending.join("\n  ")
            );
        }
        out
    }

    /// Publish via the drive-loop handle of sequencer `index`.
    pub async fn publish(
        &mut self,
        index: usize,
        payload: &str,
    ) -> Result<PublishResult, lb_zone_sdk::sequencer::Error> {
        let result = self
            .harness
            .seq(index)
            .handle()
            .publish(make_inscription(payload))
            .await
            .map(|(result, _)| result);
        if let Ok(result) = &result {
            self.harness
                .published(index, result.tx.inscription().this_msg);
        }
        self.harness.settle().await;
        result
    }

    /// Publish and step until the inscription is on chain.
    pub async fn publish_included(&mut self, index: usize, payload: &str) -> TxHash {
        let result = self
            .publish(index, payload)
            .await
            .expect("publish accepted");
        let hash = result.inscription_id();
        self.include(hash).await;
        hash
    }

    /// Submit a user deposit of `utxos`; returns the tx hash, the op and the
    /// re-created channel notes.
    pub async fn deposit(&mut self, utxos: &[Utxo], metadata: &str) -> Deposit {
        self.try_deposit(utxos, metadata)
            .await
            .expect("deposit funded")
    }

    /// [`Self::deposit`], failing when the user wallet cannot fund it.
    pub async fn try_deposit(
        &mut self,
        utxos: &[Utxo],
        metadata: &str,
    ) -> Result<Deposit, TxError> {
        let (tx, op) = txs::deposit_tx(
            self.node(),
            self.channel_id(),
            utxos,
            metadata,
            self.user_pk(),
        )
        .await?;
        let hash = Hashable::hash(&tx);
        let notes = txs::recreated_channel_notes(&op, utxos);
        self.user_txs.push(tx.clone());
        self.node().submit(tx);
        self.harness.settle().await;
        Ok((hash, op, notes))
    }

    /// Set the gas prices on every node, in force from the next block.
    pub fn set_gas_prices(&self, price: u64) {
        let prices = lb_core::mantle::transactions::GasPrices::new(price, price);
        self.node().set_gas_prices(prices.clone());
        if let Some(peer) = &self.peer {
            peer.set_gas_prices(prices);
        }
    }

    /// Finalized channel notes of sequencer `index` covering `amount`,
    /// largest first; `None` when they do not.
    #[must_use]
    pub fn finalized_inputs(&mut self, index: usize, amount: u64) -> Option<Vec<NoteId>> {
        let mut notes = self.wallet(index).finalized;
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

    /// Sequencer `index` withdraws `amount` to the user under `payload`,
    /// spending finalized channel notes, or any the SDK picks when
    /// `unfinalized` is allowed. `None` when it lacks the funds or the node
    /// refused the transaction.
    pub async fn withdraw(
        &mut self,
        index: usize,
        amount: u64,
        unfinalized: bool,
        payload: &str,
    ) -> Option<TxHash> {
        let inputs = if unfinalized {
            WithdrawInputs::Auto
        } else {
            WithdrawInputs::Explicit(self.finalized_inputs(index, amount)?)
        };
        let outputs = Outputs::new([Note::new(amount, self.user_pk())]);
        let result = self
            .harness
            .seq(index)
            .handle()
            .publish_atomic_withdraw(
                make_inscription(payload),
                vec![WithdrawArg { outputs }],
                inputs,
            )
            .await
            .ok()
            .map(|(result, _)| result.inscription_id());
        self.harness.settle().await;
        result
    }

    /// Take the user's notes of exactly these values.
    pub fn take_user_notes(&mut self, values: &[u64]) -> Vec<Utxo> {
        values
            .iter()
            .map(|&value| {
                let pos = self
                    .user_utxos
                    .iter()
                    .position(|u| u.note.value == value)
                    .unwrap_or_else(|| panic!("no user note of value {value}"));
                self.user_utxos.remove(pos)
            })
            .collect()
    }

    pub fn indexer_payloads(&self) -> impl Future<Output = Vec<String>> + Send {
        let replay = self.harness.indexer_finalized();
        async move { inscription_payloads(&replay.await) }
    }

    pub fn indexer_finalized(&self) -> impl Future<Output = Vec<FinalizedTx>> + Send {
        self.harness.indexer_finalized()
    }

    /// Finalized ops reported through sequencer `index`'s own events.
    #[must_use]
    pub fn finalized_via_events(&self, index: usize) -> Vec<FinalizedTx> {
        self.harness.seqs[index].finalized().cloned().collect()
    }

    /// Publish with retries across reconnects.
    pub async fn publish_retry(&mut self, index: usize, payload: &str) -> TxHash {
        for _ in 0..20 {
            match self.publish(index, payload).await {
                Ok(result) => return result.inscription_id(),
                Err(_) => self.harness.advance(RECONNECT_DELAY).await,
            }
        }
        panic!("publish of {payload} kept failing:\n{}", self.diagnostics());
    }

    /// Apply one catalogue move: the condition, then the chain's move with
    /// its posted transactions. During a split only the network itself may
    /// mine, so the merge stays within what the primary can reorg.
    pub async fn apply(&mut self, mv: Move) {
        self.apply_condition(mv.condition).await;
        if self.partitioned() {
            self.partition_step().await;
            return;
        }
        self.apply_shape(mv.shape, mv.posted).await;
    }

    pub async fn apply_condition(&mut self, condition: Condition) {
        match condition {
            Condition::Quiet => {}
            Condition::Drop => {
                self.node().drop_streams();
                if let Some(peer) = &self.peer {
                    peer.drop_streams();
                }
                self.harness.settle().await;
                self.harness.advance(RECONNECT_DELAY).await;
            }
            Condition::NodeDown => {
                self.node().node_down();
                self.harness.settle().await;
                self.harness.advance(Self::STEP_DURATION).await;
                self.node().node_up();
                self.harness.advance(RECONNECT_DELAY).await;
            }
            Condition::Restart { index, fresh } => {
                if index < self.harness.seqs.len() && self.harness.seqs[index].is_running() {
                    // A fresh start forgets pending publishes; the zone would
                    // re-plan them, which a chain does not model.
                    let checkpoint = if fresh && self.own_pending(index).is_empty() {
                        None
                    } else {
                        self.harness.seqs[index].checkpoint()
                    };
                    self.restart(index, checkpoint).await;
                }
            }
            Condition::FetchFail { count } => self.node().fail_next_fetches(count),
            Condition::GasPrice { price } => self.set_gas_prices(price),
        }
    }

    pub async fn apply_shape(&mut self, shape: Shape, posted: Posted) {
        let node = self.node().clone();
        match shape {
            Shape::Extend { gap } => {
                let _ = self.extend(gap, posted).await;
            }
            Shape::Fork { depth, length } => {
                if node.canonical_chain().len() <= depth + 1 {
                    let _ = self.extend(1, posted).await;
                } else {
                    let _ = self.fork_with(depth, length, posted).await;
                }
            }
            Shape::Revive { blocks } => {
                if !self.revive(blocks, posted).await {
                    let _ = self.extend(1, posted).await;
                }
            }
            Shape::Lagging { silent } => self.lagging(silent, posted).await,
            Shape::SplitTip => self.split_tip(posted).await,
            Shape::Duplicate => {
                node.reannounce(node.tip());
                self.harness.settle().await;
            }
            // LIB only moves with a block on a live node, so both come with
            // one: the event that carries the new LIB.
            Shape::FreezeLib => {
                node.freeze_lib();
                if let Some(peer) = &self.peer {
                    peer.freeze_lib();
                }
                let _ = self.extend(1, posted).await;
            }
            Shape::ThawLib => {
                node.thaw_lib();
                if let Some(peer) = &self.peer {
                    peer.thaw_lib();
                }
                let _ = self.extend(1, posted).await;
            }
            Shape::Prune => {
                let _ = node.prune_forks();
            }
        }
    }

    /// End-of-story checks: every layer held everywhere and the chain did
    /// what its catalogue entry promises.
    pub fn finish(&self) {
        self.harness.assert_layers();
        if self.steps < self.chain.steps_to_expect() {
            return;
        }
        match self.chain.expect {
            Expect::Nothing => {}
            Expect::Reorg => assert!(self.node().reorgs() > 0, "no reorg happened"),
            Expect::DeepReorg => assert!(
                self.node().deepest_reorg() >= usize::try_from(FINALITY_DEPTH).expect("fits"),
                "no reorg reached the finality depth (deepest {})",
                self.node().deepest_reorg()
            ),
            Expect::Reconnect => {
                assert!(self.node().stream_drops() > 0, "no stream drop happened");
                let reconnected = self.harness.seqs.iter().any(|s| {
                    s.events
                        .iter()
                        .filter(|e| matches!(e, lb_zone_sdk::sequencer::Event::Ready))
                        .count()
                        > 1
                });
                assert!(reconnected, "no sequencer re-emitted Ready after a drop");
            }
            Expect::Merge => {
                let peer = self.peer.as_ref().expect("peer");
                assert!(
                    self.node().deepest_reorg() >= 2,
                    "the primary never reorged onto the peer's branch (deepest {})",
                    self.node().deepest_reorg()
                );
                // Whatever the peer-side sequencers posted must have crossed
                // over to the primary's chain.
                let posted = peer.posted();
                assert!(
                    posted.is_empty()
                        || posted.iter().any(|tx| self.node().is_included(&tx.hash())),
                    "nothing posted through the peer reached the primary's chain"
                );
            }
        }
    }
}

/// Finalized inscription payloads in order.
#[must_use]
pub fn inscription_payloads(finalized: &[FinalizedTx]) -> Vec<String> {
    finalized
        .iter()
        .flat_map(|tx| tx.ops.iter())
        .filter_map(|op| match op {
            FinalizedOp::Inscription(info) => Some(payload_str(&info.payload)),
            _ => None,
        })
        .collect()
}

/// `payloads` filtered to `expected`, de-duplicated by first occurrence.
#[must_use]
pub fn filtered_first_occurrence(payloads: &[String], expected: &[&str]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    payloads
        .iter()
        .filter(|p| expected.contains(&p.as_str()))
        .filter(|p| seen.insert((*p).clone()))
        .cloned()
        .collect()
}

/// `payloads` filtered to `expected`, duplicates kept.
#[must_use]
pub fn filtered(payloads: &[String], expected: &[&str]) -> Vec<String> {
    payloads
        .iter()
        .filter(|p| expected.contains(&p.as_str()))
        .cloned()
        .collect()
}

/// Every expected payload present exactly once, in any order.
pub fn assert_exactly_once_any_order(payloads: &[String], expected: &[&str]) {
    let mut found = filtered(payloads, expected);
    found.sort();
    let mut want = strs(expected);
    want.sort();
    assert_eq!(
        found, want,
        "finalized payloads differ from the expected set"
    );
}

#[must_use]
pub fn strs(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[must_use]
pub fn inscription(payload: &str) -> Inscription {
    make_inscription(payload)
}
