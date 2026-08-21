use core::fmt::{self, Debug, Display};

use bytes::Bytes;
use futures::StreamExt as _;
use lb_core::{
    block::Block,
    events::Events,
    mantle::{
        TxGasCalculator,
        ledger::verification_mode::StandardMode,
        traits::{PreverifiedMantleTransaction, SignedMantleTx},
        transactions::{GasPrices, states::Preverified},
    },
};
use lb_storage_service::{api::chain::StorageChainApi, backends::StorageBackend};
use serde::{Serialize, de::DeserializeOwned};
use tracing::info;

use super::{Phase, PhaseTag, ProlongedBootstrapPeriod};
use crate::{
    ConsensusMsg, LOG_TARGET,
    service::{Service, reject_chain_sync_event},
};

/// `InitialBlockDownload` phase: Apply blocks until IBD completes.
pub struct InitialBlockDownload {
    /// Whether IBD has been skipped during `AwaitingGenesisTime`,
    /// in which case the phase is a no-op.
    ibd_skipped: bool,
}

impl InitialBlockDownload {
    pub(crate) const fn new(ibd_skipped: bool) -> Self {
        Self { ibd_skipped }
    }
}

impl Phase for InitialBlockDownload {
    const TAG: PhaseTag = PhaseTag::InitialBlockDownload;
}

impl Debug for InitialBlockDownload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Self::TAG.fmt(f)
    }
}

impl<Tx, Storage, RuntimeServiceId> Service<InitialBlockDownload, Tx, Storage, RuntimeServiceId>
where
    Tx: PreverifiedMantleTransaction
        + SignedMantleTx<Preverified, StandardMode>
        + TxGasCalculator<Context = GasPrices>
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + Unpin
        + 'static,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    <Storage as StorageChainApi>::Block: TryFrom<Block<Tx>> + TryInto<Block<Tx>> + Into<Bytes>,
    <Storage as StorageChainApi>::Events: TryFrom<Events> + TryInto<Events>,
    RuntimeServiceId: Display + 'static,
{
    /// Runs the phase until IBD completes.
    /// A no-op if IBD has been already skipped during previous phases.
    pub async fn process_initial_block_download(
        mut self,
    ) -> Service<ProlongedBootstrapPeriod, Tx, Storage, RuntimeServiceId> {
        info!(target: LOG_TARGET, "entering {:?} phase", self.phase);

        if self.phase.ibd_skipped {
            info!(target: LOG_TARGET, "IBD has been already skipped: finishing {:?} phase", self.phase);
            return self.with_phase(ProlongedBootstrapPeriod);
        }

        self.run_event_loop().await;
        self.with_phase(ProlongedBootstrapPeriod)
    }

    async fn run_event_loop(&mut self) {
        loop {
            tokio::select! {
                Some(msg) = self.inbound_relay.next() => match msg {
                    ConsensusMsg::Query(query) => {
                        self.process_query(query).await;
                    }
                    ConsensusMsg::ApplyBlock { block, reply_channel } => {
                        self.apply_block_and_reply(*block, reply_channel).await;
                    }
                    ConsensusMsg::ChainSync(event) => reject_chain_sync_event(event).await,
                    ConsensusMsg::IbdCompleted => {
                        info!(target: LOG_TARGET, "IBD completed: finishing {:?} phase", self.phase);
                        return;
                    }
                },
                Some(tick) = self.slot_timer.next() => self.current_slot = tick.slot,
                _ = self.state_recording_timer.tick() => self.record_recovery_state(),
            }
        }
    }
}
