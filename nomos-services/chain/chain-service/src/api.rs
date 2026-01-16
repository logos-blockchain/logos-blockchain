use nomos_core::{block::Block, header::HeaderId};
use nomos_network::message::ChainSyncEvent;
use overwatch::services::{ServiceData, relay::OutboundRelay};
use thiserror::Error;
use tokio::sync::{broadcast, oneshot};

use crate::{ConsensusMsg, CryptarchiaInfo, LibUpdate};

pub trait CryptarchiaServiceData:
    ServiceData<Message = ConsensusMsg<Self::Tx>> + Send + 'static
{
    type Tx;
}
impl<T, Tx> CryptarchiaServiceData for T
where
    T: ServiceData<Message = ConsensusMsg<Tx>> + Send + 'static,
{
    type Tx = Tx;
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Missing parent while applying block {parent}, {info:?}")]
    ParentMissing {
        parent: HeaderId,
        info: CryptarchiaInfo,
    },
    #[error("Failed to establish connection to chain-service: {0}")]
    CommsFailure(String),
    #[error("Unexpected Error: {0}")]
    Unexpected(String),
}

pub struct CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>
where
    Cryptarchia: CryptarchiaServiceData,
{
    relay: OutboundRelay<Cryptarchia::Message>,
    _id: std::marker::PhantomData<RuntimeServiceId>,
}

impl<Cryptarchia, RuntimeServiceId> Clone for CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>
where
    Cryptarchia: CryptarchiaServiceData,
{
    fn clone(&self) -> Self {
        Self {
            relay: self.relay.clone(),
            _id: std::marker::PhantomData,
        }
    }
}

impl<Cryptarchia, RuntimeServiceId> CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>
where
    Cryptarchia: CryptarchiaServiceData<Tx: Send + Sync>,
    RuntimeServiceId: Sync,
{
    #[must_use]
    pub const fn new(relay: OutboundRelay<Cryptarchia::Message>) -> Self {
        Self {
            relay,
            _id: std::marker::PhantomData,
        }
    }

    /// Get the current consensus info including LIB, tip, slot, height, and
    /// mode
    pub async fn info(&self) -> Result<CryptarchiaInfo, ApiError> {
        let (tx, rx) = oneshot::channel();

        self.relay
            .send(ConsensusMsg::Info { tx })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending GetInfo"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while recving GetInfo"))
        })
    }

    /// Subscribe to new blocks
    pub async fn subscribe_new_blocks(&self) -> Result<broadcast::Receiver<HeaderId>, ApiError> {
        let (sender, receiver) = oneshot::channel();

        self.relay
            .send(ConsensusMsg::NewBlockSubscribe { sender })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending NewBlockSubscribe"))
            })?;

        receiver.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while recving NewBlockSubscribe"))
        })
    }

    /// Subscribe to LIB (Last Immutable Block) updates
    pub async fn subscribe_lib_updates(&self) -> Result<broadcast::Receiver<LibUpdate>, ApiError> {
        let (sender, receiver) = oneshot::channel();

        self.relay
            .send(ConsensusMsg::LibSubscribe { sender })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending LibSubscribe"))
            })?;

        receiver.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while recving LibSubscribe"))
        })
    }

    /// Get headers in the range from `from` to `to`
    /// If `from` is None, defaults to tip
    /// If `to` is None, defaults to LIB
    pub async fn get_headers(
        &self,
        from: Option<HeaderId>,
        to: Option<HeaderId>,
    ) -> Result<Vec<HeaderId>, ApiError> {
        let (tx, rx) = oneshot::channel();

        self.relay
            .send(ConsensusMsg::GetHeaders { from, to, tx })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending GetHeaders"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while recving GetHeaders"))
        })
    }

    /// Get all headers from a specific block to LIB
    pub async fn get_headers_to_lib(&self, from: HeaderId) -> Result<Vec<HeaderId>, ApiError> {
        self.get_headers(Some(from), None).await
    }

    /// Get all headers from tip to a specific block
    pub async fn get_headers_from_tip(&self, to: HeaderId) -> Result<Vec<HeaderId>, ApiError> {
        self.get_headers(None, Some(to)).await
    }

    /// Get the ledger state at a specific block
    pub async fn get_ledger_state(
        &self,
        block_id: HeaderId,
    ) -> Result<Option<nomos_ledger::LedgerState>, ApiError> {
        let (tx, rx) = oneshot::channel();

        self.relay
            .send(ConsensusMsg::GetLedgerState { block_id, tx })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending GetLedgerState"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while recving GetLedgerState"))
        })
    }

    /// Get the epoch state for a given slot
    pub async fn get_epoch_state(
        &self,
        slot: cryptarchia_engine::Slot,
    ) -> Result<Option<nomos_ledger::EpochState>, ApiError> {
        let (tx, rx) = oneshot::channel();

        self.relay
            .send(ConsensusMsg::GetEpochState { slot, tx })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending GetEpochState"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while recving GetEpochState resp"))
        })
    }

    /// Apply a block through the chain service,
    /// and return the tip and reorged txs if successful.
    pub async fn apply_block(
        &self,
        block: Block<Cryptarchia::Tx>,
    ) -> Result<(HeaderId, Vec<Cryptarchia::Tx>), ApiError> {
        let (tx, rx) = oneshot::channel();

        let boxed_block = Box::new(block);
        self.relay
            .send(ConsensusMsg::ApplyBlock {
                block: boxed_block,
                tx,
            })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending ApplyBlock"))
            })?;

        rx.await
            .map_err(|relay_error| {
                ApiError::CommsFailure(format!("{relay_error} while recving ApplyBlock resp"))
            })?
            .map_err(|err| match err {
                crate::Error::ParentMissing { parent, info } => {
                    ApiError::ParentMissing { parent, info }
                }
                err => ApiError::Unexpected(format!("Failure while applying block: {err:?}")),
            })
    }

    /// Forward a chain sync event to the chain service.
    /// The response will be sent back via the `reply_sender` embedded in the
    /// event.
    pub async fn handle_chainsync_event(&self, event: ChainSyncEvent) -> Result<(), ApiError> {
        self.relay
            .send(ConsensusMsg::ChainSync(event))
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending ChainSync"))
            })?;

        Ok(())
    }

    /// Notify chain-service that Initial Block Download has completed.
    /// Chain-service will start the prolonged bootstrap timer upon receiving
    /// this.
    pub async fn notify_ibd_completed(&self) -> Result<(), ApiError> {
        self.relay
            .send(ConsensusMsg::IbdCompleted)
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending IbdCompleted"))
            })?;

        Ok(())
    }
}
