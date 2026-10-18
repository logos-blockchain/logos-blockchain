//! A simulated Logos node: one block tree answering every [`Node`] call.
//!
//! Blocks are appended by the test (the "network schedule"); the node only
//! applies the ledger rules and serves consistent answers. Posted transactions
//! wait in a mempool until a block is mined on the canonical tip.

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering as AtomicOrdering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use futures::{StreamExt as _, channel::mpsc};
use lb_common_http_client::{
    ApiBlock, ApiHeader, BlockInfo, ChainServiceInfo, CryptarchiaInfo, Events, PhaseTag,
    ProcessedBlockEvent, Slot, State, TimeInfo,
};
use lb_core::{
    events::Event as ChainEvent,
    header::{ContentId, HeaderId},
    mantle::{
        Note, NoteId, OpProof, SignedOps, Utxo,
        channel::ChannelState,
        gas::MainnetGasProfile,
        ledger::verification_mode::StandardMode,
        ops::{channel::ChannelId, op_proof::samples::SampleProof as _},
        traits::Hashable as _,
        transactions::{
            GasPrices, MantleTxBuilder, TxBuilderError, states::Unverified,
            tx_list::ops::OpsContext,
        },
    },
    proofs::leader_proof::Groth16LeaderProof,
};
use lb_http_api_common::bodies::wallet::fund::{WalletFundRequestBody, WalletFundResponseBody};
use lb_key_management_system_service::keys::{ZkPublicKey, ZkSignature};
use lb_zone_sdk::{
    adapter::{self, BoxStream},
    node_types::TxHash,
};

use crate::ledger::{Rejection, SimLedger};

/// A slot long enough that wall-clock time never moves the SDK's slot clock
/// during a test: the current slot is always the last tip slot it saw.
pub const SLOT_DURATION_MS: u64 = 60 * 60 * 1000;

/// A mined block and the ledger after it.
#[derive(Debug, Clone)]
pub struct SimBlock {
    pub id: HeaderId,
    pub parent: HeaderId,
    pub slot: Slot,
    pub height: u64,
    pub txs: Vec<SignedOps<Unverified, StandardMode>>,
    pub events: Events,
    pub ledger: SimLedger,
    included: HashSet<TxHash>,
    /// Notes this block minted out of band; replayed when another node
    /// imports the block.
    mints: Vec<(NoteId, Utxo)>,
}

impl SimBlock {
    fn api_block(&self) -> ApiBlock {
        ApiBlock {
            header: ApiHeader {
                id: self.id,
                parent_block: self.parent,
                slot: self.slot,
                body_root: ContentId::from([0; 32]),
                proof_of_leadership: genesis_proof().clone(),
            },
            uncle_headers: Vec::new(),
            transactions: self.txs.clone(),
        }
    }
}

/// Which mempool transactions a mined block takes.
#[derive(Debug, Clone)]
pub enum TxSelection {
    /// Every valid mempool transaction, in posting order.
    Mempool,
    /// No mempool transaction.
    None,
    /// Only the listed hashes, in posting order.
    Only(Vec<TxHash>),
    /// Everything but the listed hashes.
    Except(Vec<TxHash>),
    /// The listed hashes in the listed order, like a leader that orders the
    /// mempool its own way; what is not in the mempool is skipped.
    Ordered(Vec<TxHash>),
}

/// How to mine one block.
#[derive(Debug, Clone)]
pub struct BlockSpec {
    /// Parent block; the canonical tip when `None`.
    pub parent: Option<HeaderId>,
    /// Block slot; the parent's slot plus one when `None`.
    pub slot: Option<Slot>,
    pub txs: TxSelection,
    /// Extra transactions included ahead of the mempool ones, for example a
    /// user's deposit submitted straight to the block producer.
    pub extra_txs: Vec<SignedOps<Unverified, StandardMode>>,
    /// Make the block the tip even when it does not extend the longest chain.
    pub force_canonical: bool,
    /// Stream the block's event to subscribers. A block mined without it is
    /// known to the node but never announced, like an event the node dropped
    /// for a lagging subscriber.
    pub announce: bool,
}

impl Default for BlockSpec {
    fn default() -> Self {
        Self {
            parent: None,
            slot: None,
            txs: TxSelection::Mempool,
            extra_txs: Vec::new(),
            force_canonical: false,
            announce: true,
        }
    }
}

/// The node's funding wallet, which the SDK asks to pay fees.
#[derive(Debug, Clone)]
pub struct FundingWallet {
    pub pk: ZkPublicKey,
    /// Notes handed out to funded transactions that are not yet mined.
    pub reserved: HashSet<NoteId>,
}

/// Distinguishes the blocks and mints of different nodes; genesis is salt 0
/// on every node so nodes share it.
static NEXT_SALT: AtomicU64 = AtomicU64::new(1);

/// The parent id genesis reports; no block ever has it.
fn pre_genesis() -> HeaderId {
    HeaderId::from([0xFF; 32])
}

#[derive(Debug)]
struct ChainState {
    salt: u64,
    blocks: HashMap<HeaderId, SimBlock>,
    genesis: HeaderId,
    tip: HeaderId,
    lib: HeaderId,
    finality_depth: u64,
    mempool: Vec<SignedOps<Unverified, StandardMode>>,
    posted: Vec<SignedOps<Unverified, StandardMode>>,
    rejected: Vec<Rejection>,
    /// The block whose ledger last rejected each tx; gossip does not bring a
    /// tx back while that block is still the tip.
    rejected_at: HashMap<TxHash, HeaderId>,
    /// Every note minted out of band; a block mints what its branch lacks.
    mints: Vec<(NoteId, Utxo)>,
    next_block: u64,
    streams: Vec<mpsc::UnboundedSender<ProcessedBlockEvent>>,
    up: bool,
    /// LIB stays put while frozen (the bootstrap fork-choice period) and
    /// catches up with the tip when thawed.
    lib_frozen: bool,
    /// Block and block-events fetches left to fail.
    fetch_failures: usize,
    genesis_time_unix_ms: i64,
    funding: Option<FundingWallet>,
    gas_prices: GasPrices,
    reorgs: usize,
    deepest_reorg: usize,
    stream_drops: usize,
    pruned: usize,
    /// Per subscriber tag, the tip named by the last event its stream handed
    /// out: what that subscriber has been told about so far.
    pulled: HashMap<u64, HeaderId>,
}

/// What [`ChainState::apply_block`] appends.
struct NewBlock {
    id: HeaderId,
    slot: Slot,
    candidates: Vec<SignedOps<Unverified, StandardMode>>,
    mints: Vec<(NoteId, Utxo)>,
    force_canonical: bool,
    announce: bool,
}

/// Handle to the simulated node; cloning shares the node.
#[derive(Clone, Debug)]
pub struct SimNode {
    state: Arc<Mutex<ChainState>>,
    /// Identifies the subscriber behind this handle's block streams, so a
    /// test can tell what each one has been told; see [`Self::tagged`].
    tag: u64,
}

/// Building the genesis proof decompresses a key; share one copy.
fn genesis_proof() -> &'static Groth16LeaderProof {
    static PROOF: std::sync::LazyLock<Groth16LeaderProof> =
        std::sync::LazyLock::new(Groth16LeaderProof::genesis);
    &PROOF
}

fn block_id(salt: u64, counter: u64) -> HeaderId {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&counter.to_le_bytes());
    bytes[8..16].copy_from_slice(&salt.to_le_bytes());
    HeaderId::from(bytes)
}

fn now_unix_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_millis(),
    )
    .expect("fits")
}

impl Default for SimNode {
    fn default() -> Self {
        Self::new(5)
    }
}

impl SimNode {
    /// A node at genesis whose LIB trails the tip by `finality_depth` blocks.
    #[must_use]
    pub fn new(finality_depth: u64) -> Self {
        let genesis = block_id(0, 0);
        let block = SimBlock {
            id: genesis,
            // Like a real chain, the id before genesis names no block, so a
            // walk back ends with "unknown block" instead of looping.
            parent: pre_genesis(),
            slot: Slot::genesis(),
            height: 0,
            txs: Vec::new(),
            events: Events::default(),
            ledger: SimLedger::default(),
            included: HashSet::new(),
            mints: Vec::new(),
        };
        let state = ChainState {
            salt: NEXT_SALT.fetch_add(1, AtomicOrdering::Relaxed),
            blocks: HashMap::from([(genesis, block)]),
            genesis,
            tip: genesis,
            lib: genesis,
            finality_depth,
            mempool: Vec::new(),
            posted: Vec::new(),
            rejected: Vec::new(),
            rejected_at: HashMap::new(),
            mints: Vec::new(),
            next_block: 1,
            streams: Vec::new(),
            up: true,
            lib_frozen: false,
            fetch_failures: 0,
            genesis_time_unix_ms: now_unix_ms(),
            funding: None,
            gas_prices: GasPrices::default(),
            reorgs: 0,
            deepest_reorg: 0,
            stream_drops: 0,
            pruned: 0,
            pulled: HashMap::new(),
        };
        Self {
            state: Arc::new(Mutex::new(state)),
            tag: 0,
        }
    }

    fn lock(&self) -> MutexGuard<'_, ChainState> {
        self.state.lock().expect("sim node lock")
    }

    /// The same node for a subscriber identified by `tag`.
    #[must_use]
    pub fn tagged(&self, tag: u64) -> Self {
        Self {
            state: Arc::clone(&self.state),
            tag,
        }
    }

    /// The tip named by the last event the streams of this handle's
    /// subscriber delivered; `None` before its first event.
    #[must_use]
    pub fn pulled_tip(&self) -> Option<HeaderId> {
        self.lock().pulled.get(&self.tag).copied()
    }

    /// Whether this handle's subscriber has been told about the current tip.
    #[must_use]
    pub fn subscriber_at_tip(&self) -> bool {
        let state = self.lock();
        state.pulled.get(&self.tag) == Some(&state.tip)
    }

    /// Pay fees from notes owned by `pk`; without this funding is a no-op.
    pub fn set_funding_wallet(&self, pk: ZkPublicKey) {
        self.lock().funding = Some(FundingWallet {
            pk,
            reserved: HashSet::new(),
        });
    }

    #[must_use]
    pub fn funding_pk(&self) -> Option<ZkPublicKey> {
        self.lock().funding.as_ref().map(|w| w.pk)
    }

    /// Gas prices for funding and for every block mined from now on.
    pub fn set_gas_prices(&self, prices: GasPrices) {
        self.lock().gas_prices = prices;
    }

    #[must_use]
    pub fn gas_prices(&self) -> GasPrices {
        self.lock().gas_prices.clone()
    }

    /// Queue a plain note for the next mined block, on every branch; returns
    /// the note as it will exist once that block is mined.
    #[must_use]
    pub fn mint(&self, value: u64, pk: ZkPublicKey) -> Utxo {
        let mut state = self.lock();
        let mut op_id = [0u8; 32];
        op_id[..8].copy_from_slice(&(state.mints.len() as u64).to_le_bytes());
        op_id[8..16].copy_from_slice(&state.salt.to_le_bytes());
        op_id[16] = 0xAA;
        let utxo = Utxo::new(op_id, 0, Note::new(value, pk));
        state.mints.push((utxo.id(), utxo));
        utxo
    }

    /// Mine one block on the canonical tip with every valid mempool tx.
    #[must_use]
    pub fn mine(&self) -> HeaderId {
        self.mine_block(BlockSpec::default())
    }

    /// Mine one block without taking mempool transactions.
    #[must_use]
    pub fn mine_empty(&self) -> HeaderId {
        self.mine_block(BlockSpec {
            txs: TxSelection::None,
            ..BlockSpec::default()
        })
    }

    /// Mine a block including `txs` (plus the mempool) on the canonical tip.
    #[must_use]
    pub fn mine_with(&self, txs: Vec<SignedOps<Unverified, StandardMode>>) -> HeaderId {
        self.mine_block(BlockSpec {
            extra_txs: txs,
            ..BlockSpec::default()
        })
    }

    /// Mine according to `spec`. The block becomes the tip when it extends the
    /// longest chain (or is forced); then LIB follows `finality_depth` behind.
    #[must_use]
    pub fn mine_block(&self, spec: BlockSpec) -> HeaderId {
        let mut state = self.lock();
        let parent_id = spec.parent.unwrap_or(state.tip);
        let parent = state
            .blocks
            .get(&parent_id)
            .expect("parent block exists")
            .clone();
        let slot = spec
            .slot
            .unwrap_or_else(|| Slot::from(parent.slot.into_inner() + 1));
        assert!(slot > parent.slot, "block slot must exceed its parent's");

        let selected: Vec<SignedOps<Unverified, StandardMode>> = match &spec.txs {
            TxSelection::Ordered(order) => order
                .iter()
                .filter_map(|hash| state.mempool.iter().find(|tx| tx.hash() == *hash))
                .cloned()
                .collect(),
            selection => state
                .mempool
                .iter()
                .filter(|tx| {
                    let hash = tx.hash();
                    match selection {
                        TxSelection::Mempool => true,
                        TxSelection::None => false,
                        TxSelection::Only(only) => only.contains(&hash),
                        TxSelection::Except(except) => !except.contains(&hash),
                        TxSelection::Ordered(_) => unreachable!("handled above"),
                    }
                })
                .cloned()
                .collect(),
        };
        let candidates: Vec<SignedOps<Unverified, StandardMode>> =
            spec.extra_txs.into_iter().chain(selected).collect();
        // A block mints every out-of-band note its branch has not seen yet.
        let mints: Vec<(NoteId, Utxo)> = state
            .mints
            .iter()
            .filter(|(id, _)| !parent.ledger.has_minted(id))
            .copied()
            .collect();
        let id = block_id(state.salt, state.next_block);
        state.next_block += 1;
        drop(state.apply_block(
            &parent,
            NewBlock {
                id,
                slot,
                candidates,
                mints,
                force_canonical: spec.force_canonical,
                announce: spec.announce,
            },
        ));
        id
    }

    /// Stream block `id`'s event again, with the current tip and LIB, like a
    /// duplicate delivery.
    pub fn reannounce(&self, id: HeaderId) {
        let mut state = self.lock();
        if !state.up {
            return;
        }
        let event = state.event_for(id);
        state
            .streams
            .retain(|stream| stream.unbounded_send(event.clone()).is_ok());
    }

    /// Stop LIB from advancing, like a node under the bootstrap fork-choice
    /// rule; tip switches may then reorg deeper than the finality depth, but
    /// never below LIB.
    pub fn freeze_lib(&self) {
        self.lock().lib_frozen = true;
    }

    /// Let LIB catch up with the tip at once, like the switch back to the
    /// online rule, and advance normally from now on.
    pub fn thaw_lib(&self) {
        let mut state = self.lock();
        state.lib_frozen = false;
        state.advance_lib();
    }

    #[must_use]
    pub fn is_lib_frozen(&self) -> bool {
        self.lock().lib_frozen
    }

    /// Make the next `count` block or block-events fetches fail.
    pub fn fail_next_fetches(&self, count: usize) {
        self.lock().fetch_failures += count;
    }

    /// Forget every fork whose branch left the canonical chain below LIB;
    /// their blocks can no longer be fetched. Returns how many went.
    #[must_use]
    pub fn prune_forks(&self) -> usize {
        let mut state = self.lock();
        let pruned = state.prune_forks();
        drop(state);
        pruned
    }

    /// Blocks pruned so far.
    #[must_use]
    pub fn pruned(&self) -> usize {
        self.lock().pruned
    }

    /// Adopt every canonical block of `other` this node does not know, in
    /// chain order, as if received from the network. Returns how many.
    #[must_use]
    pub fn import_from(&self, other: &Self) -> usize {
        let their_chain: Vec<HeaderId> = other.canonical_chain();
        let unknown: Vec<HeaderId> = {
            let state = self.lock();
            their_chain
                .into_iter()
                .filter(|id| !state.blocks.contains_key(id))
                .collect()
        };
        let blocks: Vec<SimBlock> = {
            let theirs = other.lock();
            unknown.iter().map(|id| theirs.blocks[id].clone()).collect()
        };
        let mut state = self.lock();
        for block in &blocks {
            state.import_block(block);
        }
        drop(state);
        blocks.len()
    }

    /// Exchange mempools with `other`, like gossip between two nodes. A node
    /// ignores a tx its current tip already rejected.
    pub fn gossip_with(&self, other: &Self) {
        let mine = self.mempool();
        let theirs = other.mempool();
        {
            let mut state = self.lock();
            for tx in theirs {
                state.receive(tx);
            }
        }
        let mut other_state = other.lock();
        for tx in mine {
            other_state.receive(tx);
        }
    }

    /// Mine empty blocks until block `id` is finalized.
    pub fn finalize(&self, id: HeaderId) {
        while !self.is_finalized(id) {
            assert!(self.is_canonical_block(id), "cannot finalize a fork block");
            let _ = self.mine_empty();
        }
    }

    /// Mine empty blocks until LIB reaches the current tip.
    pub fn finalize_tip(&self) {
        let tip = self.tip();
        self.finalize(tip);
    }

    /// Close every open block stream; the node itself stays reachable.
    pub fn drop_streams(&self) {
        let mut state = self.lock();
        state.streams.clear();
        state.stream_drops += 1;
    }

    /// Tip switches that orphaned at least one block.
    #[must_use]
    pub fn reorgs(&self) -> usize {
        self.lock().reorgs
    }

    /// Most blocks orphaned by one tip switch.
    #[must_use]
    pub fn deepest_reorg(&self) -> usize {
        self.lock().deepest_reorg
    }

    /// Times the streams were closed or the node taken down.
    #[must_use]
    pub fn stream_drops(&self) -> usize {
        self.lock().stream_drops
    }

    /// Take the node down: streams end, every call fails, the mempool and
    /// funding reservations are lost.
    pub fn node_down(&self) {
        let mut state = self.lock();
        state.up = false;
        state.streams.clear();
        state.stream_drops += 1;
        state.mempool.clear();
        if let Some(wallet) = state.funding.as_mut() {
            wallet.reserved.clear();
        }
    }

    pub fn node_up(&self) {
        self.lock().up = true;
    }

    #[must_use]
    pub fn is_up(&self) -> bool {
        self.lock().up
    }

    /// Submit a transaction from outside the SDK (a user wallet).
    pub fn submit(&self, tx: SignedOps<Unverified, StandardMode>) {
        self.lock().push_mempool(tx);
    }

    #[must_use]
    pub fn tip(&self) -> HeaderId {
        self.lock().tip
    }

    #[must_use]
    pub fn lib(&self) -> HeaderId {
        self.lock().lib
    }

    #[must_use]
    pub fn tip_slot(&self) -> Slot {
        let state = self.lock();
        state.blocks[&state.tip].slot
    }

    #[must_use]
    pub fn block(&self, id: HeaderId) -> Option<SimBlock> {
        self.lock().blocks.get(&id).cloned()
    }

    /// Canonical chain from genesis to the tip.
    #[must_use]
    pub fn canonical_chain(&self) -> Vec<HeaderId> {
        let state = self.lock();
        let mut chain = state.ancestors(state.tip);
        drop(state);
        chain.reverse();
        chain
    }

    /// The canonical block `depth` blocks below the tip.
    #[must_use]
    pub fn ancestor_of_tip(&self, depth: usize) -> HeaderId {
        let chain = self.canonical_chain();
        chain[chain.len() - 1 - depth]
    }

    #[must_use]
    pub fn is_canonical_block(&self, id: HeaderId) -> bool {
        let state = self.lock();
        state.ancestors(state.tip).contains(&id)
    }

    #[must_use]
    pub fn is_finalized(&self, id: HeaderId) -> bool {
        let state = self.lock();
        state.ancestors(state.lib).contains(&id)
    }

    /// The canonical block carrying `hash`, if any.
    #[must_use]
    pub fn block_of_tx(&self, hash: &TxHash) -> Option<HeaderId> {
        let state = self.lock();
        state
            .ancestors(state.tip)
            .into_iter()
            .find(|id| state.blocks[id].txs.iter().any(|tx| tx.hash() == *hash))
    }

    #[must_use]
    pub fn is_included(&self, hash: &TxHash) -> bool {
        self.block_of_tx(hash).is_some()
    }

    /// The newest canonical ancestor of block `id` (itself when canonical):
    /// where its branch leaves the canonical chain today. `None` for an
    /// unknown or pruned block.
    #[must_use]
    pub fn fork_point_of(&self, id: HeaderId) -> Option<HeaderId> {
        let state = self.lock();
        let canonical: HashSet<HeaderId> = state.ancestors(state.tip).into_iter().collect();
        let mut current = id;
        loop {
            if canonical.contains(&current) {
                return Some(current);
            }
            current = state.blocks.get(&current)?.parent;
        }
    }

    /// Every block carrying `hash`, canonical or not, as (id, height,
    /// canonical).
    #[must_use]
    pub fn blocks_with_tx(&self, hash: &TxHash) -> Vec<(HeaderId, u64, bool)> {
        let state = self.lock();
        let canonical: HashSet<HeaderId> = state.ancestors(state.tip).into_iter().collect();
        let mut found: Vec<(HeaderId, u64, bool)> = state
            .blocks
            .values()
            .filter(|block| block.txs.iter().any(|tx| tx.hash() == *hash))
            .map(|block| (block.id, block.height, canonical.contains(&block.id)))
            .collect();
        drop(state);
        found.sort_by_key(|(id, _, _)| *id);
        found
    }

    /// Whether `hash` is on the branch ending at block `id`.
    #[must_use]
    pub fn is_included_at(&self, id: HeaderId, hash: &TxHash) -> bool {
        self.lock()
            .blocks
            .get(&id)
            .is_some_and(|block| block.included.contains(hash))
    }

    #[must_use]
    pub fn is_tx_finalized(&self, hash: &TxHash) -> bool {
        self.block_of_tx(hash)
            .is_some_and(|block| self.is_finalized(block))
    }

    /// Finalized transactions in blocks above `after` (a finalized block), in
    /// chain order.
    #[must_use]
    pub fn finalized_txs_after(&self, after: HeaderId) -> Vec<SignedOps<Unverified, StandardMode>> {
        let state = self.lock();
        let height = state.blocks.get(&after).map_or(0, |block| block.height);
        let mut chain = state.ancestors(state.lib);
        chain.reverse();
        let txs = chain
            .into_iter()
            .filter(|id| state.blocks[id].height > height)
            .flat_map(|id| state.blocks[&id].txs.clone())
            .collect();
        drop(state);
        txs
    }

    /// Finalized transactions in chain order.
    #[must_use]
    pub fn finalized_txs(&self) -> Vec<SignedOps<Unverified, StandardMode>> {
        let state = self.lock();
        state.txs_of(&state.ancestors(state.lib), &HashSet::new())
    }

    /// Canonical transactions above LIB, in chain order.
    #[must_use]
    pub fn unfinalized_txs(&self) -> Vec<SignedOps<Unverified, StandardMode>> {
        let state = self.lock();
        let finalized: HashSet<HeaderId> = state.ancestors(state.lib).into_iter().collect();
        state.txs_of(&state.ancestors(state.tip), &finalized)
    }

    /// Canonical transactions in chain order.
    #[must_use]
    pub fn canonical_txs(&self) -> Vec<SignedOps<Unverified, StandardMode>> {
        let state = self.lock();
        state.txs_of(&state.ancestors(state.tip), &HashSet::new())
    }

    #[must_use]
    pub fn mempool(&self) -> Vec<SignedOps<Unverified, StandardMode>> {
        self.lock().mempool.clone()
    }

    /// Every transaction ever posted, in order.
    #[must_use]
    pub fn posted(&self) -> Vec<SignedOps<Unverified, StandardMode>> {
        self.lock().posted.clone()
    }

    /// Every transaction a block producer refused, in order.
    #[must_use]
    pub fn rejected(&self) -> Vec<Rejection> {
        self.lock().rejected.clone()
    }

    #[must_use]
    pub fn channel_state_at_tip(&self, channel: ChannelId) -> Option<ChannelState> {
        let state = self.lock();
        state.blocks[&state.tip].ledger.channel(&channel).cloned()
    }

    #[must_use]
    pub fn ledger_at_tip(&self) -> SimLedger {
        let state = self.lock();
        state.blocks[&state.tip].ledger.clone()
    }

    #[must_use]
    pub fn open_streams(&self) -> usize {
        self.lock().streams.len()
    }
}

impl ChainState {
    fn require_up(&self) -> Result<(), lb_common_http_client::Error> {
        if self.up {
            Ok(())
        } else {
            Err(lb_common_http_client::Error::Server(
                "node is down".to_owned(),
            ))
        }
    }

    /// Transactions of `chain` (given newest first) in chain order, skipping
    /// blocks in `skip`.
    fn txs_of(
        &self,
        chain: &[HeaderId],
        skip: &HashSet<HeaderId>,
    ) -> Vec<SignedOps<Unverified, StandardMode>> {
        chain
            .iter()
            .rev()
            .filter(|id| !skip.contains(id))
            .flat_map(|id| self.blocks[id].txs.clone())
            .collect()
    }

    fn snapshot_event(&self) -> ProcessedBlockEvent {
        self.event_for(self.tip)
    }

    /// The stream event for block `id` at the current tip and LIB.
    fn event_for(&self, id: HeaderId) -> ProcessedBlockEvent {
        let block = &self.blocks[&id];
        let tip = &self.blocks[&self.tip];
        let lib = &self.blocks[&self.lib];
        ProcessedBlockEvent {
            block: block.api_block(),
            tip: tip.id,
            tip_slot: tip.slot,
            lib: lib.id,
            lib_slot: lib.slot,
        }
    }

    fn prune_forks(&mut self) -> usize {
        let canonical: HashSet<HeaderId> = self.ancestors(self.tip).into_iter().collect();
        let lib_height = self.blocks[&self.lib].height;
        let mut prunable = Vec::new();
        for id in self.blocks.keys().copied() {
            if canonical.contains(&id) {
                continue;
            }
            let mut fork_point = id;
            while !canonical.contains(&fork_point) {
                fork_point = self.blocks[&fork_point].parent;
            }
            if self.blocks[&fork_point].height < lib_height {
                prunable.push(id);
            }
        }
        for id in &prunable {
            self.blocks.remove(id);
        }
        self.pruned += prunable.len();
        prunable.len()
    }

    fn take_fetch_failure(&mut self) -> Result<(), lb_common_http_client::Error> {
        if self.fetch_failures == 0 {
            return Ok(());
        }
        self.fetch_failures -= 1;
        Err(lb_common_http_client::Error::Server(
            "fetch failed".to_owned(),
        ))
    }

    /// Ancestors of `id` from `id` down to genesis.
    fn ancestors(&self, id: HeaderId) -> Vec<HeaderId> {
        let mut chain = vec![id];
        let mut current = id;
        while current != self.genesis {
            current = self.blocks[&current].parent;
            chain.push(current);
        }
        chain
    }

    /// Append `block` on `parent` with its candidates applied in order and
    /// its mints inserted first; invalid candidates are returned and, when
    /// the parent is the tip, evicted from the mempool like the leader does.
    fn apply_block(&mut self, parent: &SimBlock, block: NewBlock) -> Vec<Rejection> {
        let NewBlock {
            id,
            slot,
            candidates,
            mints,
            force_canonical,
            announce,
        } = block;
        let on_tip = parent.id == self.tip;
        let mut ledger = parent.ledger.clone();
        ledger.set_gas_prices(self.gas_prices.clone());
        let mut included = parent.included.clone();
        for (note_id, utxo) in &mints {
            ledger.mint(*note_id, *utxo);
        }

        let mut txs = Vec::new();
        let mut events = Vec::new();
        let mut rejected = Vec::new();
        for tx in candidates {
            let hash = tx.hash();
            if included.contains(&hash) {
                continue;
            }
            match ledger.apply_tx(&tx, slot) {
                Ok(tx_events) => {
                    included.insert(hash);
                    events.extend(tx_events.into_iter().map(ChainEvent::Tx));
                    txs.push(tx);
                }
                Err(rejection) => rejected.push(rejection),
            }
        }

        let block = SimBlock {
            id,
            parent: parent.id,
            slot,
            height: parent.height + 1,
            txs,
            events: events.into_iter().collect(),
            ledger,
            included,
            mints,
        };
        let height = block.height;
        self.blocks.insert(id, block);

        if on_tip {
            let rejected_hashes: HashSet<TxHash> = rejected.iter().map(|r| r.tx_hash).collect();
            self.mempool
                .retain(|tx| !rejected_hashes.contains(&tx.hash()));
        }
        for rejection in &rejected {
            self.rejected_at.insert(rejection.tx_hash, id);
        }
        self.rejected.extend(rejected.iter().cloned());

        let tip_height = self.blocks[&self.tip].height;
        if force_canonical || height > tip_height {
            self.switch_tip(id);
        }
        self.advance_lib();

        if self.up && announce {
            let event = self.event_for(id);
            self.streams
                .retain(|stream| stream.unbounded_send(event.clone()).is_ok());
        }
        rejected
    }

    /// Adopt `block`, produced by another node; its id and mints are kept so
    /// every node agrees on them.
    fn import_block(&mut self, block: &SimBlock) {
        let parent = self
            .blocks
            .get(&block.parent)
            .expect("imported block's parent is known")
            .clone();
        for mint in &block.mints {
            if !self.mints.iter().any(|(id, _)| *id == mint.0) {
                self.mints.push(*mint);
            }
        }
        let rejected = self.apply_block(
            &parent,
            NewBlock {
                id: block.id,
                slot: block.slot,
                candidates: block.txs.clone(),
                mints: block.mints.clone(),
                force_canonical: false,
                announce: true,
            },
        );
        assert!(
            rejected.is_empty(),
            "imported block {:?} carries invalid transactions: {rejected:?}",
            block.id
        );
    }

    /// Accept a gossiped tx unless the current tip already rejected it.
    fn receive(&mut self, tx: SignedOps<Unverified, StandardMode>) {
        if self.rejected_at.get(&tx.hash()) == Some(&self.tip) {
            return;
        }
        self.push_mempool(tx);
    }

    fn push_mempool(&mut self, tx: SignedOps<Unverified, StandardMode>) {
        let hash = tx.hash();
        if self.blocks[&self.tip].included.contains(&hash)
            || self.mempool.iter().any(|t| t.hash() == hash)
        {
            return;
        }
        self.mempool.push(tx);
    }

    /// Make `id` the tip, returning the dropped branch's transactions to the
    /// mempool and removing what the new branch already carries.
    fn switch_tip(&mut self, id: HeaderId) {
        let old_branch: Vec<HeaderId> = self.ancestors(self.tip);
        let new_branch: HashSet<HeaderId> = self.ancestors(id).into_iter().collect();
        let orphaned = old_branch
            .iter()
            .filter(|b| !new_branch.contains(b))
            .count();
        if orphaned > 0 {
            self.reorgs += 1;
            self.deepest_reorg = self.deepest_reorg.max(orphaned);
        }
        self.tip = id;
        let included = self.blocks[&id].included.clone();
        let mut readmitted = Vec::new();
        for block in old_branch.into_iter().filter(|b| !new_branch.contains(b)) {
            for tx in self.blocks[&block].txs.iter().rev() {
                if !included.contains(&tx.hash()) {
                    readmitted.push(tx.clone());
                }
            }
        }
        readmitted.reverse();
        let mut mempool = std::mem::take(&mut self.mempool);
        mempool.retain(|tx| !included.contains(&tx.hash()));
        self.mempool = readmitted;
        for tx in mempool {
            self.push_mempool(tx);
        }
        if let Some(wallet) = self.funding.as_mut() {
            let ledger = &self.blocks[&id].ledger;
            wallet.reserved.retain(|note| ledger.note(note).is_some());
        }
    }

    fn advance_lib(&mut self) {
        let chain = self.ancestors(self.tip);
        if self.lib_frozen {
            assert!(chain.contains(&self.lib), "reorg below LIB");
            return;
        }
        let depth = usize::try_from(self.finality_depth).expect("fits");
        let Some(candidate) = chain.get(depth) else {
            return;
        };
        let candidate_height = self.blocks[candidate].height;
        let lib_height = self.blocks[&self.lib].height;
        if candidate_height <= lib_height {
            assert!(
                chain.contains(&self.lib),
                "reorg deeper than the finality depth"
            );
            return;
        }
        assert!(
            self.ancestors(*candidate).contains(&self.lib),
            "reorg deeper than the finality depth"
        );
        self.lib = *candidate;
    }

    /// Fund `request` the way the node's wallet service does: largest notes
    /// first, priced with `MainnetGasProfile` at the current gas prices.
    fn fund(
        &mut self,
        request: WalletFundRequestBody,
    ) -> Result<WalletFundResponseBody, lb_common_http_client::Error> {
        let server = |e: String| lb_common_http_client::Error::Server(e);
        let tip = self.tip;
        let Some(wallet) = self.funding.as_mut() else {
            let funded_tx = request
                .tx_builder
                .build()
                .map_err(|e| server(format!("funding build: {e:?}")))?;
            return Ok(WalletFundResponseBody {
                tip,
                funded_tx,
                transfer_proof: None,
            });
        };
        let ledger = &self.blocks[&tip].ledger;
        let context = OpsContext {
            gas_context: ledger.gas_context(),
            leader_reward_amount: 0,
        };
        let consumed: HashSet<NoteId> = request
            .tx_builder
            .notes_consumed_or_used_in_service()
            .collect();
        let mut candidates: Vec<(NoteId, Utxo)> = request
            .funding_public_keys
            .iter()
            .flat_map(|pk| ledger.notes_of(pk))
            .filter(|(id, _)| !wallet.reserved.contains(id) && !consumed.contains(id))
            .collect();
        candidates.sort_by(|a, b| {
            b.1.note
                .value
                .cmp(&a.1.note.value)
                .then_with(|| a.0.as_bytes().cmp(&b.0.as_bytes()))
        });
        let funded = fund_from(
            &request.tx_builder,
            &candidates,
            request.change_public_key,
            &context,
            request.priority_fee_percent,
        )
        .map_err(|e| server(format!("funding: {e}")))?;
        let tx_fee = funded
            .tx_fee()
            .map_err(|e| server(format!("funding fee: {e:?}")))?;
        if tx_fee > request.max_tx_fee {
            return Err(server(format!(
                "tx fee {} exceeds max {}",
                tx_fee.into_inner(),
                request.max_tx_fee.into_inner()
            )));
        }
        let inputs: Vec<NoteId> = funded.ledger_inputs().iter().map(Utxo::id).collect();
        wallet.reserved.extend(inputs.iter().copied());
        let funded_tx = funded
            .build()
            .map_err(|e| server(format!("funding build: {e:?}")))?;
        let transfer_proof = (!inputs.is_empty()).then(|| OpProof::ZkSig(ZkSignature::sample()));
        Ok(WalletFundResponseBody {
            tip,
            funded_tx,
            transfer_proof,
        })
    }
}

/// The node wallet's `fund_tx` loop over `candidates` in spending order: no
/// input when the transaction already pays its fee, otherwise inputs are
/// added until the mandatory fee plus the priority reserve is covered, with
/// change returned whenever the change note pays for itself.
fn fund_from(
    tx_builder: &MantleTxBuilder,
    candidates: &[(NoteId, Utxo)],
    change_pk: ZkPublicKey,
    context: &OpsContext,
    priority_fee_percent: u64,
) -> Result<MantleTxBuilder, String> {
    let builder_err = |e: TxBuilderError| format!("{e:?}");
    let delta = |builder: &MantleTxBuilder| {
        builder
            .funding_delta_with_priority_fee::<MainnetGasProfile>(context, priority_fee_percent)
            .map_err(builder_err)
    };
    let with_change = |builder: MantleTxBuilder| {
        builder
            .return_change::<MainnetGasProfile>(context, change_pk, priority_fee_percent)
            .map_err(builder_err)
    };
    match delta(tx_builder)?.cmp(&0) {
        Ordering::Equal => return Ok(tx_builder.clone()),
        Ordering::Greater => {
            if let Some(funded) = with_change(tx_builder.clone())? {
                return Ok(funded);
            }
        }
        Ordering::Less => {}
    }
    let mut funded = tx_builder.clone();
    for (_, utxo) in candidates {
        funded = funded.add_ledger_input(*utxo).map_err(builder_err)?;
        match delta(&funded)?.cmp(&0) {
            Ordering::Less => {}
            Ordering::Equal => return Ok(funded),
            Ordering::Greater => {
                if let Some(funded) = with_change(funded.clone())? {
                    return Ok(funded);
                }
            }
        }
    }
    Err(format!(
        "insufficient funds: {} available",
        candidates.iter().map(|(_, u)| u.note.value).sum::<u64>()
    ))
}

#[async_trait]
impl adapter::Node for SimNode {
    async fn consensus_info(&self) -> Result<ChainServiceInfo, lb_common_http_client::Error> {
        let state = self.lock();
        state.require_up()?;
        let tip = &state.blocks[&state.tip];
        let lib = &state.blocks[&state.lib];
        let info = CryptarchiaInfo {
            lib: lib.id,
            lib_slot: lib.slot,
            tip: tip.id,
            slot: tip.slot,
            height: tip.height,
            state: State::Online,
        };
        drop(state);
        Ok(ChainServiceInfo {
            cryptarchia_info: info,
            phase: PhaseTag::Following,
        })
    }

    async fn time_info(&self) -> Result<TimeInfo, lb_common_http_client::Error> {
        let state = self.lock();
        state.require_up()?;
        Ok(TimeInfo {
            slot_duration_ms: SLOT_DURATION_MS,
            genesis_time_unix_ms: state.genesis_time_unix_ms,
            current_slot: state.blocks[&state.tip].slot.into_inner(),
            current_epoch: 0,
            slots_per_epoch: 1_000_000,
        })
    }

    async fn channel_state(
        &self,
        channel_id: ChannelId,
    ) -> Result<Option<ChannelState>, lb_common_http_client::Error> {
        let state = self.lock();
        state.require_up()?;
        Ok(state.blocks[&state.tip]
            .ledger
            .channel(&channel_id)
            .cloned())
    }

    async fn block_stream(
        &self,
    ) -> Result<BoxStream<ProcessedBlockEvent>, lb_common_http_client::Error> {
        let mut state = self.lock();
        state.require_up()?;
        let (sender, receiver) = mpsc::unbounded();
        sender
            .unbounded_send(state.snapshot_event())
            .expect("receiver alive");
        state.streams.push(sender);
        drop(state);
        let shared = Arc::clone(&self.state);
        let tag = self.tag;
        Ok(Box::pin(receiver.inspect(move |event| {
            shared
                .lock()
                .expect("sim node lock")
                .pulled
                .insert(tag, event.tip);
        })))
    }

    async fn lib_stream(&self) -> Result<BoxStream<BlockInfo>, lb_common_http_client::Error> {
        self.lock().require_up()?;
        Ok(Box::pin(futures::stream::pending()))
    }

    async fn block(&self, id: HeaderId) -> Result<Option<ApiBlock>, lb_common_http_client::Error> {
        let mut state = self.lock();
        state.require_up()?;
        state.take_fetch_failure()?;
        Ok(state.blocks.get(&id).map(SimBlock::api_block))
    }

    async fn block_events(
        &self,
        id: HeaderId,
    ) -> Result<Option<Events>, lb_common_http_client::Error> {
        let mut state = self.lock();
        state.require_up()?;
        state.take_fetch_failure()?;
        Ok(state.blocks.get(&id).map(|b| b.events.clone()))
    }

    async fn immutable_blocks(
        &self,
        slot_from: Slot,
        slot_to: Slot,
    ) -> Result<Vec<ApiBlock>, lb_common_http_client::Error> {
        let state = self.lock();
        state.require_up()?;
        let lib_slot = state.blocks[&state.lib].slot;
        if slot_from > lib_slot {
            return Ok(Vec::new());
        }
        let slot_to = slot_to.min(lib_slot);
        let mut chain = state.ancestors(state.lib);
        chain.reverse();
        let blocks = chain
            .into_iter()
            .map(|id| &state.blocks[&id])
            .filter(|b| b.slot >= slot_from && b.slot <= slot_to)
            .map(SimBlock::api_block)
            .collect();
        drop(state);
        Ok(blocks)
    }

    async fn post_transaction(
        &self,
        tx: SignedOps<Unverified, StandardMode>,
    ) -> Result<(), lb_common_http_client::Error> {
        let mut state = self.lock();
        state.require_up()?;
        state.posted.push(tx.clone());
        state.push_mempool(tx);
        drop(state);
        Ok(())
    }

    async fn fund_tx(
        &self,
        request: WalletFundRequestBody,
    ) -> Result<WalletFundResponseBody, lb_common_http_client::Error> {
        let mut state = self.lock();
        state.require_up()?;
        state.fund(request)
    }
}
