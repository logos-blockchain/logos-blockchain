use core::{
    fmt::{Debug, Display},
    pin::Pin,
    task::{Context, Poll},
};

use async_trait::async_trait;
use futures::{
    FutureExt as _, Stream, StreamExt as _,
    future::{self, BoxFuture, FusedFuture as _},
    stream::{self, FusedStream as _},
};
use lb_blend::proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs;
use lb_blend_service::epoch_info::{PolEpochInfo, PolInfoProvider as PolInfoProviderTrait};
use lb_chain_leader_service::{LeaderMsg, WinningPolEpochSlots, WinningPolSlotStream};
use lb_core::proofs::leader_proof::LeaderPrivate;
use lb_pol::{PolChainInputsData, PolWalletInputsData, PolWitnessInputsData};
use lb_services_utils::wait_until_services_are_ready;
use overwatch::{overwatch::OverwatchHandle, services::AsServiceId};
use tokio::sync::{mpsc, oneshot::channel};
use tokio_stream::wrappers::ReceiverStream;

use crate::CryptarchiaLeaderService;

/// How many winning slots of an epoch the channel to Blend holds, and so how
/// many wins the scan gets ahead of Blend before it waits for room.
///
/// Each winning slot backs one block proposal. The scan only stops for room
/// when it already holds wins, so the bound never makes a proposal wait: the
/// next proposal takes one of them, which lets the scan go on. The proofs a win
/// backs are still generated when the proposal asks for them. The bound caps
/// the wins held, not the slots scanned: an epoch with at most this many wins
/// has all its slots scanned back to back as soon as it is handed over. Once
/// the next epoch is handed over, the wins held are all Blend has left of this
/// one.
const WINNING_SLOTS_LOOKAHEAD_COUNT: usize = 2;

/// The provider of a stream of winning `PoL` epoch slots for the Blend service,
/// without introducing a cyclic dependency from Blend service to chain service.
pub struct PolInfoProvider;

#[async_trait]
impl<RuntimeServiceId> PolInfoProviderTrait<RuntimeServiceId> for PolInfoProvider
where
    RuntimeServiceId:
        AsServiceId<CryptarchiaLeaderService> + Clone + Debug + Display + Send + Sync + 'static,
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
        Some(Box::new(ScannedEpochs::new(winning_pol_epoch_slots_stream)))
    }
}

/// Each epoch's [`PolEpochInfo`] as the chain leader hands it over, with the
/// latest epoch's slots scanned for wins as this stream is polled.
///
/// The chain-leader stream does no work until polled, and each of its slots
/// costs a KMS lottery round trip per aged note, while Blend only polls for
/// leadership quota when a block proposal is waiting for it. Polled from there,
/// the scan would only run while a proposal waits, and the proposal would wait
/// for every slot from where the scan last stopped up to its own: minutes to
/// hours on a node with many notes, by which time the block is orphaned. Blend
/// polls this stream all along instead, as [`PolInfoProviderTrait::Stream`]
/// asks, so the scan runs from here, up to [`WINNING_SLOTS_LOOKAHEAD_COUNT`]
/// wins ahead of the proposals.
struct ScannedEpochs<Epochs> {
    winning_slot_epoch_stream: stream::Fuse<Epochs>,
    /// The latest epoch's scan, terminated before the first epoch and once the
    /// scan has ended.
    scan: future::Fuse<BoxFuture<'static, ()>>,
}

impl<Epochs> ScannedEpochs<Epochs>
where
    Epochs: Stream<Item = WinningPolEpochSlots>,
{
    fn new(winning_slot_epoch_stream: Epochs) -> Self {
        Self {
            winning_slot_epoch_stream: winning_slot_epoch_stream.fuse(),
            scan: future::Fuse::terminated(),
        }
    }
}

impl<Epochs> Stream for ScannedEpochs<Epochs>
where
    Epochs: Stream<Item = WinningPolEpochSlots> + Unpin,
{
    type Item = PolEpochInfo;

    /// Starts the scan of a newly handed over epoch in place of the previous
    /// one, then drives the scan until it waits on KMS or for room for its
    /// wins.
    ///
    /// Ends once the chain leader hands over no more epochs and the scan has
    /// ended.
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let new_epoch = match this.winning_slot_epoch_stream.poll_next_unpin(cx) {
            Poll::Ready(Some(epoch)) => {
                let (epoch_info, scan) = epoch_info_and_scan(epoch);
                // Drops the previous epoch's scan, if it is still going.
                this.scan = scan.boxed().fuse();
                Some(epoch_info)
            }
            Poll::Ready(None) | Poll::Pending => None,
        };
        // A new epoch's scan is polled here right away, so it starts now and
        // registers the waker that re-polls us, without depending on an
        // external re-poll. A scan that has ended just stays pending.
        let _ = this.scan.poll_unpin(cx);

        match new_epoch {
            Some(epoch_info) => Poll::Ready(Some(epoch_info)),
            None if this.winning_slot_epoch_stream.is_terminated() && this.scan.is_terminated() => {
                Poll::Ready(None)
            }
            None => Poll::Pending,
        }
    }
}

/// A handed over epoch's [`PolEpochInfo`], and the scan of its slots that
/// sends the wins it finds to the info's winning-slot stream.
fn epoch_info_and_scan(
    WinningPolEpochSlots {
        epoch,
        state,
        slots,
    }: WinningPolEpochSlots,
) -> (PolEpochInfo, impl Future<Output = ()> + Send + 'static) {
    let (found_slots, winning_slots) = mpsc::channel(WINNING_SLOTS_LOOKAHEAD_COUNT);
    let epoch_info = PolEpochInfo {
        epoch,
        state,
        winning_pol_info_stream: Box::pin(ReceiverStream::new(winning_slots)),
    };
    (epoch_info, scan_for_winning_slots(slots, found_slots))
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
            .send(leadership_quota_inputs(&leader_private))
            .await
            .is_err()
        {
            // Channel closed, drop the scan stream.
            return;
        }
    }
}

/// The `PoQ` leader inputs a winning [`LeaderPrivate`] gives.
#[must_use]
pub fn leadership_quota_inputs(leader_private: &LeaderPrivate) -> ProofOfLeadershipQuotaInputs {
    let PolWitnessInputsData {
        wallet:
            PolWalletInputsData {
                note_value,
                note_nonce,
                note_program,
                note_data,
                set_selectors,
                commitment_aged_path,
                commitment_aged_selectors,
                low_leaf_aged_nullifier,
                low_leaf_aged_next_nullifier,
                low_leaf_aged_path,
                low_leaf_aged_selectors,
                set_aged_path,
                nullifier_key,
                staking_secret_key,
                ..
            },
        chain: PolChainInputsData { slot_number, .. },
    } = leader_private.input();

    ProofOfLeadershipQuotaInputs {
        slot: *slot_number,
        note_value: *note_value,
        note_nonce: *note_nonce,
        note_program: *note_program,
        note_data: *note_data,
        commitment_aged_path_and_selectors: core::array::from_fn(|i| {
            (commitment_aged_path[i], commitment_aged_selectors[i])
        }),
        low_leaf_aged_nullifier: *low_leaf_aged_nullifier,
        low_leaf_aged_next_nullifier: *low_leaf_aged_next_nullifier,
        low_leaf_aged_path_and_selectors: core::array::from_fn(|i| {
            (low_leaf_aged_path[i], low_leaf_aged_selectors[i])
        }),
        set_aged_path_and_selectors: core::array::from_fn(|i| (set_aged_path[i], set_selectors[i])),
        nullifier_key: *nullifier_key,
        staking_secret_key: *staking_secret_key,
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
        mantle::Note,
        proofs::leader_proof::{LeaderPrivate, test_utils::note_set_leader_inputs},
    };
    use lb_cryptarchia_engine::{Epoch, Slot};
    use lb_groth16::{AdditiveGroup as _, Fr};
    use lb_key_management_system_service::keys::ZkPublicKey;
    use lb_pol::PolWitnessInputsData;
    use tokio::{sync::oneshot, task::yield_now, time::timeout};

    use super::{ScannedEpochs, WINNING_SLOTS_LOOKAHEAD_COUNT, leadership_quota_inputs};

    const EPOCH_SLOTS: u64 = 100;

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "The test exercises the KMS-to-Blend proof path"
    )]
    async fn independent_credentials_generate_and_verify_leadership_quota() {
        use lb_blend::{
            message::crypto::proofs::PoQVerificationInputsMinusSigningKey,
            proofs::quota::{
                Quota,
                inputs::prove::{
                    PublicInputs,
                    public::{CoreInputs, LeaderInputs, PowInputs},
                },
            },
            provers::provers::{
                ProofsGeneratorSettings,
                leader::{LeaderProofsGenerator as _, RealLeaderProofsGenerator},
            },
        };
        use lb_core::{
            mantle::{
                note_set::{CommitmentMmr, NoteSetIndex, NoteSets},
                notes::derive_note_nullifier,
            },
            proofs::leader_proof::{LeaderPublic, pol_wallet_inputs},
        };
        use lb_key_management_system_service::{
            backend::{
                KMSBackend as _,
                preload::{PreloadKMSBackend, PreloadKMSBackendSettings},
            },
            keys::{Key, KeyMultiOperators, ZkKey, kdf_public_key},
            operators::zk::leader::BuildPrivateInputsWithLeaderKey,
        };

        let nf = Fr::from(7u64);
        let stake = Fr::from(19u64);
        let note = Note {
            stake_public_key: kdf_public_key(&stake),
            ..Note::new(1000, Fr::from(11u64), kdf_public_key(&nf))
        };
        let (_, path) = CommitmentMmr::new()
            .push_with_paths(note.commitment(), &mut [])
            .unwrap();
        let sets = NoteSets::new();
        let ledger = sets
            .ledger()
            .clone()
            .execute_adding(&[note.commitment()])
            .unwrap();
        let sets = sets.update_ledger(ledger).end_block(1, 1024).unwrap();
        let nullifier = derive_note_nullifier(&note.commitment(), &nf);
        let wallet =
            pol_wallet_inputs(&note, NoteSetIndex::Ledger, &nullifier, &path, &sets, &sets)
                .unwrap();
        let public = LeaderPublic::new(
            sets.eligible_root(),
            sets.eligible_root(),
            Fr::from(3u64),
            0,
            -Fr::from(1u64),
            Fr::ZERO,
        );
        let backend = PreloadKMSBackend::new(PreloadKMSBackendSettings {
            keys: [
                ("nf".into(), Key::Zk(ZkKey::from_direct_secret(nf))),
                ("stake".into(), Key::Zk(ZkKey::from_direct_secret(stake))),
            ]
            .into(),
        });
        let (tx, rx) = oneshot::channel();
        backend
            .execute_multiple(
                &["nf".into(), "stake".into()],
                KeyMultiOperators::Zk(Box::new(BuildPrivateInputsWithLeaderKey::new(
                    tx, public, wallet, 0, note,
                ))),
            )
            .await
            .unwrap();
        let private = rx.await.unwrap();
        let epoch_inputs = PoQVerificationInputsMinusSigningKey {
            core: CoreInputs {
                zk_root: Fr::from(1u64),
                quota: Quota::ONE,
            },
            leader: LeaderInputs {
                pol_eligible_aged: public.aged_root,
                pol_epoch_nonce: public.epoch_nonce,
                message_quota: Quota::ONE,
                lottery_0: public.lottery_0,
                lottery_1: public.lottery_1,
                pol_window_first_slot: 0,
                pol_window_last_slot: 0,
            },
            pow: PowInputs::disabled(),
        };
        let mut generator = RealLeaderProofsGenerator::new(
            ProofsGeneratorSettings {
                local_node_index: None,
                membership_size: 1,
                public_inputs: epoch_inputs,
                encapsulation_layers: 1.try_into().unwrap(),
                epoch: Epoch::new(0),
                pow_mining_pool: Arc::new(rayon::ThreadPoolBuilder::new().build().unwrap()),
            },
            Box::pin(stream::iter([leadership_quota_inputs(&private)])),
        );
        let proof = generator.get_next_proof().await.unwrap();
        let verify_inputs = PublicInputs {
            signing_key: proof.ephemeral_signing_key.public_key().into_inner(),
            core: epoch_inputs.core,
            leader: epoch_inputs.leader,
            pow: epoch_inputs.pow,
        };
        proof
            .proof_of_quota
            .into_inner()
            .verify(&verify_inputs)
            .unwrap();
    }

    /// The chain leader's handoff of an epoch made of `slots`.
    fn handoff(
        slots: impl Stream<Item = WinningSlotFuture> + Send + Unpin + 'static,
    ) -> WinningPolEpochSlots {
        WinningPolEpochSlots {
            epoch: Epoch::new(1),
            state: PolEpochState {
                nonce: Fr::ZERO,
                eligible_aged: Fr::ZERO,
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
        // A leader winning slot 0, standing in for the winner of every slot.
        let (_, winner) = note_set_leader_inputs(
            Fr::from(7u64),
            1000,
            Fr::from(11u64),
            Fr::ZERO,
            &Note::new(0, Fr::ZERO, ZkPublicKey::zero()),
        );
        let scanned = Arc::clone(scanned);

        handoff(stream::iter(0..EPOCH_SLOTS).map(move |_| {
            let leader_private = winner.clone();
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
        let mut epochs = ScannedEpochs::new(stream::iter([winning_epoch(&scanned)]));
        let mut epoch_info = epochs
            .next()
            .await
            .expect("The epoch should be handed over.");

        keep_polling(&mut epochs).await;
        // A full channel, and one more win waiting for room.
        assert_eq!(
            scanned.load(Ordering::SeqCst),
            WINNING_SLOTS_LOOKAHEAD_COUNT + 1
        );

        let first_win = epoch_info
            .winning_pol_info_stream
            .next()
            .now_or_never()
            .flatten()
            .expect("The first win should be ready before Blend asks for it.");
        assert_eq!(first_win.slot, 0);

        keep_polling(&mut epochs).await;
        assert_eq!(
            scanned.load(Ordering::SeqCst),
            WINNING_SLOTS_LOOKAHEAD_COUNT + 2
        );
    }

    /// Dropping an epoch's winning slots stops its scan, even while a slot is
    /// waiting on KMS.
    #[tokio::test]
    async fn dropping_an_epoch_stops_its_scan() {
        // A slot whose KMS reply never comes.
        let (kms_reply, reply) = oneshot::channel::<Option<LeaderPrivate>>();
        let waiting_slot: WinningSlotFuture = Box::pin(async move { reply.await.ok().flatten() });
        let mut epochs = ScannedEpochs::new(stream::iter([handoff(stream::iter([waiting_slot]))]));
        let epoch_info = epochs
            .next()
            .await
            .expect("The epoch should be handed over.");

        // Let the scan get to the slot before Blend drops the epoch.
        keep_polling(&mut epochs).await;
        drop(epoch_info);

        // No epoch follows, so the epochs end once the dropped one's scan
        // stops.
        let next_epoch = timeout(Duration::from_secs(5), epochs.next())
            .await
            .expect("Dropping the epoch should stop its scan.");
        assert!(next_epoch.is_none());
        // The scan let go of the slot it was waiting on.
        assert!(kms_reply.is_closed());
    }

    /// Handing over the next epoch stops the previous epoch's scan, even while
    /// a slot is waiting on KMS, and ends the previous epoch's winning slots.
    #[tokio::test]
    async fn handing_over_an_epoch_stops_the_previous_scan() {
        // A slot whose KMS reply never comes.
        let (kms_reply, reply) = oneshot::channel::<Option<LeaderPrivate>>();
        let waiting_slot: WinningSlotFuture = Box::pin(async move { reply.await.ok().flatten() });
        let mut epochs = ScannedEpochs::new(stream::iter([
            handoff(stream::iter([waiting_slot])),
            handoff(stream::empty()),
        ]));
        let mut previous_epoch = epochs
            .next()
            .await
            .expect("The epoch should be handed over.");
        assert!(
            !kms_reply.is_closed(),
            "The scan should be waiting on the slot."
        );

        epochs
            .next()
            .await
            .expect("The next epoch should be handed over.");
        // The previous scan let go of the slot it was waiting on, and its
        // epoch's winning slots end.
        assert!(kms_reply.is_closed());
        assert!(
            previous_epoch
                .winning_pol_info_stream
                .next()
                .await
                .is_none()
        );
    }

    #[test]
    fn leadership_quota_inputs_take_the_aged_half_of_the_pol_witness() {
        let (_, private) = note_set_leader_inputs(
            Fr::from(7u64),
            1000,
            Fr::from(11u64),
            Fr::ZERO,
            &Note::new(0, Fr::ZERO, ZkPublicKey::zero()),
        );
        let PolWitnessInputsData { wallet, chain } = private.input().clone();
        let inputs = leadership_quota_inputs(&private);
        assert_eq!(
            (
                inputs.slot,
                inputs.note_value,
                inputs.note_nonce,
                inputs.nullifier_key,
                inputs.staking_secret_key
            ),
            (
                chain.slot_number,
                1000,
                Fr::from(11u64),
                wallet.nullifier_key,
                wallet.staking_secret_key
            )
        );
        assert_eq!(
            inputs.low_leaf_aged_nullifier,
            wallet.low_leaf_aged_nullifier
        );
        assert_eq!(
            inputs.low_leaf_aged_path_and_selectors[0],
            (
                wallet.low_leaf_aged_path[0],
                wallet.low_leaf_aged_selectors[0]
            )
        );
        assert_eq!(
            inputs.commitment_aged_path_and_selectors[0],
            (
                wallet.commitment_aged_path[0],
                wallet.commitment_aged_selectors[0]
            )
        );
        assert_eq!(
            inputs.set_aged_path_and_selectors[0],
            (wallet.set_aged_path[0], wallet.set_selectors[0])
        );
    }
}
