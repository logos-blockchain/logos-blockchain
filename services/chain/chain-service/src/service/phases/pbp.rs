use core::fmt::{self, Debug};

use futures::{StreamExt as _, future::OptionFuture};
use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode};
use lb_core::mantle::{
    ledger::verification_mode::StandardMode,
    traits::{PreverifiedMantleTransaction, SignedMantleTx, StorageSize},
    transactions::states::Preverified,
};
use serde::{Serialize, de::DeserializeOwned};
use tracing::{debug, info};

use super::{Following, Phase, PhaseTag};
use crate::{
    ConsensusMsg, LOG_TARGET,
    service::{Service, delete_stale_blocks_from_storage, go_online, reject_chain_sync_event},
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

impl<Tx> Service<ProlongedBootstrapPeriod, Tx>
where
    Tx: PreverifiedMantleTransaction
        + SignedMantleTx<Preverified, StandardMode>
        + StorageSize
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + BinaryEncode
        + BinaryDecode<Context = u32>
        + Send
        + Sync
        + Unpin
        + 'static,
{
    /// Runs the phase until PBP is over,
    /// and then switches `Cryptarchia` to online.
    /// A no-op if `Cryptarchia` was initialized as online.
    pub async fn process_prolonged_bootstrap_period(mut self) -> Service<Following, Tx> {
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
                // Going online changes the chain, which a block under
                // verification would undo on commit.
                () = pbp_timer.as_mut(), if self.block_under_verification.is_none() => {
                    return;
                },
                Some(msg) = self.inbound_relay.next() => match msg {
                    ConsensusMsg::Query(query) => {
                        self.process_query(query).await;
                    }
                    ConsensusMsg::ApplyBlock { block, reply_channel } => {
                        self.apply_block(*block, reply_channel);
                    }
                    ConsensusMsg::ChainSync(event) => reject_chain_sync_event(event).await,
                    ConsensusMsg::IbdCompleted => {
                        debug!(target: LOG_TARGET, "ignoring IbdCompleted: already in {:?} phase", self.phase);
                    }
                },
                Some(verification) = OptionFuture::from(self.block_under_verification.as_mut()) => {
                    self.finish_block(verification).await;
                }
                Some(tick) = self.slot_timer.next() => self.current_slot = tick.slot,
                _ = self.state_recording_timer.tick() => self.record_recovery_state(),
            }
        }
    }

    /// Switch `Cryptarchia` to online, notify subscribers,
    /// and prune the stale part of the chain from memory and storage.
    async fn switch_to_online(mut self) -> Self {
        let (cryptarchia, pruned_blocks, immutable_blocks_to_index) = go_online(
            self.cryptarchia,
            &self.immutable_blocks_to_index,
            self.relays.storage(),
        )
        .await;
        self.cryptarchia = cryptarchia;
        self.immutable_blocks_to_index = immutable_blocks_to_index;
        info!(target: LOG_TARGET, "chain switched to online");

        self.chain_online_notifier.notify();

        self.storage_blocks_to_remove = delete_stale_blocks_from_storage(
            pruned_blocks.stale_blocks().copied(),
            &self.storage_blocks_to_remove,
            self.relays.storage(),
        )
        .await;

        self
    }
}
