use std::{
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
    mantle::ops::pow::{ClaimPowRewardOp, PowTarget},
};
use lb_key_management_system_keys::keys::UnsecuredZkKey;
use lb_ledger::LedgerState;
use tokio_stream::{
    StreamMap, StreamNotifyClose,
    wrappers::{BroadcastStream, errors::BroadcastStreamRecvError},
};
use tracing::{error, log::warn};

pub type WinnerTicket = (UnsecuredZkKey, ClaimPowRewardOp);
pub type WinnerTicketStream = Pin<Box<dyn Stream<Item = WinnerTicket> + Send>>;
pub type TicketSearchTask<'a> = BoxFuture<'a, ClaimPowRewardOp>;

pub struct TicketGenerator<Tx, CryptarchiaServiceData, RuntimeServiceId>
where
    CryptarchiaServiceData:
        Send + overwatch::services::ServiceData<Message = ConsensusMsg<Tx>> + 'static,
{
    cryptarchia_api: CryptarchiaServiceApi<CryptarchiaServiceData, RuntimeServiceId>,
    processed_block_stream:
        Pin<Box<dyn Stream<Item = (EpochState, LedgerState, ProcessedBlockEvent)> + Send>>,
    tickets_search: StreamMap<HeaderId, WinnerTicketStream>,
}

impl<Tx, CryptarchiaServiceData, RuntimeServiceId>
    TicketGenerator<Tx, CryptarchiaServiceData, RuntimeServiceId>
where
    CryptarchiaServiceData:
        Send + Sync + overwatch::services::ServiceData<Message = ConsensusMsg<Tx>> + 'static,
    RuntimeServiceId: Send + Sync + 'static,
    Tx: Send + Sync + 'static,
{
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
        })
    }

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

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Poll::Ready(Some((_, winner_ticket))) = this.tickets_search.poll_next_unpin(cx) {
            return Poll::Ready(Some(winner_ticket));
        }

        if let Poll::Ready(Some((
            ref epoch_state,
            ref ledger_state,
            ProcessedBlockEvent {
                block_id, tip_slot, ..
            },
        ))) = this.processed_block_stream.poll_next_unpin(cx)
        {
            let stream = Self::new_block_search_stream(block_id, epoch_state, ledger_state);
            this.tickets_search.insert(block_id, stream);
        }

        Poll::Pending
    }
}
