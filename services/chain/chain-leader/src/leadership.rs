use std::fmt::{Debug, Display};

use futures::StreamExt as _;
use lb_chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use lb_core::{
    header::HeaderId,
    mantle::Utxo,
    proofs::leader_proof::{Groth16LeaderProof, LeaderPrivate, LeaderPublic},
};
use lb_cryptarchia_engine::{Epoch, Slot};
use lb_key_management_system_service::{
    api::KmsServiceApi, backend::preload::KeyId, keys::Ed25519Key,
    operators::zk::leader::BuildPrivateInputsWithLeaderKey,
};
use lb_ledger::{EpochState, UtxoTree};
use lb_time_service::{EpochSlotTickStream, SlotTick, TimeServiceMessage};
use lb_wallet_service::{
    UtxoWithKeyId,
    api::{WalletApi, WalletServiceData},
};
use overwatch::services::{AsServiceId, relay::OutboundRelay};
use rand::rngs::OsRng;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    LOG_TARGET, WinningPolEpochSlots, WinningPolSlotStream,
    kms::{KmsAdapter, PreloadKmsService},
};

/// Bounded buffer of pre-computed winning slots per epoch, per subscriber. The
/// scan blocks (backpressure) once this many winning slots are buffered ahead
/// of the consumer, so the whole epoch is never materialized at once, since the
/// number of winning slots can be very large for long epochs and/or relatively
/// easy lotteries.
const WINNING_SLOT_BUFFER_SIZE: usize = 8;

/// Return a leadership proof and signing key if the current slot is a winning
/// one for any of the eligible UTXOs, for use in a block proposal.
///
/// If the slot is not a winning one, it returns `None`.
#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: address this in a dedicated refactor"
)]
pub async fn build_proof_for<Wallet, RuntimeServiceId>(
    utxos: &[UtxoWithKeyId],
    latest_tree: &UtxoTree,
    epoch_state: &EpochState,
    slot: Slot,
    wallet: &WalletApi<Wallet, RuntimeServiceId>,
    kms: &(impl KmsAdapter<RuntimeServiceId, KeyId = KeyId> + Sync),
) -> Option<(Groth16LeaderProof, Ed25519Key)>
where
    Wallet: WalletServiceData,
    RuntimeServiceId: Debug + Display + Sync + AsServiceId<Wallet>,
{
    for UtxoWithKeyId { utxo, key_id } in utxos {
        let public_inputs = public_inputs_for_slot(epoch_state, slot, latest_tree);
        let winning = kms
            .check_winning_with_key(key_id.clone(), utxo, &public_inputs)
            .await;
        if winning {
            tracing::debug!(
                "leader for slot {:?}, {:?}/{:?}",
                slot,
                utxo.note.value,
                epoch_state.total_stake()
            );

            let voucher_cm = match wallet.generate_new_voucher().await {
                Ok(voucher_cm) => voucher_cm,
                Err(e) => {
                    tracing::error!("Failed to generate voucher: {e:?}");
                    continue;
                }
            };

            let private_inputs_result = kms
                .build_private_inputs_for_winning_utxo_and_slot(
                    key_id.clone(),
                    utxo,
                    epoch_state,
                    public_inputs,
                    latest_tree,
                )
                .await;
            let (private_inputs, leader_signing_key) = match private_inputs_result {
                Ok(result) => result,
                Err(e) => {
                    tracing::error!(
                        "Failed to build private inputs for winning utxo {:?} for {slot:?}: {e:?}",
                        utxo.id(),
                    );
                    continue;
                }
            };

            let res = tokio::task::spawn_blocking(move || {
                Groth16LeaderProof::prove(private_inputs, voucher_cm)
            })
            .await;
            match res {
                Ok(Ok(proof)) => return Some((proof, leader_signing_key)),
                Ok(Err(e)) => {
                    tracing::error!("Failed to build proof: {:?}", e);
                }
                Err(e) => {
                    tracing::error!("Failed to wait thread to build proof: {:?}", e);
                }
            }
        } else {
            tracing::trace!(
                "Not a leader for slot {:?}, {:?}/{:?}",
                slot,
                utxo.note.value,
                epoch_state.total_stake()
            );
        }
    }

    None
}

pub fn operator_for_private_inputs_arguments_for_winning_utxo_and_slot(
    utxo: &Utxo,
    epoch_state: &EpochState,
    public_inputs: LeaderPublic,
    latest_tree: &UtxoTree,
) -> Result<
    (
        BuildPrivateInputsWithLeaderKey,
        oneshot::Receiver<LeaderPrivate>,
        Ed25519Key,
    ),
    PrivateInputsError,
> {
    let (sender, receiver) = oneshot::channel();
    let aged_path = epoch_state
        .utxo_merkle_path(utxo)
        .ok_or(PrivateInputsError::AgedNoteNotFound)?;
    let latest_path = latest_tree
        .path(&utxo.id())
        .ok_or(PrivateInputsError::LatestNoteNotFound)?;
    // Generate a random one-time Ed25519 key for P_LEAD (as per PoL spec)
    let leader_signing_key = Ed25519Key::generate(&mut OsRng);
    let leader_pk = leader_signing_key.public_key();

    Ok((
        BuildPrivateInputsWithLeaderKey::new(
            sender,
            *utxo,
            public_inputs,
            aged_path,
            latest_path,
            leader_pk,
        ),
        receiver,
        leader_signing_key,
    ))
}

fn public_inputs_for_slot(
    epoch_state: &EpochState,
    slot: Slot,
    latest_tree: &UtxoTree,
) -> LeaderPublic {
    LeaderPublic::new(
        epoch_state.utxo_merkle_root(),
        latest_tree.root(),
        epoch_state.nonce,
        slot.into(),
        epoch_state.lottery_0,
        epoch_state.lottery_1,
    )
}

#[derive(thiserror::Error, Debug)]
pub enum PrivateInputsError {
    #[error("Aged note not found from merkle tree")]
    AgedNoteNotFound,
    #[error("Latest note not found from merkle tree")]
    LatestNoteNotFound,
}

/// The per-epoch chain state needed to check winning slots, shared by the
/// per-slot block-proposal path and the per-epoch winning-slot scan.
///
/// These inputs are all fixed for the whole epoch: `epoch_state` (including the
/// aged UTXO tree) and the wallet's leader-eligible notes (aged at the end of
/// the previous epoch). The block-proposal path additionally needs the *latest*
/// ledger state (to prove a note is still unspent), which it fetches separately
/// per slot; the Blend winning-slot scan does not, since the leadership quota
/// proof only attests that a note was aged, not that it is unspent.
pub struct SlotContext {
    pub tip: HeaderId,
    pub epoch_state: EpochState,
    pub eligible_aged: Vec<UtxoWithKeyId>,
}

/// Per-subscriber background task that streams an epoch's winning slots to a
/// Blend subscriber.
///
/// On subscribe it starts at the *current* slot and scans to the end of the
/// current epoch (skipping past slots, so a mid-epoch start wastes no work),
/// then keeps scanning each subsequent epoch in full. For each epoch it hands
/// the subscriber a fresh bounded `mpsc` stream and fills it lazily; the
/// bounded channel applies backpressure so the whole epoch is never
/// materialized at once. The task exits when the subscriber drops its stream.
#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: address this in a dedicated refactor"
)]
pub async fn search_for_winning_slots<CryptarchiaService, Wallet, RuntimeServiceId>(
    cryptarchia_api: CryptarchiaServiceApi<CryptarchiaService, RuntimeServiceId>,
    wallet_api: WalletApi<Wallet, RuntimeServiceId>,
    kms: KmsServiceApi<PreloadKmsService<RuntimeServiceId>, RuntimeServiceId>,
    time_relay: OutboundRelay<TimeServiceMessage>,
    ledger_config: lb_ledger::Config,
    epoch_handoff_sender: mpsc::Sender<WinningPolEpochSlots>,
) where
    CryptarchiaService: CryptarchiaServiceData<Tx: Send + Sync>,
    Wallet: WalletServiceData,
    RuntimeServiceId: AsServiceId<Wallet>
        + AsServiceId<PreloadKmsService<RuntimeServiceId>>
        + Debug
        + Display
        + Send
        + Sync
        + 'static,
{
    // Subscribe to future slot ticks (used to detect epoch boundaries) and read
    // the current slot to start scanning from immediately.
    let Some(mut slot_timer) = async {
        let (sender, receiver) = oneshot::channel();
        time_relay
            .send(TimeServiceMessage::Subscribe { sender })
            .await
            .ok()?;
        receiver.await.ok()
    }
    .await
    else {
        tracing::error!(target: LOG_TARGET, "Failed to subscribe to slot ticks; winning slots subscriber cannot run.");
        return;
    };

    // Process one epoch at a time, starting from whichever slot is current when
    // we subscribe. Each iteration handles a single epoch; the `tokio::select!`
    // at the end yields the first tick of the next epoch to process, or `None`
    // when the tick stream ends (which ends the loop).
    let mut current_slot_tick = slot_timer.next().await;
    while let Some(SlotTick { slot, epoch }) = current_slot_tick {
        let Some(slot_context) =
            fetch_slot_context(&cryptarchia_api, &wallet_api, &ledger_config, slot).await
        else {
            tracing::debug!(target: LOG_TARGET, "Could not fetch slot context for slot {slot:?}; retrying on the next tick.");
            current_slot_tick = slot_timer.next().await;
            continue;
        };

        let (winning_slots_sender, winning_slots_receiver) =
            mpsc::channel(WINNING_SLOT_BUFFER_SIZE);
        let winning_slots_stream: WinningPolSlotStream =
            Box::pin(ReceiverStream::new(winning_slots_receiver));
        if epoch_handoff_sender
            .send(WinningPolEpochSlots {
                epoch,
                slots: winning_slots_stream,
            })
            .await
            .is_err()
        {
            tracing::debug!(target: LOG_TARGET, "Winning slots subscriber dropped its handoff stream; exiting.");
            return;
        }

        // Scan this epoch's winning slots — over the aged UTXO tree, since the
        // leadership quota proof only attests that a note was aged at the end of
        // the previous epoch, not that it is unspent — concurrently with watching
        // for the epoch to roll over. If the epoch changes first the now-stale
        // scan is abandoned; if the scan finishes first the stream stays open
        // until the epoch changes.
        let epoch_winning_slot_compute_task = scan_epoch_winning_slots(
            &ledger_config,
            &slot_context.epoch_state,
            &slot_context.eligible_aged,
            &slot_context.epoch_state.utxos,
            &kms,
            slot,
            &winning_slots_sender,
        );
        let next_epoch = next_epoch_tick(&mut slot_timer, epoch);
        tokio::pin!(epoch_winning_slot_compute_task, next_epoch);
        // We poll both next epoch and the scan task, so that we can stop the scan task
        // if a new epoch starts without stopping the main loop.
        current_slot_tick = tokio::select! {
            () = &mut epoch_winning_slot_compute_task => (&mut next_epoch).await,
            tick = &mut next_epoch => tick,
        };
    }

    tracing::trace!(target: LOG_TARGET, "Slot tick stream ended; winning slots subscriber exiting.");
}

/// Awaits the first slot tick belonging to an epoch other than `epoch` (i.e.
/// the first tick of the next epoch), returning it, or `None` if the tick
/// stream ends.
async fn next_epoch_tick(
    slot_timer: &mut EpochSlotTickStream,
    current_epoch: Epoch,
) -> Option<SlotTick> {
    loop {
        match slot_timer.next().await {
            Some(tick) if tick.epoch > current_epoch => return Some(tick),
            Some(_) => {}
            None => return None,
        }
    }
}

/// Fetches the [`SlotContext`] for `slot` from the tip: the tip header, the
/// slot's epoch state, and the wallet's eligible leader UTXOs (with the faucet
/// UTXO filtered out). Returns `None` if any lookup fails.
pub async fn fetch_slot_context<CryptarchiaService, Wallet, RuntimeServiceId>(
    cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService, RuntimeServiceId>,
    wallet_api: &WalletApi<Wallet, RuntimeServiceId>,
    ledger_config: &lb_ledger::Config,
    slot: Slot,
) -> Option<SlotContext>
where
    CryptarchiaService: CryptarchiaServiceData<Tx: Send + Sync>,
    Wallet: WalletServiceData,
    RuntimeServiceId: AsServiceId<Wallet> + Debug + Display + Sync,
{
    let tip = cryptarchia_api.info().await.ok()?.cryptarchia_info.tip;
    let epoch_state = cryptarchia_api.get_epoch_state(slot).await.ok()?.ok()?;
    let eligible_utxos = wallet_api.get_leader_aged_notes(Some(tip)).await.ok()?;
    let eligible = match &ledger_config.faucet_pk {
        Some(faucet_pk) => eligible_utxos
            .response
            .into_iter()
            .filter(|utxo| utxo.utxo.note.pk != *faucet_pk)
            .collect(),
        None => eligible_utxos.response,
    };
    Some(SlotContext {
        tip,
        epoch_state,
        eligible_aged: eligible,
    })
}

/// Iterates the slots of an epoch from `start_slot` to the epoch's last slot,
/// sending the leadership private inputs of each winning slot to the
/// subscriber. `send().await` applies backpressure, so the scan pauses rather
/// than racing ahead of the consumer (and never collects the whole epoch).
#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: address this in a dedicated refactor"
)]
async fn scan_epoch_winning_slots<RuntimeServiceId>(
    ledger_config: &lb_ledger::Config,
    epoch_state: &EpochState,
    eligible_utxos: &[UtxoWithKeyId],
    latest_tree: &UtxoTree,
    kms: &(impl KmsAdapter<RuntimeServiceId, KeyId = KeyId> + Sync),
    start_slot: Slot,
    slot_tx: &mpsc::Sender<LeaderPrivate>,
) {
    let slots_per_epoch = ledger_config.epoch_length();
    let epoch_first_slot: u64 = ledger_config
        .epoch_config
        .starting_slot(&epoch_state.epoch, ledger_config.base_period_length())
        .into();
    let epoch_last_slot = epoch_first_slot
        .checked_add(slots_per_epoch)
        .expect("Epoch slot calculation overflow.")
        - 1;
    // Skip slots earlier than the start slot: a mid-epoch subscriber does not
    // waste work on slots it has already passed.
    let scan_starting_slot = u64::from(start_slot).max(epoch_first_slot);

    for slot in scan_starting_slot..=epoch_last_slot {
        let public_inputs = public_inputs_for_slot(epoch_state, slot.into(), latest_tree);
        for UtxoWithKeyId { utxo, key_id } in eligible_utxos {
            if !kms
                .check_winning_with_key(key_id.clone(), utxo, &public_inputs)
                .await
            {
                continue;
            }
            tracing::trace!(target: LOG_TARGET, "Found winning utxo with ID {:?} for slot {slot}", utxo.id());

            match kms
                .build_private_inputs_for_winning_utxo_and_slot(
                    key_id.clone(),
                    utxo,
                    epoch_state,
                    public_inputs,
                    latest_tree,
                )
                .await
            {
                Ok((leader_private, _)) => {
                    if slot_tx.send(leader_private).await.is_err() {
                        tracing::debug!(
                            target: LOG_TARGET,
                            "Winning slots subscriber dropped its stream; stopping scan of epoch {:?}.",
                            epoch_state.epoch
                        );
                        return;
                    }
                }
                Err(e) => {
                    tracing::error!(
                        target: LOG_TARGET, "Failed to build private inputs for winning utxo {:?} at slot {slot}: {e:?}",
                        utxo.id(),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod pol_tests {
    use core::fmt;
    use std::{fmt::Formatter, num::NonZero, slice, sync::Arc};

    use lb_core::{
        mantle::{
            ledger::{Inputs, Note, Outputs},
            ops::{leader_claim::VoucherCm, transfer::TransferOp},
        },
        proofs::leader_proof::{LeaderProof as _, check_winning},
        sdp::{MinStake, ServiceParameters, ServiceType},
    };
    use lb_cryptarchia_engine::EpochConfig;
    use lb_groth16::{Fr, fr_from_bytes_unchecked};
    use lb_key_management_system_service::keys::{UnsecuredZkKey, ZkKey};
    use lb_ledger::mantle::sdp::{
        Config as SdpConfig, ServiceRewardsParameters, rewards::blend::RewardsParameters,
    };
    use lb_utils::math::{NonNegativeF64, NonNegativeRatio};
    use lb_wallet_service::{WalletMsg, WalletServiceSettings};
    use overwatch::services::{
        ServiceData,
        state::{NoOperator, NoState},
    };

    use super::*;

    /// Test that [`Leader::build_proof_for`] generates `PoL` which can be
    /// verified successfully.
    #[tokio::test]
    async fn test_build_proof_for() {
        let config = test_config();

        // Create secret key and leader
        let kms = DummyKms;
        let key_id = KeyId::from("0");
        let sk = UnsecuredZkKey::new(Fr::from(0u64));
        let pk = sk.to_public_key();

        // Create a UTXO
        let transfer = TransferOp::new(Inputs::empty(), Outputs::new([Note::new(1000u64, pk)]));
        let utxo = transfer.outputs.utxo_by_index(0, &transfer).unwrap();

        // Create aged/latest UTXO trees
        let aged_tree = UtxoTree::new().insert(utxo.id(), utxo).0;
        let latest_tree = UtxoTree::new().insert(utxo.id(), utxo).0;

        // Create EpochState
        let total_stake = utxo.note.value;
        let (lottery_0, lottery_1) = config
            .lottery_constants()
            .compute_lottery_values(total_stake);
        let epoch_state = EpochState {
            epoch: 1.into(),
            nonce: Fr::from(999u64),
            utxos: aged_tree.clone(),
            total_stake,
            lottery_0,
            lottery_1,
            active_declarations: Arc::new(lb_core::sdp::Declarations::default()),
        };

        // Create dummy wallet service
        let wallet = DummyWallet::spawn();

        // Find a winning slot by calling `build_proof_for` until it succeeds
        let (proof, winning_slot) = find_winning_slot_and_build_proof(
            (0..1000).map(Slot::from),
            UtxoWithKeyId { utxo, key_id },
            &epoch_state,
            &latest_tree,
            &wallet,
            &kms,
        )
        .await
        .expect("should find a winning slot and build a proof");
        assert_eq!(proof.voucher_cm(), &dummy_voucher_cm());

        // Verify proof
        let public_inputs = LeaderPublic::new(
            aged_tree.root(),
            latest_tree.root(),
            epoch_state.nonce,
            winning_slot.into(),
            epoch_state.lottery_0,
            epoch_state.lottery_1,
        );
        assert!(
            proof.verify(&public_inputs),
            "proof verification should succeed"
        );
    }

    /// Find a winning slot by calling `build_proof_for` until it succeeds
    async fn find_winning_slot_and_build_proof(
        slots: impl Iterator<Item = Slot>,
        utxo: UtxoWithKeyId,
        epoch_state: &EpochState,
        latest_tree: &UtxoTree,
        wallet: &WalletApi<DummyWallet, TestRuntimeServiceId>,
        kms: &(impl KmsAdapter<TestRuntimeServiceId, KeyId = KeyId> + Sync),
    ) -> Option<(Groth16LeaderProof, Slot)> {
        for slot in slots {
            if let Some((proof, _signing_key)) = build_proof_for(
                slice::from_ref(&utxo),
                latest_tree,
                epoch_state,
                slot,
                wallet,
                kms,
            )
            .await
            {
                return Some((proof, slot));
            }
        }
        None
    }

    /// Build an [`EpochState`] and a winning UTXO for `scan` tests.
    fn scan_test_fixtures() -> (
        lb_ledger::Config,
        DummyKms,
        Vec<UtxoWithKeyId>,
        UtxoTree,
        EpochState,
    ) {
        let config = test_config();
        let kms = DummyKms;
        let key_id = KeyId::from("0");
        let sk = UnsecuredZkKey::new(Fr::from(0u64));
        let pk = sk.to_public_key();

        let transfer = TransferOp::new(Inputs::empty(), Outputs::new([Note::new(1000u64, pk)]));
        let utxo = transfer.outputs.utxo_by_index(0, &transfer).unwrap();

        let aged_tree = UtxoTree::new().insert(utxo.id(), utxo).0;
        let latest_tree = UtxoTree::new().insert(utxo.id(), utxo).0;

        let total_stake = utxo.note.value;
        let (lottery_0, lottery_1) = config
            .lottery_constants()
            .compute_lottery_values(total_stake);
        let epoch_state = EpochState {
            epoch: 1.into(),
            nonce: Fr::from(999u64),
            utxos: aged_tree,
            total_stake,
            lottery_0,
            lottery_1,
            active_declarations: Arc::new(lb_core::sdp::Declarations::default()),
        };

        (
            config,
            kms,
            vec![UtxoWithKeyId { utxo, key_id }],
            latest_tree,
            epoch_state,
        )
    }

    /// The scan only emits winning slots within `[start_slot, epoch_end)`: it
    /// skips past slots (so a mid-epoch start wastes no work) and never runs
    /// off the end of the epoch.
    #[tokio::test]
    async fn scan_emits_only_slots_in_range() {
        let (config, kms, eligible, latest_tree, epoch_state) = scan_test_fixtures();

        let epoch_starting_slot: u64 = config
            .epoch_config
            .starting_slot(&epoch_state.epoch, config.base_period_length())
            .into();
        let epoch_end = epoch_starting_slot + config.epoch_length();
        let start_slot = epoch_starting_slot + config.epoch_length() / 2;

        // A buffer large enough to hold the whole second half avoids blocking.
        let (slot_tx, mut slot_rx) = mpsc::channel(config.epoch_length() as usize + 1);
        scan_epoch_winning_slots(
            &config,
            &epoch_state,
            &eligible,
            &latest_tree,
            &kms,
            start_slot.into(),
            &slot_tx,
        )
        .await;
        drop(slot_tx);

        let mut count = 0usize;
        while let Some(leader_private) = slot_rx.recv().await {
            let slot = leader_private.input().chain.slot_number;
            assert!(
                slot >= start_slot && slot < epoch_end,
                "winning slot {slot} outside [{start_slot}, {epoch_end})",
            );
            count += 1;
        }
        // With the easy test lottery (f = 1) and a mid-epoch start, there is at
        // least one winning slot to emit.
        assert!(count > 0, "expected at least one winning slot in range");
    }

    /// Starting the scan at the epoch's end emits nothing.
    #[tokio::test]
    async fn scan_past_epoch_end_emits_nothing() {
        let (config, kms, eligible, latest_tree, epoch_state) = scan_test_fixtures();

        let epoch_starting_slot: u64 = config
            .epoch_config
            .starting_slot(&epoch_state.epoch, config.base_period_length())
            .into();
        let epoch_end = epoch_starting_slot + config.epoch_length();

        let (slot_tx, mut slot_rx) = mpsc::channel(8);
        scan_epoch_winning_slots(
            &config,
            &epoch_state,
            &eligible,
            &latest_tree,
            &kms,
            epoch_end.into(),
            &slot_tx,
        )
        .await;
        drop(slot_tx);

        assert!(
            slot_rx.recv().await.is_none(),
            "no winning slots should be emitted when starting past the epoch end",
        );
    }

    fn test_config() -> lb_ledger::Config {
        lb_ledger::Config {
            epoch_config: EpochConfig {
                epoch_stake_distribution_stabilization: NonZero::new(3u8).unwrap(),
                epoch_period_nonce_buffer: NonZero::new(3).unwrap(),
                epoch_period_nonce_stabilization: NonZero::new(4).unwrap(),
            },
            consensus_config: lb_cryptarchia_engine::Config::new(
                NonZero::new(5).unwrap(),
                NonNegativeRatio::new(1, 10.try_into().unwrap()),
                1f64.try_into().expect("1 > 0"),
            ),
            sdp_config: SdpConfig {
                service_params: Arc::new(
                    [(
                        ServiceType::BlendNetwork,
                        ServiceParameters {
                            inactivity_period: 20.try_into().unwrap(),
                            retention_period: 100.into(),
                            epoch: 0.into(),
                        },
                    )]
                    .into(),
                ),
                service_rewards_params: ServiceRewardsParameters {
                    blend: RewardsParameters {
                        rounds_per_epoch: NonZero::new(10u64).unwrap(),
                        message_frequency_per_round: NonNegativeF64::try_from(1.0).unwrap(),
                        num_blend_layers: NonZero::new(3u64).unwrap(),
                        minimum_network_size: NonZero::new(1u64).unwrap(),
                        data_replication_factor: 0,
                        activity_threshold_sensitivity: 1,
                    },
                },
                min_stake: MinStake {
                    threshold: 1,
                    timestamp: 0,
                },
            },
            faucet_pk: None,
        }
    }

    struct DummyKms;

    #[async_trait::async_trait]
    impl KmsAdapter<TestRuntimeServiceId> for DummyKms {
        type KeyId = KeyId;

        async fn check_winning_with_key(
            &self,
            _: Self::KeyId,
            utxo: &Utxo,
            leader_public: &LeaderPublic,
        ) -> bool {
            let sk = ZkKey::new(Fr::from(0u64));
            check_winning(*utxo, *leader_public, &sk.to_public_key(), Fr::from(0u64))
        }

        async fn build_private_inputs_for_winning_utxo_and_slot(
            &self,
            _: Self::KeyId,
            utxo: &Utxo,
            epoch_state: &EpochState,
            public_inputs: LeaderPublic,
            latest_tree: &UtxoTree,
        ) -> Result<(LeaderPrivate, Ed25519Key), PrivateInputsError> {
            let aged_path = epoch_state
                .utxo_merkle_path(utxo)
                .ok_or(PrivateInputsError::AgedNoteNotFound)?;
            let latest_path = latest_tree
                .path(&utxo.id())
                .ok_or(PrivateInputsError::LatestNoteNotFound)?;
            // Generate a random one-time Ed25519 key for P_LEAD (as per PoL spec)
            let leader_signing_key = Ed25519Key::generate(&mut OsRng);
            let leader_pk = leader_signing_key.public_key();
            let leader_private = LeaderPrivate::new(
                public_inputs,
                *utxo,
                &aged_path,
                &latest_path,
                Fr::from(0u64),
                &leader_pk,
            );
            Ok((leader_private, leader_signing_key))
        }
    }

    struct DummyWallet;

    impl ServiceData for DummyWallet {
        type Settings = WalletServiceSettings;
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = WalletMsg;
    }

    impl WalletServiceData for DummyWallet {
        type Kms = ();
        type Cryptarchia = ();
        type Tx = ();
        type Storage = ();
    }

    impl DummyWallet {
        fn spawn() -> WalletApi<Self, TestRuntimeServiceId> {
            let (msg_sender, mut msg_receiver) = mpsc::channel(10);

            tokio::spawn(async move {
                while let Some(msg) = msg_receiver.recv().await {
                    if let WalletMsg::GenerateNewVoucherSecret { resp_tx } = msg {
                        let _ = resp_tx.send(dummy_voucher_cm());
                    }
                }
            });

            WalletApi::<Self, TestRuntimeServiceId>::new(OutboundRelay::new(msg_sender))
        }
    }

    const DUMMY_VOUCHER_CM_BYTES: [u8; 32] = [99u8; 32];

    fn dummy_voucher_cm() -> VoucherCm {
        fr_from_bytes_unchecked(&DUMMY_VOUCHER_CM_BYTES).into()
    }

    #[derive(Debug)]
    struct TestRuntimeServiceId;

    impl AsServiceId<DummyWallet> for TestRuntimeServiceId {
        const SERVICE_ID: Self = Self;
    }

    impl Display for TestRuntimeServiceId {
        fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
            write!(f, "TestRuntimeServiceId")
        }
    }
}
