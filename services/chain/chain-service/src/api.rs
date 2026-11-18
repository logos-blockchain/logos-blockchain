use std::{
    collections::HashMap,
    fmt::{Debug, Display},
    pin::Pin,
};

use futures::{Stream, TryStreamExt as _};
use lb_core::{
    block::{UncleHeaders, genesis::GenesisBlock},
    events::Events,
    header::{Header, HeaderId},
    mantle::note_set::NoteSetError,
    sdp::{Declaration, DeclarationId},
};
use lb_cryptarchia_engine::Slot;
use lb_ledger::LedgerError;
use lb_network_service::message::ChainSyncEvent;
use overwatch::{
    overwatch::OverwatchHandle,
    services::{AsServiceId, ServiceData, relay::OutboundRelay},
};
use thiserror::Error;
use tokio::sync::{broadcast, oneshot};

use crate::{
    AppliedBlock, BlockWithVerifiedHeader, ChainServiceInfo, ConsensusMsg, CryptarchiaInfo,
    EpochStateQueryResult, LibUpdate, ProcessedBlockEvent, Query, VerifiedBlockHeader,
};

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
    #[error("Block {0} has already been applied")]
    AlreadyApplied(HeaderId),
    /// The header is not valid on its own, so the block is invalid.
    #[error("Invalid block header: {0}")]
    InvalidHeader(String),
    /// The slot of the header is not after its parent's, so the block is
    /// invalid.
    #[error("Block slot {block:?} is not after its parent's slot {parent:?}")]
    InvalidSlot { parent: Slot, block: Slot },
    /// The proof of leadership in the header is invalid. The block ID does not
    /// cover the proof, so only the received copy is invalid, not the block.
    #[error("Invalid proof of leadership")]
    InvalidLeaderProof,
    /// An operation proof of block {0} is invalid. The block ID does not cover
    /// the proofs, so only the received copy is invalid, not the block.
    #[error("Invalid Operation proof in block {0}")]
    InvalidOperationProof(HeaderId),
    /// The uncles carried with the header are invalid.
    #[error("Invalid uncles: {0}")]
    InvalidUncles(String),
    /// The uncles or transactions carried are not the ones the header commits
    /// to, so only the received copy is invalid, not the block.
    #[error("Invalid block body: {0}")]
    InvalidBody(String),
    #[error("Failed to establish connection to chain-service: {0}")]
    CommsFailure(String),
    /// The local storage failed while the block was applied, which says
    /// nothing of the block: it can be applied again once the storage
    /// recovers.
    #[error("Local storage failed while applying block {id}: {reason}")]
    Storage { id: HeaderId, reason: String },
    #[error("Unexpected Error: {0}")]
    Unexpected(String),
}

/// The [`ApiError`] of `err`, raised while applying block `id`.
pub(crate) fn apply_block_api_error(err: crate::Error, id: HeaderId) -> ApiError {
    match err {
        crate::Error::BatchDeferredProofVerification(_) => ApiError::InvalidOperationProof(id),
        crate::Error::Ledger(LedgerError::VerificationError(err))
            if err.is_operation_proof_failure() =>
        {
            ApiError::InvalidOperationProof(id)
        }
        crate::Error::Storage(reason) => ApiError::Storage { id, reason },
        // The nullifier tree reads the store while the block is applied.
        crate::Error::Ledger(LedgerError::NoteSet(NoteSetError::Imt(err)))
            if err.is_store_failure() =>
        {
            ApiError::Storage {
                id,
                reason: err.to_string(),
            }
        }
        err => err.into(),
    }
}

pub struct CryptarchiaServiceApi<Cryptarchia>
where
    Cryptarchia: CryptarchiaServiceData,
{
    relay: OutboundRelay<Cryptarchia::Message>,
}

impl<Cryptarchia> Clone for CryptarchiaServiceApi<Cryptarchia>
where
    Cryptarchia: CryptarchiaServiceData,
{
    fn clone(&self) -> Self {
        Self {
            relay: self.relay.clone(),
        }
    }
}

impl<Cryptarchia> CryptarchiaServiceApi<Cryptarchia>
where
    Cryptarchia: CryptarchiaServiceData<Tx: Send>,
{
    #[must_use]
    pub const fn new(relay: OutboundRelay<Cryptarchia::Message>) -> Self {
        Self { relay }
    }

    /// Connect to the chain service through the overwatch `handle`.
    ///
    /// Fetches the relay for `Cryptarchia` itself, so the service type is
    /// named once, on this wrapper. Use [`Self::new`] when a relay is already
    /// at hand.
    ///
    /// # Panics
    ///
    /// Panics if the relay cannot be established, which only happens before
    /// the chain service has started.
    pub async fn from_overwatch_handle<RuntimeServiceId>(
        handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Self
    where
        RuntimeServiceId: AsServiceId<Cryptarchia> + Debug + Display + Sync,
    {
        let relay = handle
            .relay::<Cryptarchia>()
            .await
            .expect("Relay should be available after the service is started.");
        Self::new(relay)
    }

    /// Get the current consensus info including LIB, tip, slot, height, and
    /// mode
    pub async fn info(&self) -> Result<ChainServiceInfo, ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        self.relay
            .send(Query::Info { reply_channel }.into())
            .await
            .map_err(|error| ApiError::CommsFailure(format!("{error} while sending GetInfo")))?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving GetInfo"))
        })
    }

    /// Get the genesis block of the starting state
    pub async fn get_genesis_block(&self) -> Result<GenesisBlock, ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        self.relay
            .send(Query::GetGenesisBlock { reply_channel }.into())
            .await
            .map_err(|error| {
                ApiError::CommsFailure(format!("{error} while sending GetGenesisBlock"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving GetGenesisBlock"))
        })
    }

    /// The ID and slot of the block of the highest slot in the block tree.
    pub async fn highest_block(&self) -> Result<(HeaderId, Slot), ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        self.relay
            .send(Query::HighestBlock { reply_channel }.into())
            .await
            .map_err(|error| {
                ApiError::CommsFailure(format!("{error} while sending HighestBlock"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving HighestBlock"))
        })
    }

    /// Subscribe to new blocks
    pub async fn subscribe_new_blocks(
        &self,
    ) -> Result<broadcast::Receiver<ProcessedBlockEvent>, ApiError> {
        let (sender, receiver) = oneshot::channel();

        self.relay
            .send(Query::NewBlockSubscribe { sender }.into())
            .await
            .map_err(|error| {
                ApiError::CommsFailure(format!("{error} while sending NewBlockSubscribe"))
            })?;

        receiver.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving NewBlockSubscribe"))
        })
    }

    /// Subscribe to LIB (Last Immutable Block) updates
    pub async fn subscribe_lib_updates(&self) -> Result<broadcast::Receiver<LibUpdate>, ApiError> {
        let (sender, receiver) = oneshot::channel();

        self.relay
            .send(Query::LibSubscribe { sender }.into())
            .await
            .map_err(|error| {
                ApiError::CommsFailure(format!("{error} while sending LibSubscribe"))
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
        from_descendant: Option<HeaderId>,
        to_ancestor: Option<HeaderId>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<HeaderId, ApiError>> + Send>>, ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        self.relay
            .send(
                Query::GetHeaders {
                    from_descendant,
                    to_ancestor,
                    reply_channel,
                }
                .into(),
            )
            .await
            .map_err(|error| ApiError::CommsFailure(format!("{error} while sending GetHeaders")))?;

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
            .send(
                Query::GetLedgerState {
                    block_id,
                    reply_channel,
                }
                .into(),
            )
            .await
            .map_err(|error| {
                ApiError::CommsFailure(format!("{error} while sending GetLedgerState"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving GetLedgerState"))
        })
    }

    /// The ledger config of the era of `slot`.
    pub async fn get_ledger_config(&self, slot: Slot) -> Result<lb_ledger::Config, ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        self.relay
            .send(
                Query::GetLedgerConfigAtSlot {
                    slot,
                    reply_channel,
                }
                .into(),
            )
            .await
            .map_err(|error| {
                ApiError::CommsFailure(format!("{error} while sending GetLedgerConfig"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving GetLedgerConfig"))
        })
    }

    /// All declarations in the current SDP registry at the tip, keyed by
    /// declaration id. This is the live registry, not the epoch snapshot.
    pub async fn get_sdp_declarations(
        &self,
    ) -> Result<HashMap<DeclarationId, Declaration>, ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        self.relay
            .send(Query::GetSdpDeclarations { reply_channel }.into())
            .await
            .map_err(|error| {
                ApiError::CommsFailure(format!("{error} while sending GetSdpDeclarations"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving GetSdpDeclarations"))
        })
    }

    /// One declaration from the last irreversible block's SDP registry.
    pub async fn get_finalized_sdp_declaration(
        &self,
        declaration_id: DeclarationId,
    ) -> Result<Option<Declaration>, ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        self.relay
            .send(
                Query::GetFinalizedSdpDeclaration {
                    declaration_id,
                    reply_channel,
                }
                .into(),
            )
            .await
            .map_err(|error| {
                ApiError::CommsFailure(format!("{error} while sending GetFinalizedSdpDeclaration"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!(
                "{relay_error} while receiving GetFinalizedSdpDeclaration"
            ))
        })
    }

    /// All declarations for a service from the last irreversible block's SDP
    /// registry.
    pub async fn get_finalized_sdp_declarations(
        &self,
        service_type: lb_core::sdp::ServiceType,
    ) -> Result<Option<HashMap<DeclarationId, Declaration>>, ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        self.relay
            .send(
                Query::GetFinalizedSdpDeclarations {
                    service_type,
                    reply_channel,
                }
                .into(),
            )
            .await
            .map_err(|error| {
                ApiError::CommsFailure(format!("{error} while sending GetFinalizedSdpDeclarations"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!(
                "{relay_error} while receiving GetFinalizedSdpDeclarations"
            ))
        })
    }

    /// The SDP snapshot frozen for the tip's epoch, keyed by declaration id.
    pub async fn get_sdp_snapshot(&self) -> Result<HashMap<DeclarationId, Declaration>, ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        self.relay
            .send(Query::GetSdpSnapshot { reply_channel }.into())
            .await
            .map_err(|error| {
                ApiError::CommsFailure(format!("{error} while sending GetSdpSnapshot"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving GetSdpSnapshot"))
        })
    }

    /// Get the epoch state for a given slot
    pub async fn get_epoch_state(
        &self,
        slot: Slot,
    ) -> Result<Result<lb_ledger::EpochState, crate::Error>, ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        self.relay
            .send(
                Query::GetEpochState {
                    slot,
                    reply_channel,
                }
                .into(),
            )
            .await
            .map_err(|error| {
                ApiError::CommsFailure(format!("{error} while sending GetEpochState"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving GetEpochState resp"))
        })
    }

    /// Get the epoch state and the exact chain tip/LIB used to synthesize it.
    ///
    /// Requesting this richer result also registers its source for stale-source
    /// correlation if that tip later leaves the canonical chain.
    pub async fn get_epoch_state_with_source(
        &self,
        slot: Slot,
    ) -> Result<Result<EpochStateQueryResult, crate::Error>, ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        self.relay
            .send(
                Query::GetEpochStateWithSource {
                    slot,
                    reply_channel,
                }
                .into(),
            )
            .await
            .map_err(|error| {
                ApiError::CommsFailure(format!("{error} while sending GetEpochState"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving GetEpochState resp"))
        })
    }

    pub async fn get_block_events(&self, id: HeaderId) -> Result<Option<Events>, ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        self.relay
            .send(Query::GetBlockEvents { id, reply_channel }.into())
            .await
            .map_err(|error| {
                ApiError::CommsFailure(format!("{error} while sending GetBlockEvents"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving GetBlockEvents"))
        })
    }

    /// Selects uncles for a new block extending `parent` at `slot`.
    pub async fn select_uncles(
        &self,
        parent: HeaderId,
        slot: Slot,
    ) -> Result<UncleHeaders, ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        self.relay
            .send(
                Query::SelectUncles {
                    parent,
                    slot,
                    reply_channel,
                }
                .into(),
            )
            .await
            .map_err(|error| {
                ApiError::CommsFailure(format!("{error} while sending SelectUncles"))
            })?;

        rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving SelectUncles"))
        })
    }

    /// Verifies a block header through the chain service.
    pub async fn verify_header(
        &self,
        header: Header,
        uncle_headers: UncleHeaders,
    ) -> Result<VerifiedBlockHeader, ApiError> {
        let (reply_channel, rx) = oneshot::channel();
        self.relay
            .send(
                Query::VerifyHeader {
                    header: Box::new(header),
                    uncle_headers: Box::new(uncle_headers),
                    reply_channel,
                }
                .into(),
            )
            .await
            .map_err(|error| {
                ApiError::CommsFailure(format!("{error} while sending VerifyHeader"))
            })?;

        rx.await
            .map_err(|relay_error| {
                ApiError::CommsFailure(format!("{relay_error} while receiving VerifyHeader"))
            })?
            .map_err(Into::into)
    }

    /// Apply a block whose header is verified through the chain service, and
    /// return how the canonical chain changed if successful.
    pub async fn apply_block(
        &self,
        block: BlockWithVerifiedHeader<Cryptarchia::Tx>,
    ) -> Result<AppliedBlock<Cryptarchia::Tx>, ApiError> {
        let (reply_channel, rx) = oneshot::channel();

        let id = block.header().id();
        let boxed_block = Box::new(block);
        self.relay
            .send(ConsensusMsg::ApplyBlock {
                block: boxed_block,
                reply_channel,
            })
            .await
            .map_err(|error| ApiError::CommsFailure(format!("{error} while sending ApplyBlock")))?;

        rx.await
            .map_err(|relay_error| {
                ApiError::CommsFailure(format!("{relay_error} while receiving ApplyBlock resp"))
            })?
            .map_err(|err| apply_block_api_error(err, id))
    }

    /// Forward a chain sync event to the chain service.
    /// The response will be sent back via the `reply_sender` embedded in the
    /// event.
    pub async fn handle_chainsync_event(&self, event: ChainSyncEvent) -> Result<(), ApiError> {
        self.relay
            .send(ConsensusMsg::ChainSync(event))
            .await
            .map_err(|error| ApiError::CommsFailure(format!("{error} while sending ChainSync")))?;

        Ok(())
    }

    /// Notify chain-service that Initial Block Download has completed.
    /// Chain-service will start the prolonged bootstrap timer upon receiving
    /// this.
    pub async fn notify_ibd_completed(&self) -> Result<(), ApiError> {
        self.relay
            .send(ConsensusMsg::IbdCompleted)
            .await
            .map_err(|error| {
                ApiError::CommsFailure(format!("{error} while sending IbdCompleted"))
            })?;

        Ok(())
    }

    /// Wait until the chain becomes the Online mode.
    /// For details, see [`Query::SubscribeChainOnline`].
    pub async fn wait_until_chain_becomes_online(&self) -> Result<(), ApiError> {
        let (sender, receiver) = oneshot::channel();

        self.relay
            .send(Query::SubscribeChainOnline { sender }.into())
            .await
            .map_err(|error| {
                ApiError::CommsFailure(format!("{error} while sending SubscribeChainOnline"))
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

impl From<crate::Error> for ApiError {
    fn from(err: crate::Error) -> Self {
        match err {
            crate::Error::ParentMissing { parent, info } => Self::ParentMissing { parent, info },
            crate::Error::FutureBlock {
                block_slot,
                current_slot,
            } => Self::FutureBlock {
                block_slot,
                current_slot,
            },
            crate::Error::AlreadyApplied(block_id) => Self::AlreadyApplied(block_id),
            crate::Error::InvalidHeader(err) => Self::InvalidHeader(err.to_string()),
            crate::Error::InvalidSlot { parent, block } => Self::InvalidSlot { parent, block },
            crate::Error::InvalidLeaderProof => Self::InvalidLeaderProof,
            err @ (crate::Error::InvalidUncle { .. } | crate::Error::TooManyUncles { .. }) => {
                Self::InvalidUncles(err.to_string())
            }
            err => Self::Unexpected(format!("Failure while applying block: {err:?}")),
        }
    }
}
