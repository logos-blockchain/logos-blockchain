use core::fmt::{Debug, Display};

use async_trait::async_trait;
use futures::{
    Stream, StreamExt as _, future,
    stream::{self, BoxStream},
};
use lb_blend::proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs;
use lb_blend_service::epoch_info::{PolEpochInfo, PolInfoProvider as PolInfoProviderTrait};
use lb_chain_leader_service::{LeaderMsg, WinningPolEpochSlots, WinningPolSlotStream};
use lb_pol::{PolChainInputsData, PolWalletInputsData, PolWitnessInputsData};
use lb_services_utils::wait_until_services_are_ready;
use overwatch::{overwatch::OverwatchHandle, services::AsServiceId};
use tokio::sync::{mpsc, oneshot::channel};
use tokio_stream::wrappers::ReceiverStream;

use crate::CryptarchiaLeaderService;

/// How many winning slots of an epoch the channel to Blend holds, and so how
/// many wins the scan gets ahead of Blend before it waits for room.
///
/// Each winning slot backs one block proposal, so a few in hand are enough for
/// the next proposal to find its winning slot already scanned; the proofs that
/// slot backs are still generated when the proposal asks for them. The bound
/// caps the wins held, not the slots scanned: an epoch with at most this many
/// wins has all its slots scanned back to back as soon as it is handed over.
const WINNING_SLOTS_AHEAD: usize = 10;

/// The provider of a stream of winning `PoL` epoch slots for the Blend service,
/// without introducing a cyclic dependency from Blend service to chain service.
pub struct PolInfoProvider;

#[async_trait]
impl<RuntimeServiceId> PolInfoProviderTrait<RuntimeServiceId> for PolInfoProvider
where
    RuntimeServiceId:
        AsServiceId<CryptarchiaLeaderService> + Debug + Display + Send + Sync + 'static,
{
    type Stream = Box<dyn Stream<Item = PolEpochInfo> + Send + Unpin>;

    /// Subscribes to the chain-leader's per-epoch winning-slot handoffs (one
    /// per epoch) and maps each into a [`PolEpochInfo`], converting that
    /// epoch's stream of winning leadership inputs into the Blend
    /// leadership-quota inputs.
    async fn subscribe(
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Option<Self::Stream> {
        wait_until_services_are_ready!(
            overwatch_handle,
            // No timeout since chain-leader service becomes ready
            // only after switching to Online mode.
            None,
            CryptarchiaLeaderService
        )
        .await
        .ok()?;
        let cryptarchia_service_relay = overwatch_handle
            .relay::<CryptarchiaLeaderService>()
            .await
            .ok()?;
        let (sender, receiver) = channel();
        cryptarchia_service_relay
            .send(LeaderMsg::PotentialWinningPolEpochSlotStreamSubscribe { sender })
            .await
            .ok()?;
        let winning_pol_epoch_slots_stream = receiver.await.ok()?;
        Some(Box::new(scanned_epochs(winning_pol_epoch_slots_stream)))
    }
}

/// Each epoch's [`PolEpochInfo`] as the chain leader hands it over, with the
/// epochs' slots scanned for wins as this stream is polled.
///
/// The chain-leader stream does no work until polled, and each of its slots
/// costs a KMS lottery round trip per aged note, while Blend only polls for
/// leadership quota when a block proposal is waiting for it. Polled from there,
/// the scan would only run while a proposal waits, and the proposal would wait
/// for every slot from where the scan last stopped up to its own: minutes to
/// hours on a node with many notes, by which time the block is orphaned. Blend
/// polls this stream all along instead, as [`PolInfoProviderTrait::Stream`]
/// asks, so each epoch's scan runs from here, alongside later epochs' scans,
/// up to [`WINNING_SLOTS_AHEAD`] wins ahead of the proposals.
fn scanned_epochs(
    winning_epoch_slot_stream: impl Stream<Item = WinningPolEpochSlots> + Send + Unpin + 'static,
) -> impl Stream<Item = PolEpochInfo> + Send + Unpin {
    winning_epoch_slot_stream
        .map(epoch_info_then_scan)
        .flatten_unordered(None)
}

/// One epoch's [`PolEpochInfo`], then the scan of the epoch's slots, which
/// yields nothing: it is there to be driven by whoever polls the stream.
fn epoch_info_then_scan(
    WinningPolEpochSlots {
        epoch,
        state,
        slots,
    }: WinningPolEpochSlots,
) -> BoxStream<'static, PolEpochInfo> {
    let (found_slots, winning_slots) = mpsc::channel(WINNING_SLOTS_AHEAD);
    let epoch_info = PolEpochInfo {
        epoch,
        state,
        winning_pol_info_stream: Box::pin(ReceiverStream::new(winning_slots)),
    };
    let scan = stream::once(scan_for_winning_slots(slots, found_slots));

    stream::once(future::ready(epoch_info))
        .chain(scan.filter_map(|()| future::ready(None)))
        .boxed()
}

/// Sends each win among `slots` to `found_slots`, as Blend leadership-quota
/// inputs, until the slots run out or Blend drops the epoch.
///
/// KMS answers one request at a time, so while the scan runs, the leader's own
/// per-slot check takes turns with it.
async fn scan_for_winning_slots(
    slots: WinningPolSlotStream,
    found_slots: mpsc::Sender<ProofOfLeadershipQuotaInputs>,
) {
    // Drive each per-slot future and drop the non-winners.
    let mut wins = slots.filter_map(|winning_slot| winning_slot);
    loop {
        let win = tokio::select! {
            // Blend dropped the epoch: stop now.
            () = found_slots.closed() => return,
            win = wins.next() => win,
        };
        let Some(leader_private) = win else {
            return;
        };
        if found_slots
            .send(leadership_quota_inputs(leader_private.input()))
            .await
            .is_err()
        {
            // Channel closed, drop the scan stream.
            return;
        }
    }
}

/// The Blend leadership-quota inputs of a winning slot.
fn leadership_quota_inputs(inputs: &PolWitnessInputsData) -> ProofOfLeadershipQuotaInputs {
    let PolWitnessInputsData {
        wallet:
            PolWalletInputsData {
                aged_path,
                aged_selectors,
                note_value,
                output_number,
                secret_key,
                transaction_hash,
                ..
            },
        chain: PolChainInputsData { slot_number, .. },
    } = inputs;

    let aged_path_and_selectors = core::array::from_fn(|i| (aged_path[i], aged_selectors[i]));

    ProofOfLeadershipQuotaInputs {
        aged_path_and_selectors,
        note_value: *note_value,
        output_number: *output_number,
        secret_key: *secret_key,
        slot: *slot_number,
        transaction_hash: *transaction_hash,
    }
}

#[cfg(test)]
mod tests {
    use core::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    use std::sync::Arc;

    use futures::{FutureExt as _, Stream, StreamExt as _, stream};
    use lb_blend_service::epoch_info::{PolEpochInfo, PolEpochState, PolEpochStateSource};
    use lb_chain_leader_service::{WinningPolEpochSlots, WinningSlotFuture};
    use lb_core::{
        header::HeaderId,
        mantle::{Utxo, ledger::Note},
        proofs::leader_proof::{LeaderPrivate, LeaderPublic},
    };
    use lb_cryptarchia_engine::{Epoch, Slot};
    use lb_groth16::{AdditiveGroup as _, Fr};
    use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey};
    use lb_ledger::UtxoTree;
    use tokio::{sync::oneshot, task::yield_now, time::timeout};

    use super::{WINNING_SLOTS_AHEAD, scanned_epochs};

    const EPOCH_SLOTS: u64 = 100;

    /// The chain leader's handoff of an epoch made of `slots`.
    fn handoff(
        slots: impl Stream<Item = WinningSlotFuture> + Send + Unpin + 'static,
    ) -> WinningPolEpochSlots {
        WinningPolEpochSlots {
            epoch: Epoch::new(1),
            state: PolEpochState {
                nonce: Fr::ZERO,
                aged_utxo_root: Fr::ZERO,
                lottery_0: Fr::ZERO,
                lottery_1: Fr::ZERO,
                source: PolEpochStateSource {
                    tip_id: HeaderId::from([0; 32]),
                    tip_slot: Slot::from(0),
                    lib_id: HeaderId::from([0; 32]),
                    lib_slot: Slot::from(0),
                },
            },
            slots: Box::pin(slots),
        }
    }

    /// An epoch whose every slot wins, each waiting on a wakeup as a KMS round
    /// trip does, counting in `scanned` how many have been scanned.
    fn winning_epoch(scanned: &Arc<AtomicUsize>) -> WinningPolEpochSlots {
        let note = Utxo::new([0; 32], 0, Note::new(1, ZkPublicKey::zero()));
        let path = UtxoTree::new()
            .insert(note.id(), note)
            .0
            .path(&note.id())
            .expect("The note should be in the tree it was just inserted into.");
        let leader_pk = Ed25519Key::from_bytes(&[1; 32]).public_key();
        let scanned = Arc::clone(scanned);

        handoff(stream::iter(0..EPOCH_SLOTS).map(move |slot| {
            let public = LeaderPublic::new(Fr::ZERO, Fr::ZERO, Fr::ZERO, slot, Fr::ZERO, Fr::ZERO);
            let leader_private =
                LeaderPrivate::new(public, note, &path, &path, Fr::ZERO, &leader_pk);
            let scanned = Arc::clone(&scanned);
            let winning_slot: WinningSlotFuture = Box::pin(async move {
                yield_now().await;
                scanned.fetch_add(1, Ordering::SeqCst);
                Some(leader_private)
            });
            winning_slot
        }))
    }

    /// Polls `epochs` for a while, as Blend's event loop does, with no other
    /// epoch handed over meanwhile.
    async fn keep_polling(epochs: &mut (impl Stream<Item = PolEpochInfo> + Unpin)) {
        let next_epoch = timeout(Duration::from_millis(100), epochs.next()).await;
        assert!(next_epoch.is_err(), "No other epoch should be handed over.");
    }

    /// Polling the epochs gets an epoch's scan as far ahead of Blend as the
    /// bound, with nothing polling its winning slots, and each win Blend takes
    /// makes room for one more.
    #[tokio::test]
    async fn scan_gets_ahead_of_blend_up_to_the_bound() {
        let scanned = Arc::new(AtomicUsize::new(0));
        let mut epochs = scanned_epochs(stream::iter([winning_epoch(&scanned)]));
        let mut epoch_info = epochs
            .next()
            .await
            .expect("The epoch should be handed over.");

        keep_polling(&mut epochs).await;
        // A full channel, and one more win waiting for room.
        assert_eq!(scanned.load(Ordering::SeqCst), WINNING_SLOTS_AHEAD + 1);

        let first_win = epoch_info
            .winning_pol_info_stream
            .next()
            .now_or_never()
            .flatten()
            .expect("The first win should be ready before Blend asks for it.");
        assert_eq!(first_win.slot, 0);

        keep_polling(&mut epochs).await;
        assert_eq!(scanned.load(Ordering::SeqCst), WINNING_SLOTS_AHEAD + 2);
    }

    /// Dropping an epoch's winning slots stops its scan, even while a slot is
    /// waiting on KMS.
    #[tokio::test]
    async fn dropping_an_epoch_stops_its_scan() {
        // A slot whose KMS reply never comes.
        let (kms_reply, reply) = oneshot::channel::<Option<LeaderPrivate>>();
        let waiting_slot: WinningSlotFuture = Box::pin(async move { reply.await.ok().flatten() });
        let mut epochs = scanned_epochs(stream::iter([handoff(stream::iter([waiting_slot]))]));
        let epoch_info = epochs
            .next()
            .await
            .expect("The epoch should be handed over.");

        // Let the scan get to the slot before Blend drops the epoch.
        keep_polling(&mut epochs).await;
        drop(epoch_info);

        // No epoch follows, so the epochs end once the dropped one's scan stops.
        let next_epoch = timeout(Duration::from_secs(5), epochs.next())
            .await
            .expect("Dropping the epoch should stop its scan.");
        assert!(next_epoch.is_none());
        // The scan let go of the slot it was waiting on.
        assert!(kms_reply.is_closed());
    }
}
