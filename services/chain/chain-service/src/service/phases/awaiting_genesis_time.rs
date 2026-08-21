use core::fmt::{self, Debug, Display};
use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
    time::Duration,
};

use bytes::Bytes;
use futures::StreamExt as _;
use lb_core::{
    block::Block,
    events::Events,
    header::HeaderId,
    mantle::{
        traits::{GenesisTx as _, MantleTxWithProofs, PreverifiedMantleTx},
        transactions::GasPrices,
    },
};
use lb_cryptarchia_engine::Slot;
use lb_storage_service::{api::chain::StorageChainApi, backends::StorageBackend};
use overwatch::services::{relay::InboundRelay, state::StateUpdater};
use serde::{Serialize, de::DeserializeOwned};
use time::OffsetDateTime;
use tokio::{
    sync::{broadcast, oneshot},
    time::Sleep,
};
use tracing::{debug, error, info};

use super::{InitialBlockDownload, Phase, PhaseTag};
use crate::{
    ConsensusMsg, Cryptarchia, CryptarchiaConsensusState, Error, LOG_TARGET, LibUpdate,
    ProcessedBlockEvent, StartingState,
    notifier::ChainOnlineNotifier,
    relays::CryptarchiaConsensusRelays,
    service::{Service, reject_chain_sync_event},
    sync::block_provider::BlockProvider,
};

/// `AwaitingGenesisTime` phase: Wait until the genesis time is reached.
pub struct AwaitingGenesisTime {
    /// Fires when the genesis time is reached.
    /// `None` if the genesis time is already in the past,
    /// in which case the phase is a no-op.
    genesis_timer: Option<Pin<Box<Sleep>>>,
}

impl Phase for AwaitingGenesisTime {
    const TAG: PhaseTag = PhaseTag::AwaitingGenesisTime;
}

impl Debug for AwaitingGenesisTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Self::TAG.fmt(f)
    }
}

impl<Tx, Storage, RuntimeServiceId> Service<AwaitingGenesisTime, Tx, Storage, RuntimeServiceId>
where
    Tx: PreverifiedMantleTx
        + MantleTxWithProofs<Context = GasPrices>
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
    /// Create a [`Service`] in its first phase.
    #[expect(clippy::too_many_arguments, reason = "Need all ingredients")]
    pub fn new(
        starting_state: StartingState,
        cryptarchia: Cryptarchia,
        inbound_relay: InboundRelay<ConsensusMsg<Tx>>,
        state_updater: StateUpdater<Option<CryptarchiaConsensusState>>,
        new_block_subscription_sender: broadcast::Sender<ProcessedBlockEvent>,
        lib_subscription_sender: broadcast::Sender<LibUpdate>,
        chain_online_notifier: ChainOnlineNotifier,
        current_slot: Slot,
        storage_blocks_to_remove: HashSet<HeaderId>,
        relays: CryptarchiaConsensusRelays<Tx, Storage, RuntimeServiceId>,
        sync_blocks_provider: BlockProvider<Storage, Tx>,
        slot_timer: lb_time_service::EpochSlotTickStream,
        state_recording_timer: tokio::time::Interval,
        prolonged_bootstrap_period: Duration,
    ) -> Self {
        Self {
            phase: AwaitingGenesisTime {
                genesis_timer: create_genesis_timer(starting_state),
            },
            cryptarchia,
            inbound_relay,
            state_updater,
            new_block_subscription_sender,
            lib_subscription_sender,
            chain_online_notifier,
            current_slot,
            storage_blocks_to_remove,
            relays,
            sync_blocks_provider,
            slot_timer,
            state_recording_timer,
            prolonged_bootstrap_period,
            epoch_state_query_sources: HashMap::new(),
        }
    }

    /// Runs the phase until the genesis time is reached.
    /// A no-op if the genesis time is already in the past.
    ///
    /// `IbdCompleted` can be received during this phase if IBD has been
    /// skipped, in which case the next phase is a no-op.
    pub async fn process_awaiting_genesis_time(
        mut self,
    ) -> Service<InitialBlockDownload, Tx, Storage, RuntimeServiceId> {
        info!(target: LOG_TARGET, "entering {:?} phase", self.phase);

        let Some(genesis_timer) = self.phase.genesis_timer.take() else {
            info!(target: LOG_TARGET, "genesis time is already in the past: finishing {:?} phase with no-op", self.phase);
            return self.with_phase(InitialBlockDownload::new(false));
        };

        let ibd_skipped = self.run_event_loop(genesis_timer).await;
        self.with_phase(InitialBlockDownload::new(ibd_skipped))
    }

    async fn run_event_loop(&mut self, mut genesis_timer: Pin<Box<Sleep>>) -> bool {
        let mut ibd_skipped = false;
        loop {
            tokio::select! {
                () = genesis_timer.as_mut() => {
                    info!(target: LOG_TARGET, "genesis time reached: finishing {:?} phase", self.phase);
                    return ibd_skipped;
                }
                Some(msg) = self.inbound_relay.next() => match msg {
                    ConsensusMsg::Query(query) => {
                        self.process_query(query).await;
                    }
                    ConsensusMsg::ApplyBlock { reply_channel, .. } => {
                        self.reject_apply_block_msg(reply_channel);
                    }
                    ConsensusMsg::ChainSync(event) => reject_chain_sync_event(event).await,
                    ConsensusMsg::IbdCompleted => {
                        info!(target: LOG_TARGET, "Initial Block Download has been skipped during {:?} phase", self.phase);
                        ibd_skipped = true;
                    }
                },
                Some(tick) = self.slot_timer.next() => self.current_slot = tick.slot,
                _ = self.state_recording_timer.tick() => self.record_recovery_state(),
            }
        }
    }

    fn reject_apply_block_msg(
        &self,
        reply_channel: oneshot::Sender<Result<(HeaderId, Vec<Tx>), Error>>,
    ) {
        debug!(target: LOG_TARGET, "rejecting a block received during {:?} phase", self.phase);
        reply_channel
            .send(Err(Error::AwaitingGenesisTime))
            .unwrap_or_else(|_| {
                error!(target: LOG_TARGET, "failed to send error through channel");
            });
    }
}

/// Creates a timer that fires when the genesis time is reached,
/// if the starting state is `Genesis` and the genesis time is in the future.
/// Returns `None`, otherwise.
fn create_genesis_timer(starting_state: StartingState) -> Option<Pin<Box<Sleep>>> {
    if let StartingState::Genesis { genesis_block } = starting_state {
        let genesis_time: OffsetDateTime = genesis_block
            .genesis_tx()
            .cryptarchia_parameter()
            .genesis_time
            .into();
        let now = OffsetDateTime::now_utc();

        if genesis_time > now {
            info!(target: LOG_TARGET, %genesis_time, "genesis time is in the future");
            let delay = (genesis_time - now).try_into().unwrap_or_default();
            return Some(Box::pin(tokio::time::sleep(delay)));
        }
    }
    None
}
