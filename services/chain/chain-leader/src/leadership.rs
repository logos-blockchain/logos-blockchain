use std::{
    fmt::{Debug, Display},
    sync::Arc,
    time::Instant,
};

use futures::{StreamExt as _, stream};
use lb_chain_service::{
    EpochStateQueryResult,
    api::{CryptarchiaServiceApi, CryptarchiaServiceData},
};
use lb_core::{
    header::HeaderId,
    mantle::note_set::NoteSets,
    proofs::leader_proof::{LeaderPublic, LeaderWitnessError, pol_wallet_inputs},
    sdp::blend::{PolEpochState, PolEpochStateSource},
};
use lb_cryptarchia_engine::{Epoch, Slot};
use lb_groth16::{AdditiveGroup as _, Fr};
use lb_key_management_system_service::{api::KmsServiceApi, backend::preload::KeyId};
use lb_ledger::{
    ConsensusExcludedKeys, EpochState,
    config::{EraScheduledConfig, config_for_epoch},
};
use lb_log_targets::{chain, diagnostic::BLEND_REACHABILITY};
use lb_pol::PolWalletInputsData;
use lb_time_service::{EpochSlotTickStream, SlotTick, TimeServiceMessage};
use lb_wallet_service::{
    AgedNote,
    api::{WalletApi, WalletServiceData},
};
use overwatch::services::{AsServiceId, relay::OutboundRelay};
#[cfg(test)]
pub use pol_tests::{single_era, test_config};
use tokio::sync::{mpsc, oneshot};

use crate::{
    WinningPolEpochSlots, WinningPolSlotStream, WinningSlotFuture,
    kms::{KmsAdapter, PreloadKmsService},
    metrics,
};

const LOG_TARGET: &str = chain::leader::LEADERSHIP;

// An aged note of the wallet winning the slot, with the public inputs it wins.
#[derive(Debug)]
pub struct WinningNote {
    pub note: AgedNote,
    pub public_inputs: LeaderPublic,
}

// The first aged note winning `slot`, checked against C_LEAD and the latest
// eligible root.
pub async fn find_winning_note<RuntimeServiceId>(
    aged: &[AgedNote],
    epoch_state: &EpochState,
    latest: &NoteSets,
    slot: Slot,
    kms: &(impl KmsAdapter<RuntimeServiceId, KeyId = KeyId> + Sync),
) -> Option<WinningNote> {
    let (mut non_winning_notes, start) = (0usize, Instant::now());
    let public_inputs = public_inputs_for_slot(epoch_state, slot, latest);
    for note in aged {
        match kms
            .check_winning_with_key(note.key_id.clone(), &note.note.note, &public_inputs)
            .await
        {
            Ok(true) => {
                tracing::debug!(
                    target: LOG_TARGET,
                    "leader for slot {:?}, {:?}/{:?}",
                    slot,
                    note.note.note.value,
                    epoch_state.total_stake()
                );
                return Some(WinningNote {
                    note: AgedNote {
                        note: note.note.clone(),
                        aged_path: note.aged_path.clone(),
                        key_id: note.key_id.clone(),
                    },
                    public_inputs,
                });
            }
            Ok(false) => non_winning_notes += 1,
            Err(e) => {
                metrics::consensus_proposals_create_failed("leadership_check");
                tracing::error!(
                    target: LOG_TARGET,
                    "Failed to check winning note {:?} for {slot:?}: {e:?}",
                    note.note.note.commitment(),
                );
            }
        }
    }

    tracing::trace!(
        target: LOG_TARGET,
        "Leadership scan completed in {:.2?} - slot: {}, eligible: {}, winning: 0, \
        non winning: {non_winning_notes}",
        start.elapsed(),
        slot.into_inner(),
        aged.len(),
    );

    None
}

// The wallet half of the PoL witness of the winning note, proven against the
// aged note sets of the epoch and the `latest` note sets.
pub fn leader_wallet_inputs(
    winning: &WinningNote,
    epoch_state: &EpochState,
    latest: &NoteSets,
) -> Result<PolWalletInputsData, PrivateInputsError> {
    let note = &winning.note.note;
    Ok(pol_wallet_inputs(
        &note.note,
        note.note_set,
        &note.nullifier,
        &winning.note.aged_path,
        &epoch_state.note_sets,
        latest,
    )?)
}

// C_LEAD and eligible_LATEST.
pub fn public_inputs_for_slot(
    epoch_state: &EpochState,
    slot: Slot,
    latest: &NoteSets,
) -> LeaderPublic {
    LeaderPublic::new(
        epoch_state.eligible_root(),
        latest.eligible_root(),
        epoch_state.nonce,
        slot.into(),
        epoch_state.lottery_0,
        epoch_state.lottery_1,
    )
}

#[derive(thiserror::Error, Debug)]
pub enum PrivateInputsError {
    #[error("Leader witness error: {0}")]
    Witness(#[from] LeaderWitnessError),
    #[error("KMS API error: {0}")]
    KmsApi(#[from] overwatch::DynError),
    #[error("KMS API did not respond")]
    KmsResponse,
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
    /// Tip explicitly passed to `get_leader_aged_notes` for the wallet query.
    pub wallet_tip: HeaderId,
    pub epoch_state: EpochState,
    pub eligible_aged: Vec<AgedNote>,
    /// Tip/LIB provenance of the chain-derived epoch state.
    pub source: PolEpochStateSource,
}

/// Per-subscriber background task that hands one lazy winning-slot stream per
/// epoch.
///
/// On subscribe it starts at the *current* slot and, for each epoch, hands the
/// subscriber a lazy [`WinningPolSlotStream`] over that epoch's slot range
/// (from the current slot for the ongoing epoch — so a mid-epoch start wastes
/// no work — and in full for each later epoch). The stream is lazy: no slot is
/// scanned until the subscriber drives it, and the subscriber decides how far
/// ahead to pre-compute (e.g. via the `Buffered` adapter), so the whole epoch
/// is never materialized here. This task only produces the cheap per-epoch
/// handoffs; it exits when the subscriber drops its stream.
#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: address this in a dedicated refactor"
)]
pub async fn search_for_winning_slots<CryptarchiaService, Wallet, RuntimeServiceId>(
    cryptarchia_api: CryptarchiaServiceApi<CryptarchiaService>,
    wallet_api: WalletApi<Wallet, RuntimeServiceId>,
    kms: KmsServiceApi<PreloadKmsService<RuntimeServiceId>, RuntimeServiceId>,
    time_relay: OutboundRelay<TimeServiceMessage>,
    era_schedule: Arc<EraScheduledConfig>,
    consensus_excluded_keys: ConsensusExcludedKeys,
    epoch_handoff_sender: mpsc::Sender<WinningPolEpochSlots>,
) where
    CryptarchiaService: CryptarchiaServiceData<Tx: Send>,
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
            .send(TimeServiceMessage::SubscribeToEpochSlots { sender })
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
    while let Some(SlotTick { slot, epoch, .. }) = current_slot_tick {
        let Some(slot_context) = fetch_slot_context(
            &cryptarchia_api,
            &wallet_api,
            &consensus_excluded_keys,
            slot,
        )
        .await
        else {
            tracing::debug!(target: LOG_TARGET, "Could not fetch slot context for slot {slot:?}; retrying on the next tick.");
            current_slot_tick = slot_timer.next().await;
            continue;
        };

        let SlotContext {
            wallet_tip,
            epoch_state,
            eligible_aged,
            source,
        } = slot_context;
        let state = PolEpochState {
            nonce: epoch_state.nonce,
            eligible_aged: epoch_state.eligible_root(),
            lottery_0: epoch_state.lottery_0,
            lottery_1: epoch_state.lottery_1,
            source,
        };
        tracing::debug!(
            target: LOG_TARGET,
            diagnostic = BLEND_REACHABILITY,
            event = "pol_epoch_state_frozen",
            epoch = u32::from(epoch),
            slot = u64::from(slot),
            wallet_tip_id = %wallet_tip,
            source_tip_id = %state.source.tip_id,
            source_tip_slot = u64::from(state.source.tip_slot),
            source_lib_id = %state.source.lib_id,
            source_lib_slot = u64::from(state.source.lib_slot),
            wallet_tip_matches_epoch_state_source = wallet_tip == state.source.tip_id,
            nonce = ?state.nonce,
            eligible_aged = ?state.eligible_aged,
            lottery_0 = ?state.lottery_0,
            lottery_1 = ?state.lottery_1,
            "Frozen ChainLeader epoch state for winning PoL slots"
        );

        // Hand the subscriber a *lazy* stream over this epoch's slot range. No
        // slot is scanned until the subscriber drives the stream, and it decides
        // how far ahead to pre-compute, so the whole epoch is never materialized
        // here. A scan made stale by an epoch rollover is implicitly abandoned:
        // the subscriber just stops polling it once it moves to the next epoch.
        let winning_slots_stream = epoch_winning_slots_stream(
            &era_schedule,
            epoch_state,
            &eligible_aged,
            kms.clone(),
            slot,
        );
        if epoch_handoff_sender
            .send(WinningPolEpochSlots {
                epoch,
                state,
                slots: winning_slots_stream,
            })
            .await
            .is_err()
        {
            tracing::debug!(target: LOG_TARGET, "Winning slots subscriber dropped its handoff stream; exiting.");
            return;
        }

        // Wait for the first tick of the next epoch to produce a new winning slot
        // stream and pass it to consumers.
        current_slot_tick = next_epoch_tick(&mut slot_timer, epoch).await;
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
/// slot's epoch state, and the wallet's eligible leader UTXOs (with the
/// consensus-excluded UTXOs filtered out). Returns `None` if any lookup fails.
pub async fn fetch_slot_context<CryptarchiaService, Wallet, RuntimeServiceId>(
    cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService>,
    wallet_api: &WalletApi<Wallet, RuntimeServiceId>,
    consensus_excluded_keys: &ConsensusExcludedKeys,
    slot: Slot,
) -> Option<SlotContext>
where
    CryptarchiaService: CryptarchiaServiceData<Tx: Send>,
    Wallet: WalletServiceData,
    RuntimeServiceId: AsServiceId<Wallet> + Debug + Display + Sync,
{
    let wallet_tip = cryptarchia_api.info().await.ok()?.cryptarchia_info.tip;
    let EpochStateQueryResult {
        epoch_state,
        source_tip_id,
        source_tip_slot,
        source_lib_id,
        source_lib_slot,
        ..
    } = cryptarchia_api
        .get_epoch_state_with_source(slot)
        .await
        .ok()?
        .ok()?;
    let eligible_utxos = wallet_api
        .get_leader_aged_notes(Some(wallet_tip), epoch_state.epoch)
        .await
        .ok()?;
    let eligible = eligible_utxos
        .response
        .into_iter()
        .filter(|note| !consensus_excluded_keys.contains(&note.note.note.public_key))
        .collect();
    Some(SlotContext {
        wallet_tip,
        epoch_state,
        eligible_aged: eligible,
        source: PolEpochStateSource {
            tip_id: source_tip_id,
            tip_slot: source_tip_slot,
            lib_id: source_lib_id,
            lib_slot: source_lib_slot,
        },
    })
}

/// Builds a *lazy* stream of one epoch's per-slot leadership-proof work: one
/// [`WinningSlotFuture`] per slot from `start_slot` to the epoch's last slot.
///
/// The stream does no work until polled. Each item is a future that, when
/// driven, performs the KMS lottery check for that slot and — on a win — builds
/// the leadership private inputs, resolving to `Some(LeaderPrivate)` for a
/// winning slot or `None` otherwise. The consumer drives the futures (and
/// decides how far ahead to pre-compute, e.g. via the `Buffered` adapter), so
/// the whole epoch is never materialized at once.
///
/// The winning check uses the aged UTXO tree (`epoch_state.utxos`), since the
/// leadership quota proof only attests that a note was aged at the end of the
/// previous epoch, not that it is unspent. Slots earlier than `start_slot` are
/// skipped so a mid-epoch subscriber wastes no work.
fn epoch_winning_slots_stream<RuntimeServiceId>(
    era_schedule: &EraScheduledConfig,
    epoch_state: EpochState,
    eligible_aged: &[AgedNote],
    kms: impl KmsAdapter<RuntimeServiceId, KeyId = KeyId> + Send + Sync + 'static,
    start_slot: Slot,
) -> WinningPolSlotStream {
    let slots_per_epoch = config_for_epoch(era_schedule, epoch_state.epoch).epoch_length();
    let epoch_first_slot: u64 = era_schedule
        .starting_slot_for_epoch(epoch_state.epoch)
        .into();
    let epoch_last_slot = epoch_first_slot
        .checked_add(slots_per_epoch)
        .expect("Epoch slot calculation overflow.")
        - 1;
    // Skip slots earlier than the start slot: a mid-epoch subscriber does not
    // waste work on slots it has already passed.
    let scan_starting_slot = u64::from(start_slot).max(epoch_first_slot);

    // Share the read-only per-epoch inputs across all per-slot futures.
    // `AgedNote` is not `Clone`, so collect owned copies of its fields.
    let epoch_state = Arc::new(epoch_state);
    let eligible_aged: Arc<Vec<AgedNote>> = Arc::new(
        eligible_aged
            .iter()
            .map(|note| AgedNote {
                note: note.note.clone(),
                aged_path: note.aged_path.clone(),
                key_id: note.key_id.clone(),
            })
            .collect(),
    );
    let kms = Arc::new(kms);

    let stream = stream::iter(scan_starting_slot..=epoch_last_slot).map(move |slot| {
        let epoch_state = Arc::clone(&epoch_state);
        let eligible_aged = Arc::clone(&eligible_aged);
        let kms = Arc::clone(&kms);
        let is_slot_winning_task: WinningSlotFuture = Box::pin(async move {
            // Only the aged half of the witness reaches the PoQ, so the latest
            // note sets are the aged ones and the msg is 0.
            let latest = &epoch_state.note_sets;
            let public_inputs = public_inputs_for_slot(&epoch_state, slot.into(), latest);
            for note in eligible_aged.iter() {
                let winning = match kms
                    .check_winning_with_key(note.key_id.clone(), &note.note.note, &public_inputs)
                    .await
                {
                    Ok(winning) => winning,
                    Err(e) => {
                        tracing::error!(
                            target: LOG_TARGET,
                            "Failed to check winning note {:?} at slot {slot}: {e:?}",
                            note.note.note.commitment(),
                        );
                        continue;
                    }
                };
                if !winning {
                    continue;
                }
                tracing::trace!(target: LOG_TARGET, "Found winning note {:?} for slot {slot}", note.note.note.commitment());
                let wallet = match pol_wallet_inputs(
                    &note.note.note,
                    note.note.note_set,
                    &note.note.nullifier,
                    &note.aged_path,
                    &epoch_state.note_sets,
                    latest,
                ) {
                    Ok(wallet) => wallet,
                    Err(e) => {
                        tracing::error!(
                            target: LOG_TARGET,
                            "Failed to build the witness of winning note {:?} at slot {slot}: {e:?}",
                            note.note.note.commitment(),
                        );
                        continue;
                    }
                };
                match kms
                    .build_private_inputs(note.key_id.clone(), public_inputs, wallet, Fr::ZERO)
                    .await
                {
                    Ok(leader_private) => return Some(leader_private),
                    Err(e) => tracing::error!(
                        target: LOG_TARGET,
                        "Failed to build private inputs for winning note {:?} at slot {slot}: {e:?}",
                        note.note.note.commitment(),
                    ),
                }
            }
            None
        });
        is_slot_winning_task
    });

    Box::pin(stream)
}

#[cfg(test)]
mod pol_tests {
    use core::fmt;
    use std::{fmt::Formatter, num::NonZero};

    use lb_core::{
        mantle::{
            Note,
            note_set::{CommitmentMmr, LEDGER_SET},
            notes::derive_note_nullifier,
        },
        proofs::leader_proof::{LeaderPrivate, check_winning},
        sdp::{MinStake, ServiceParameters, ServiceType},
    };
    use lb_cryptarchia_engine::EpochConfig;
    use lb_key_management_system_service::keys::{UnsecuredZkKey, ZkKey};
    use lb_ledger::{
        LedgerState,
        config::{BlendPoWConfig, ModulusShift, PoWConfig, RewardPoWConfig},
        mantle::sdp::{
            Config as SdpConfig, ServiceRewardsParameters, rewards::blend::RewardsParameters,
        },
    };
    use lb_time::era::{EpochLength, EraEntriesAfterGenesis, EraSchedule, EraTiming};
    use lb_utils::math::{NonNegativeRatio, PositiveF64};
    use lb_wallet::OwnedNote;

    use super::*;

    /// `config` as the only era, from genesis.
    pub fn single_era(config: lb_ledger::Config) -> EraScheduledConfig {
        EraSchedule::new(
            time::OffsetDateTime::UNIX_EPOCH,
            EraTiming {
                slot_duration: core::time::Duration::from_secs(1),
                epoch_length: EpochLength::epoch_length(&config),
            },
            EraEntriesAfterGenesis::empty(),
        )
        .expect("a single era resolves")
        .map(move |_| config.clone())
    }

    /// Build an [`EpochState`] and a winning UTXO for `scan` tests.
    fn scan_test_fixtures() -> (
        lb_ledger::Config,
        DummyKms,
        Vec<AgedNote>,
        NoteSets,
        EpochState,
    ) {
        let config = test_config();
        let kms = DummyKms;
        let key_id = KeyId::from("0");
        let sk = UnsecuredZkKey::new(Fr::from(0u64));
        let pk = sk.to_public_key();

        let note = Note::new(1000u64, Fr::from(11u64), pk);
        let (_, aged_path) = CommitmentMmr::new()
            .push_with_paths(note.commitment(), &mut [])
            .unwrap();
        let note_sets =
            LedgerState::from_notes([note], &config, 0, &ConsensusExcludedKeys::empty())
                .latest_note_sets()
                .clone();

        let total_stake = note.value;
        let (lottery_0, lottery_1) = config
            .lottery_constants()
            .compute_lottery_values(total_stake);
        let epoch_state = EpochState {
            epoch: 1.into(),
            nonce: Fr::from(999u64),
            blend_pow_difficulty: Fr::from(0u64),
            note_sets: note_sets.clone(),
            total_stake,
            lottery_0,
            lottery_1,
            active_declarations: Arc::new(lb_core::sdp::Declarations::default()),
        };

        let owned = OwnedNote {
            note,
            note_set: LEDGER_SET,
            nullifier: derive_note_nullifier(&note.commitment(), sk.as_fr()),
            path: aged_path.clone(),
        };
        (
            config,
            kms,
            vec![AgedNote {
                note: owned,
                aged_path,
                key_id,
            }],
            note_sets,
            epoch_state,
        )
    }

    /// The scan only emits winning slots within `[start_slot, epoch_end)`: it
    /// skips past slots (so a mid-epoch start wastes no work) and never runs
    /// off the end of the epoch.
    #[tokio::test]
    async fn scan_emits_only_slots_in_range() {
        let (config, kms, eligible, _, epoch_state) = scan_test_fixtures();
        let eras = single_era(config.clone());

        let epoch_starting_slot: u64 = eras.starting_slot_for_epoch(epoch_state.epoch).into();
        let epoch_end = epoch_starting_slot + config.epoch_length();
        let start_slot = epoch_starting_slot + config.epoch_length() / 2;

        // Drive every per-slot future and keep the winning ones.
        let winners: Vec<_> =
            epoch_winning_slots_stream(&eras, epoch_state, &eligible, kms, start_slot.into())
                .filter_map(|winning_slot| winning_slot)
                .collect()
                .await;

        for leader_private in &winners {
            let slot = leader_private.input().chain.slot_number;
            assert!(
                slot >= start_slot && slot < epoch_end,
                "winning slot {slot} outside [{start_slot}, {epoch_end})",
            );
        }
        // With the easy test lottery (f = 1) and a mid-epoch start, there is at
        // least one winning slot to emit.
        assert!(
            !winners.is_empty(),
            "expected at least one winning slot in range"
        );
    }

    /// Starting the scan at the epoch's end emits nothing.
    #[tokio::test]
    async fn scan_past_epoch_end_emits_nothing() {
        let (config, kms, eligible, _, epoch_state) = scan_test_fixtures();
        let eras = single_era(config.clone());

        let epoch_starting_slot: u64 = eras.starting_slot_for_epoch(epoch_state.epoch).into();
        let epoch_end = epoch_starting_slot + config.epoch_length();

        let winners: Vec<_> =
            epoch_winning_slots_stream(&eras, epoch_state, &eligible, kms, epoch_end.into())
                .filter_map(|winning_slot| winning_slot)
                .collect()
                .await;

        assert!(
            winners.is_empty(),
            "no winning slots should be emitted when starting past the epoch end",
        );
    }

    /// A reward config with claiming disabled, standing in for a real
    /// deployment config in tests.
    fn disabled_reward_config() -> RewardPoWConfig {
        RewardPoWConfig {
            minimum_difficulty: ModulusShift::new::<26>(),
            ema_smoothing_factor: 9,
            ema_smoothing_precision: core::num::NonZeroU64::new(10).unwrap(),
            target_claims_per_block: 100,
            rate_num: 0,
            rate_den: core::num::NonZeroU64::MIN,
            target_claim_per_block: core::num::NonZeroU64::MIN,
            pow_share: 0,
            share_den: core::num::NonZeroU64::MIN,
            slot_window: core::num::NonZeroU64::new(100).unwrap(),
        }
    }

    pub fn test_config() -> lb_ledger::Config {
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
                12,
                4,
            ),
            sdp_config: SdpConfig {
                service_params: Arc::new(
                    [(
                        ServiceType::BlendNetwork,
                        ServiceParameters {
                            inactivity_period: 20.try_into().unwrap(),
                            epoch: 0.into(),
                        },
                    )]
                    .into(),
                ),
                service_rewards_params: ServiceRewardsParameters {
                    blend: RewardsParameters {
                        rounds_per_epoch: NonZero::new(10u64).unwrap(),
                        message_frequency_per_round: PositiveF64::try_from(1.0).unwrap(),
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
            pow_config: PoWConfig {
                blend: BlendPoWConfig {
                    base_difficulty: ModulusShift::new::<19>(),
                    damping_den_offset: 0,
                    damping_num: 1.try_into().unwrap(),
                    max_step: 1.try_into().unwrap(),
                    target_transactions_per_block: 1.try_into().unwrap(),
                },
                reward: disabled_reward_config(),
            },
        }
    }

    struct DummyKms;

    #[async_trait::async_trait]
    impl KmsAdapter<TestRuntimeServiceId> for DummyKms {
        type KeyId = KeyId;

        async fn check_winning_with_key(
            &self,
            _: Self::KeyId,
            note: &Note,
            leader_public: &LeaderPublic,
        ) -> Result<bool, overwatch::DynError> {
            let sk = ZkKey::new(Fr::from(0u64));
            Ok(check_winning(
                note,
                *leader_public,
                &sk.to_public_key(),
                Fr::from(0u64),
            ))
        }

        async fn build_private_inputs(
            &self,
            _: Self::KeyId,
            public_inputs: LeaderPublic,
            wallet: PolWalletInputsData,
            msg: Fr,
        ) -> Result<LeaderPrivate, PrivateInputsError> {
            let wallet = PolWalletInputsData {
                secret_key: Fr::from(0u64),
                ..wallet
            };
            Ok(LeaderPrivate::new(public_inputs, wallet, msg))
        }
    }

    #[derive(Debug)]
    struct TestRuntimeServiceId;

    impl Display for TestRuntimeServiceId {
        fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
            write!(f, "TestRuntimeServiceId")
        }
    }
}
