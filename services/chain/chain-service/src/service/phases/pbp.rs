use core::fmt::{self, Debug};

use futures::StreamExt as _;
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
    ConsensusMsg, Error, LOG_TARGET,
    service::{
        Service, delete_stale_blocks_from_storage, immutable_blocks_index, reject_chain_sync_event,
    },
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
        + BinaryDecode<Context = lb_core::mantle::transactions::TxDecodeContext>
        + Send
        + Sync
        + Unpin
        + 'static,
{
    /// Runs the phase until PBP is over,
    /// and then switches `Cryptarchia` to online.
    /// A no-op if `Cryptarchia` was initialized as online.
    pub async fn process_prolonged_bootstrap_period(
        mut self,
    ) -> Result<Service<Following, Tx>, Error> {
        info!(target: LOG_TARGET, "entering {:?} phase", self.phase);

        if !self.cryptarchia.is_bootstrapping() {
            info!(target: LOG_TARGET, "chain is already not in bootstrapping state: finishing {:?} phase", self.phase);
            return Ok(self.with_phase(Following));
        }

        self.run_event_loop().await;

        info!(target: LOG_TARGET, "Prolonged Bootstrap Period completed: switching chain to online");
        Ok(self.switch_to_online().await?.with_phase(Following))
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
                _ = self.state_recording_timer.tick() => self.record_recovery_state().await,
            }
        }
    }

    /// Switch `Cryptarchia` to online, notify subscribers,
    /// and prune the stale part of the chain from memory and storage.
    async fn switch_to_online(mut self) -> Result<Self, Error> {
        let (cryptarchia, pruned_blocks) = self.cryptarchia.online();
        self.cryptarchia = cryptarchia;

        let immutable_blocks = immutable_blocks_index(
            &pruned_blocks,
            None,
            self.cryptarchia.lib(),
            self.cryptarchia.lib_branch().slot(),
        );

        let mut pending = self.storage_blocks_to_remove.clone();
        pending.extend(pruned_blocks.stale_blocks().copied());

        self.store_recovery_state(immutable_blocks, pending).await?;

        info!(target: LOG_TARGET, "chain switched to online");
        self.chain_online_notifier.notify();

        self.storage_blocks_to_remove = delete_stale_blocks_from_storage(
            pruned_blocks.stale_blocks().copied(),
            &self.storage_blocks_to_remove,
            self.relays.storage(),
        )
        .await;

        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, num::NonZeroUsize, sync::Arc, time::Duration};

    use futures::stream;
    use lb_binary_codec::bincode::DeserializeOp as _;
    use lb_core::{block::UncleHeaders, era::ForkDigest, mantle::SignedOps};
    use lb_cryptarchia_engine::{Slot, State};
    use lb_storage_service::{StorageMsg, api::StorageApi, recovery::recovery_key};
    use overwatch::services::relay::{OutboundRelay, Relay};
    use tokio::{
        sync::{broadcast, mpsc},
        time::interval,
    };

    use super::{Preverified, ProlongedBootstrapPeriod, Service, StandardMode};
    use crate::{
        DEFAULT_CHECKPOINT_INTERVAL_BLOCKS, LibCheckpoint,
        notifier::ChainOnlineNotifier,
        relays::CryptarchiaConsensusRelays,
        service::EpochStateQuerySourceTracker,
        states::{RECOVERY_PROGRESS_KEY_SUFFIX, RecoveryProgress, RecoveryRecorder},
        sync::{block_provider::BlockProvider, config::BlockProviderConfig},
        tests::{genesis_cryptarchia_on, ledger_config, single_era, try_build_block, utxo},
    };

    #[tokio::test]
    async fn going_online_commits_the_immutable_index_and_recovery_before_notifying() {
        type Tx = SignedOps<Preverified, StandardMode>;

        let (leader_key, leader_utxo) = utxo();
        let eras = single_era(ledger_config(1.try_into().unwrap()));
        let mut chain = genesis_cryptarchia_on(leader_utxo, Arc::clone(&eras));
        let fork_digests = Arc::new(eras.map(|_| ForkDigest::new([0x11; 32])));
        let checkpoint = LibCheckpoint::from_cryptarchia_and_unpruned_blocks(
            &chain,
            HashSet::new(),
            &fork_digests,
        )
        .unwrap();

        let mut blocks = Vec::new();
        let mut next_slot = Slot::new(1);
        for _ in 0..2 {
            let (block, _) = try_build_block(
                &chain,
                chain.tip(),
                leader_utxo,
                &leader_key,
                next_slot,
                UncleHeaders::empty(),
            )
            .unwrap();
            let slot = block.header().slot();
            blocks.push((slot, block.header().id()));

            chain.try_apply_block(block, slot).unwrap();
            next_slot = slot.strict_add(1.into());
        }
        let [(lib_slot, lib_id), (tip_slot, tip_id)] = blocks.as_slice() else {
            panic!("two blocks were applied");
        };
        assert_eq!(chain.lib(), checkpoint.lib);

        let (storage_tx, mut storage_rx) = mpsc::channel(1);
        let (broadcast_tx, _broadcast_rx) = mpsc::channel(1);
        let (time_tx, _time_rx) = mpsc::channel(1);
        let consensus = Relay::new(1);
        let storage = StorageApi::<Tx>::new(OutboundRelay::new(storage_tx));
        let sync_blocks_provider = BlockProvider::new(
            storage.clone(),
            BlockProviderConfig {
                batch_size: NonZeroUsize::MIN,
            },
        );
        let notifier = ChainOnlineNotifier::new(State::Bootstrapping);
        let online = notifier.subscribe();
        let service = Service {
            phase: ProlongedBootstrapPeriod,
            cryptarchia: chain,
            inbound_relay: consensus.inbound_relay,
            recovery_recorder: RecoveryRecorder::new(
                &checkpoint,
                fork_digests,
                DEFAULT_CHECKPOINT_INTERVAL_BLOCKS,
            ),
            new_block_subscription_sender: broadcast::channel(1).0,
            lib_subscription_sender: broadcast::channel(1).0,
            chain_online_notifier: notifier,
            current_slot: *tip_slot,
            storage_blocks_to_remove: HashSet::new(),
            relays: CryptarchiaConsensusRelays::new(
                OutboundRelay::new(broadcast_tx),
                storage,
                OutboundRelay::new(time_tx),
            ),
            sync_blocks_provider,
            slot_timer: Box::pin(stream::pending()),
            state_recording_timer: interval(Duration::from_secs(60)),
            prolonged_bootstrap_period: Duration::ZERO,
            epoch_state_query_sources: EpochStateQuerySourceTracker::default(),
        };

        let mut transition = Box::pin(service.switch_to_online());
        let request = tokio::select! {
            request = storage_rx.recv() => request.unwrap(),
            _ = &mut transition => panic!("online transition completed without a recovery write"),
        };
        let StorageMsg::StoreImmutableBlockIdsAndRecovery {
            ids,
            recovery_writes,
            response_tx,
        } = request
        else {
            panic!("expected the online transition's storage batch");
        };
        assert!(!*online.borrow());
        assert_eq!(ids.get(lib_slot), Some(lib_id));

        let [entry] = recovery_writes.as_slice() else {
            panic!("expected a compact recovery record");
        };
        assert_eq!(entry.key, recovery_key(RECOVERY_PROGRESS_KEY_SUFFIX));
        let progress = RecoveryProgress::from_bytes(&entry.value).unwrap();
        assert_eq!(progress.lib, *lib_id);
        assert_eq!(progress.tip, *tip_id);
        assert_eq!(progress.last_engine_state.unwrap().state, State::Online);

        response_tx.send(Ok(())).unwrap();
        transition.await.unwrap();
        assert!(*online.borrow());
    }
}
