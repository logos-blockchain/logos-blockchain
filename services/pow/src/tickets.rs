use std::{
    iter,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Stream, StreamExt as _, future::BoxFuture, stream};
use lb_chain_service::{ConsensusMsg, ProcessedBlockEvent, Slot, api::CryptarchiaServiceApi};
use lb_core::{header::HeaderId, mantle::ops::pow::ClaimPowRewardOp};
use lb_key_management_system_keys::keys::UnsecuredZkKey;
use tokio_stream::{StreamMap, StreamNotifyClose, wrappers::BroadcastStream};
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
    processed_block_stream: Pin<Box<dyn Stream<Item = ProcessedBlockEvent> + Send>>,
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
        let processed_block_stream: Pin<Box<dyn Stream<Item = ProcessedBlockEvent> + Send>> =
            Box::pin(stream.filter_map(async |event| match event {
                Ok(event) => Some(event),
                Err(e) => {
                    error!("Missed new block event due to: {e}");
                    None
                }
            }));
        Ok(Self {
            cryptarchia_api,
            processed_block_stream,
            tickets_search: StreamMap::new(),
        })
    }

    fn new_block_search_stream(
        cryptarchia_api: CryptarchiaServiceApi<CryptarchiaServiceData, RuntimeServiceId>,
        block_header: HeaderId,
        block_slot: Slot,
    ) -> WinnerTicketStream
    where
        CryptarchiaServiceData:
            'static + Send + Sync + overwatch::services::ServiceData<Message = ConsensusMsg<Tx>>,
        RuntimeServiceId: 'static + Send + Sync + Unpin,
        Tx: 'static + Send + Sync,
    {
        let tasks = iter::repeat_with(move || {
            let cryptarchia_api = cryptarchia_api.clone();
            async move {
                let Ok(Ok(epoch_state)) = cryptarchia_api.get_epoch_state(block_slot).await else {
                    warn!("Epoch state not found for block slot: {block_slot:?}");
                    return None;
                };
                let Ok(Some(ledger_state)) = cryptarchia_api.get_ledger_state(block_header).await
                else {
                    warn!("Ledger state not found for block: {block_header:?}");
                    return None;
                };

                // Ticket computation is heavy, need to be run in blocking threads not to block
                // async execution.
                let task = tokio::task::spawn_blocking(move || {
                    let mut rng = rand::thread_rng();
                    let sk = UnsecuredZkKey::from_rng(&mut rng);
                    let pk = sk.to_public_key();
                    let claim = ClaimPowRewardOp {
                        epoch_nonce: epoch_state.nonce,
                        block_hash: block_header.into(),
                        public_key: pk,
                    };
                    let ticket = claim.get_puzzle_ticket();
                    let difficulty = ledger_state.mantle_ledger().pow.reward_difficulty();
                    ticket
                        .validate_difficulty_reward(&difficulty)
                        .is_ok()
                        .then_some((sk, claim))
                });
                task.await.ok().flatten()
            }
        });
        let results = stream::iter(tasks).buffer_unordered(16);
        Box::pin(tokio_stream::StreamExt::filter_map(
            results,
            |maybe_winner| maybe_winner,
        ))
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

        if let Poll::Ready(Some(ProcessedBlockEvent {
            block_id, tip_slot, ..
        })) = this.processed_block_stream.poll_next_unpin(cx)
        {
            let stream =
                Self::new_block_search_stream(this.cryptarchia_api.clone(), block_id, tip_slot);
            this.tickets_search.insert(block_id, stream);
        }

        Poll::Pending
    }
}
