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
use lb_cryptarchia_engine::{PrunedBlocks, Slot};
use lb_cryptarchia_sync::HeaderId;
use lb_storage_service::{api::chain::StorageChainApi, backends::StorageBackend};
use serde::{Serialize, de::DeserializeOwned};
use tracing::{debug, error, info};

use super::{Following, Phase, PhaseTag};
use crate::{
    ConsensusMsg, Error, LOG_TARGET,
    service::{
        Service, delete_stale_blocks_from_storage, immutable_blocks_index, reject_chain_sync_event,
    },
    storage::{StorageAdapter as _, adapters::StorageAdapter},
};

/// `ProlongedBootstrapPeriod` phase: Apply blocks until PBP is over.
/// A no-op if `Cryptarchia` was initialized as online.
pub struct ProlongedBootstrapPeriod;

impl Phase for ProlongedBootstrapPeriod {
    const TAG: PhaseTag = PhaseTag::ProlongedBootstrapPeriod;
}

impl Debug for ProlongedBootstrapPeriod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Self::TAG.fmt(f)
    }
}

impl<Tx, Storage, RuntimeServiceId> Service<ProlongedBootstrapPeriod, Tx, Storage, RuntimeServiceId>
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
    /// Runs the phase until PBP is over,
    /// and then switches `Cryptarchia` to online.
    /// A no-op if `Cryptarchia` was initialized as online.
    pub async fn process_prolonged_bootstrap_period(
        mut self,
    ) -> Service<Following, Tx, Storage, RuntimeServiceId> {
        info!(target: LOG_TARGET, "entering {:?} phase", self.phase);

        if !self.cryptarchia.is_bootstrapping() {
            info!(target: LOG_TARGET, "chain is already not in bootstrapping state: finishing {:?} phase", self.phase);
            return self.with_phase(Following);
        }

        self.run_event_loop().await;

        info!(target: LOG_TARGET, "Prolonged Bootstrap Period completed: switching chain to online");
        self = self.switch_to_online().await;
        self.record_recovery_state();

        self.with_phase(Following)
    }

    async fn run_event_loop(&mut self) {
        let mut pbp_timer = Box::pin(tokio::time::sleep(self.prolonged_bootstrap_period));
        loop {
            tokio::select! {
                () = pbp_timer.as_mut() => {
                    return;
                },
                Some(msg) = self.inbound_relay.next() => match msg {
                    ConsensusMsg::Query(query) => {
                        self.process_query(query).await;
                    }
                    ConsensusMsg::ApplyBlock { block, reply_channel } => {
                        self.apply_block_and_reply(*block, reply_channel).await;
                    }
                    ConsensusMsg::ChainSync(event) => reject_chain_sync_event(event).await,
                    ConsensusMsg::IbdCompleted => {
                        debug!(target: LOG_TARGET, "ignoring IbdCompleted: already in {:?} phase", self.phase);
                    }
                },
                Some(tick) = self.slot_timer.next() => self.current_slot = tick.slot,
                _ = self.state_recording_timer.tick() => self.record_recovery_state(),
            }
        }
    }

    /// Switch `Cryptarchia` to online, notify subscribers,
    /// and prune the stale part of the chain from memory and storage.
    async fn switch_to_online(mut self) -> Self {
        let (cryptarchia, pruned_blocks) = self.cryptarchia.online();
        self.cryptarchia = cryptarchia;
        info!(target: LOG_TARGET, "chain switched to online");

        self.chain_online_notifier.notify();

        if let Err(err) = Self::store_immutable_blocks_index(
            &pruned_blocks,
            None,
            self.cryptarchia.lib(),
            self.cryptarchia.lib_branch().slot(),
            self.relays.storage_adapter(),
        )
        .await
        {
            error!(target: LOG_TARGET, %err, "failed to store immutable block IDs");
        }

        self.storage_blocks_to_remove = delete_stale_blocks_from_storage(
            pruned_blocks.stale_blocks().copied(),
            &self.storage_blocks_to_remove,
            self.relays.storage_adapter(),
        )
        .await;

        self
    }

    /// Store immutable block IDs to storage, including the new LIB if needed.
    /// If `prev_lib` is None, always includes the new LIB.
    /// If `prev_lib` is Some, only includes new LIB if it changed.
    async fn store_immutable_blocks_index(
        pruned_blocks: &PrunedBlocks<HeaderId>,
        prev_lib: Option<HeaderId>,
        new_lib: HeaderId,
        new_lib_slot: Slot,
        storage_adapter: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
    ) -> Result<(), Error> {
        let immutable_blocks =
            immutable_blocks_index(pruned_blocks, prev_lib, new_lib, new_lib_slot);

        storage_adapter
            .store_immutable_block_ids(immutable_blocks)
            .await
            .map_err(|e| Error::Storage(format!("Failed to store immutable block ids: {e}")))
    }
}
