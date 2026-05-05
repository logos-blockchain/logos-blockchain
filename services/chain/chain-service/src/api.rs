use std::pin::Pin;

use futures::{Stream, TryStreamExt as _};
use lb_core::{block::Block, header::HeaderId};
use lb_cryptarchia_engine::Slot;
use lb_network_service::message::ChainSyncEvent;
use overwatch::services::{ServiceData, relay::OutboundRelay};
use thiserror::Error;
use tokio::sync::{broadcast, oneshot};

use crate::{ChainServiceInfo, ConsensusMsg, CryptarchiaInfo, LibUpdate, ProcessedBlockEvent};

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
        info: Box<CryptarchiaInfo>,
    },
    #[error("Block from future slot({block_slot:?}): current_slot:{current_slot:?}")]
    FutureBlock {
        block_slot: Slot,
        current_slot: Slot,
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
    pub async fn info(&self) -> Result<ChainServiceInfo, ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        self.relay
            .send(ConsensusMsg::Info { reply_channel })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending GetInfo"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving GetInfo"))
        })
    }

    /// Subscribe to new blocks
    pub async fn subscribe_new_blocks(
        &self,
    ) -> Result<broadcast::Receiver<ProcessedBlockEvent>, ApiError> {
        let (sender, receiver) = oneshot::channel();

        self.relay
            .send(ConsensusMsg::NewBlockSubscribe { sender })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending NewBlockSubscribe"))
            })?;

        receiver.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving NewBlockSubscribe"))
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
            ApiError::CommsFailure(format!("{relay_error} while receiving LibSubscribe"))
        })
    }

    /// Get headers in the range from descendant (inclusive) to ancestor
    /// (inclusive).
    ///
    /// If `from_descendant` is None, defaults to tip
    /// If `to_ancestor` is None, defaults to LIB
    pub async fn get_headers(
        &self,
        from_descendant: HeaderId,
        to_ancestor: HeaderId,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<HeaderId, ApiError>> + Send>>, ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        self.relay
            .send(ConsensusMsg::GetHeaders {
                from_descendant: Some(from_descendant),
                to_ancestor: Some(to_ancestor),
                reply_channel,
            })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending GetHeaders"))
            })?;

        let stream = rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving GetHeaders"))
        })?;

        Ok(Box::pin(stream.map_err(|e| {
            ApiError::Unexpected(format!("Error while fetching block IDs: {e}"))
        })))
    }

    /// Get the ledger state at a specific block
    pub async fn get_ledger_state(
        &self,
        block_id: HeaderId,
    ) -> Result<Option<lb_ledger::LedgerState>, ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        self.relay
            .send(ConsensusMsg::GetLedgerState {
                block_id,
                reply_channel,
            })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending GetLedgerState"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving GetLedgerState"))
        })
    }

    /// Get the epoch state for a given slot
    pub async fn get_epoch_state(
        &self,
        slot: Slot,
    ) -> Result<Result<lb_ledger::EpochState, crate::Error>, ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        self.relay
            .send(ConsensusMsg::GetEpochState {
                slot,
                reply_channel,
            })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending GetEpochState"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving GetEpochState resp"))
        })
    }

    /// Get the epoch and consensus configs
    pub async fn get_epoch_config(
        &self,
    ) -> Result<
        (
            lb_cryptarchia_engine::EpochConfig,
            lb_cryptarchia_engine::Config,
        ),
        ApiError,
    > {
        let (reply_channel, rx) = oneshot::channel();

        self.relay
            .send(ConsensusMsg::GetEpochConfig { reply_channel })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending GetEpochConfig"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving GetEpochConfig"))
        })
    }

    /// Apply a block through the chain service,
    /// and return the tip and reorged txs if successful.
    pub async fn apply_block(
        &self,
        block: Block<Cryptarchia::Tx>,
    ) -> Result<(HeaderId, Vec<Cryptarchia::Tx>), ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        let boxed_block = Box::new(block);
        self.relay
            .send(ConsensusMsg::ApplyBlock {
                block: boxed_block,
                reply_channel,
            })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending ApplyBlock"))
            })?;

        rx.await
            .map_err(|relay_error| {
                ApiError::CommsFailure(format!("{relay_error} while receiving ApplyBlock resp"))
            })?
            .map_err(|err| match err {
                crate::Error::ParentMissing { parent, info } => {
                    ApiError::ParentMissing { parent, info }
                }
                crate::Error::FutureBlock {
                    block_slot,
                    current_slot,
                } => ApiError::FutureBlock {
                    block_slot,
                    current_slot,
                },
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

    /// Wait until the chain becomes the Online mode.
    /// For details, see [`ConsensusMsg::SubscribeChainOnline`].
    pub async fn wait_until_chain_becomes_online(&self) -> Result<(), ApiError> {
        let (sender, receiver) = oneshot::channel();

        self.relay
            .send(ConsensusMsg::SubscribeChainOnline { sender })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending SubscribeChainOnline"))
            })?;

        let mut subscriber = receiver.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!(
                "{relay_error} while receiving SubscribeChainOnline"
            ))
        })?;

        // Wait until the channel returns `true`.
        subscriber
            .wait_for(|&is_online| is_online)
            .await
            .map_err(|e| {
                ApiError::CommsFailure(format!("Failed to wait for chain to become online: {e}"))
            })?;

        Ok(())
    }
}
