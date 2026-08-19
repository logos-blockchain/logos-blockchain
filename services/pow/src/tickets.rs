//! Proof-of-Work reward ticket generation.
//!
//! Watches the chain for newly processed blocks and, for every block still
//! within the reward window, runs a concurrent search for a "winning" ticket:
//! a random key whose derived puzzle ticket meets the block's difficulty
//! target. Winning tickets are surfaced through the [`TicketGenerator`] stream.

use std::{
    collections::{HashMap, HashSet},
    iter,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Stream, StreamExt as _, future::BoxFuture, stream};
use lb_chain_service::{
    ConsensusMsg, EpochState, ProcessedBlockEvent, Slot, api::CryptarchiaServiceApi,
};
use lb_core::{
    crypto::ZkHash,
    header::HeaderId,
    mantle::ops::pow::{ClaimPowRewardOp, PowTarget, SLOT_WINDOW},
};
use lb_key_management_system_keys::keys::UnsecuredZkKey;
use lb_ledger::LedgerState;
use tokio_stream::{
    StreamMap,
    wrappers::{BroadcastStream, errors::BroadcastStreamRecvError},
};
use tracing::{error, log::warn};

/// A winning Proof-of-Work reward: the secret key that produced the ticket,
/// paired with the reward-claim operation to be published.
pub type WinnerTicket = (UnsecuredZkKey, ClaimPowRewardOp);
/// A boxed stream of [`WinnerTicket`]s found for a single block.
pub type WinnerTicketStream = Pin<Box<dyn Stream<Item = WinnerTicket> + Send>>;
/// A boxed future resolving to a single reward-claim operation.
pub type TicketSearchTask<'a> = BoxFuture<'a, ClaimPowRewardOp>;

/// A [`Stream`] of winning Proof-of-Work reward tickets.
///
/// It subscribes to the chain's processed-block events and drives, per block,
/// an independent concurrent search for winning tickets. Searches for blocks
/// that fall out of the reward window are pruned as the tip advances.
pub struct TicketGenerator<Tx, CryptarchiaServiceData, RuntimeServiceId>
where
    CryptarchiaServiceData:
        Send + overwatch::services::ServiceData<Message = ConsensusMsg<Tx>> + 'static,
{
    /// Handle to the chain service, used to subscribe to processed blocks and
    /// to query the epoch and ledger state of each block.
    cryptarchia_api: CryptarchiaServiceApi<CryptarchiaServiceData, RuntimeServiceId>,
    /// Stream of processed blocks, each enriched with the epoch and ledger
    /// state required to search for tickets.
    processed_block_stream:
        Pin<Box<dyn Stream<Item = (EpochState, LedgerState, ProcessedBlockEvent)> + Send>>,
    /// Ongoing per-block ticket searches, keyed by block header. Each entry
    /// yields winning tickets for that block as they are found.
    tickets_search: StreamMap<HeaderId, WinnerTicketStream>,
    /// Index from a block's slot to the headers with an active search at that
    /// slot, used to prune searches once their block leaves the reward window.
    tickets_search_by_slot: HashMap<Slot, HashSet<HeaderId>>,
}

impl<Tx, CryptarchiaServiceData, RuntimeServiceId>
    TicketGenerator<Tx, CryptarchiaServiceData, RuntimeServiceId>
where
    CryptarchiaServiceData:
        Send + Sync + overwatch::services::ServiceData<Message = ConsensusMsg<Tx>> + 'static,
    RuntimeServiceId: Send + Sync + 'static,
    Tx: Send + Sync + 'static,
{
    /// Creates a new [`TicketGenerator`].
    ///
    /// Subscribes to the chain service's new-block stream and wires the
    /// enrichment pipeline that attaches each block's epoch and ledger state.
    ///
    /// # Errors
    ///
    /// Returns an error if the subscription to the chain service fails.
    pub async fn new(
        cryptarchia_api: CryptarchiaServiceApi<CryptarchiaServiceData, RuntimeServiceId>,
    ) -> Result<Self, lb_chain_service::api::ApiError> {
        let stream = BroadcastStream::new(cryptarchia_api.subscribe_new_blocks().await?);
        let cryptarchia_api_send = cryptarchia_api.clone();
        let processed_block_stream: Pin<
            Box<dyn Stream<Item = (EpochState, LedgerState, ProcessedBlockEvent)> + Send>,
        > = Box::pin(stream.filter_map(move |event| {
            let cryptarchia_api = cryptarchia_api_send.clone();
            async move { Self::process_block_event(event, cryptarchia_api).await }
        }));
        Ok(Self {
            cryptarchia_api,
            processed_block_stream,
            tickets_search: StreamMap::new(),
            tickets_search_by_slot: HashMap::new(),
        })
    }

    /// Enriches a raw processed-block event with its epoch and ledger state.
    ///
    /// Returns `None` (dropping the event) when the broadcast subscription
    /// lagged, or when the epoch or ledger state for the block cannot be
    /// fetched from the chain service.
    async fn process_block_event(
        event: Result<ProcessedBlockEvent, BroadcastStreamRecvError>,
        cryptarchia_api: CryptarchiaServiceApi<CryptarchiaServiceData, RuntimeServiceId>,
    ) -> Option<(EpochState, LedgerState, ProcessedBlockEvent)> {
        match event {
            Ok(
                event @ ProcessedBlockEvent {
                    block_id, tip_slot, ..
                },
            ) => {
                let Ok(Ok(epoch_state)) = cryptarchia_api.get_epoch_state(tip_slot).await else {
                    warn!("Epoch state not found for block slot: {tip_slot:?}");
                    return None;
                };
                let Ok(Some(ledger_state)) = cryptarchia_api.get_ledger_state(block_id).await
                else {
                    warn!("Ledger state not found for block: {block_id:?}");
                    return None;
                };
                Some((epoch_state, ledger_state, event))
            }
            Err(e) => {
                error!("Missed new block event due to: {e}");
                None
            }
        }
    }

    /// Builds an unbounded stream that searches for winning tickets for a
    /// single block.
    ///
    /// Up to 16 attempts run concurrently; each draws a fresh random key and
    /// checks the resulting ticket against the block's difficulty target. The
    /// stream yields every winning `(secret key, claim)` pair it finds and
    /// never terminates on its own — it is dropped once the block leaves the
    /// reward window (see [`Self::prune_out_of_window_streams`]).
    fn new_block_search_stream(
        block_header: HeaderId,
        epoch_state: &EpochState,
        ledger_state: &LedgerState,
    ) -> WinnerTicketStream
    where
        CryptarchiaServiceData:
            'static + Send + Sync + overwatch::services::ServiceData<Message = ConsensusMsg<Tx>>,
        RuntimeServiceId: 'static + Send + Sync + Unpin,
        Tx: 'static + Send + Sync,
    {
        let epoch_nonce = epoch_state.nonce;
        let difficulty = ledger_state.mantle_ledger().pow.reward_difficulty();
        #[allow(
            rustc::closure_returning_async_block,
            reason = "`repeat_with` takes a FnMut not an async closure"
        )]
        let tasks = iter::repeat_with(move || {
            Self::search_winner_ticket(block_header, epoch_nonce, difficulty)
        });
        let results = stream::iter(tasks).buffer_unordered(16);
        Box::pin(tokio_stream::StreamExt::filter_map(
            results,
            |maybe_winner| maybe_winner,
        ))
    }

    /// Runs a single ticket-search attempt for a block.
    ///
    /// Generates a random key, builds the reward claim, and validates its
    /// puzzle ticket against `difficulty`. The heavy computation is off-loaded
    /// to a blocking thread so it does not stall the async runtime. Returns the
    /// winning `(key, claim)` when the ticket meets the difficulty target,
    /// otherwise `None`.
    async fn search_winner_ticket(
        block_header: HeaderId,
        epoch_nonce: ZkHash,
        difficulty: PowTarget,
    ) -> Option<(UnsecuredZkKey, ClaimPowRewardOp)> {
        // Ticket computation is heavy, need to be run in blocking threads not to block
        // async execution.
        let task = tokio::task::spawn_blocking(move || {
            let mut rng = rand::thread_rng();
            let sk = UnsecuredZkKey::from_rng(&mut rng);
            let pk = sk.to_public_key();
            let claim = ClaimPowRewardOp {
                epoch_nonce,
                block_hash: block_header.into(),
                public_key: pk,
            };
            let ticket = claim.get_puzzle_ticket();
            ticket
                .validate_difficulty_reward(&difficulty)
                .is_ok()
                .then_some((sk, claim))
        });
        task.await.ok().flatten()
    }

    /// Drops the ticket searches for every block whose slot is older than
    /// `frontier_slot`, i.e. blocks that have fallen out of the reward window
    /// and can no longer produce claimable tickets.
    fn prune_out_of_window_streams(&mut self, frontier_slot: Slot) {
        let to_remove = self
            .tickets_search_by_slot
            .extract_if(|k, _| k < &frontier_slot)
            .flat_map(|(_, headers)| headers);
        for header in to_remove {
            self.tickets_search.remove(&header);
        }
    }
}

impl<Tx, CryptarchiaServiceData, RuntimeServiceId> Stream
    for TicketGenerator<Tx, CryptarchiaServiceData, RuntimeServiceId>
where
    CryptarchiaServiceData:
        Send + Sync + overwatch::services::ServiceData<Message = ConsensusMsg<Tx>> + 'static,
    RuntimeServiceId: Send + Sync + Unpin + 'static,
    Tx: Send + Sync + 'static,
{
    type Item = WinnerTicket;

    /// Advances the generator.
    ///
    /// First drains any winning ticket that a per-block search has already
    /// produced. Otherwise it ingests the next processed block: if the block is
    /// still within the reward window it starts a new search for it, and it
    /// always prunes the searches for blocks that have aged out of the window.
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Poll::Ready(Some((_, winner_ticket))) = this.tickets_search.poll_next_unpin(cx) {
            return Poll::Ready(Some(winner_ticket));
        }

        if let Poll::Ready(Some((
            ref epoch_state,
            ref ledger_state,
            ProcessedBlockEvent {
                block_id,
                block_slot,
                tip_slot,
                ..
            },
        ))) = this.processed_block_stream.poll_next_unpin(cx)
        {
            // compute which slot is old enough
            let frontier_slot = tip_slot.saturating_sub(Slot::new(SLOT_WINDOW));
            // trigger new stream if its new enough
            if frontier_slot < block_slot {
                let stream = Self::new_block_search_stream(block_id, epoch_state, ledger_state);
                this.tickets_search.insert(block_id, stream);
                this.tickets_search_by_slot
                    .entry(block_slot)
                    .or_default()
                    .insert(block_id);
            }
            // prune old enough winning tickets
            this.prune_out_of_window_streams(frontier_slot);
        }

        Poll::Pending
    }
}
