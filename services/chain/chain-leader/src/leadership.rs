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
use lb_key_management_system_service::api::KmsServiceApi;
use lb_ledger::{
    EpochState,
    config::{EraScheduledConfig, config_for_epoch},
};
use lb_log_targets::{chain, diagnostic::BLEND_REACHABILITY};
use lb_pol::PolWalletInputsData;
use lb_time_service::{EpochSlotTickStream, SlotTick, TimeServiceMessage};
use lb_utils::tokio::task::spawn_blocking;
use lb_wallet_service::{
    AgedNote, LeadershipKeyIds as KeyId,
    api::{WalletApi, WalletServiceData},
};
use overwatch::{
    DynError,
    services::{AsServiceId, relay::OutboundRelay},
};
#[cfg(test)]
pub use pol_tests::{genesis_parameters, single_era, test_config};
use tokio::sync::{mpsc, oneshot};

use crate::{
    WinningPolEpochSlots, WinningPolSlotStream, WinningSlotFuture,
    kms::{KmsAdapter, PreloadKmsService},
    metrics,
};

const LOG_TARGET: &str = chain::leader::LEADERSHIP;

// An aged note of the wallet winning the slot, with the public inputs it wins
// and its wallet witness.
#[derive(Debug)]
pub struct WinningNote {
    pub note: AgedNote,
    pub public_inputs: LeaderPublic,
    pub wallet_inputs: PolWalletInputsData,
}

// The first aged note winning `slot` whose witness can be built, checked
// against C_LEAD and the latest eligible root.
#[expect(
    clippy::cognitive_complexity,
    reason = "The lottery check and the witness of each note, with a log for each outcome"
)]
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
                // A winning note whose witness fails, such as one spent since
                // the epoch snapshot, leaves the slot to the next winning note.
                let Some(wallet_inputs) =
                    leader_wallet_inputs(note, epoch_state, latest, slot).await
                else {
                    continue;
                };
                return Some(WinningNote {
                    note: AgedNote {
                        note: note.note.clone(),
                        aged_path: note.aged_path.clone(),
                        key_id: note.key_id.clone(),
                    },
                    public_inputs,
                    wallet_inputs,
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

// The wallet half of the PoL witness of an aged note, proven against the aged
// note sets of the epoch and the `latest` note sets, or `None` if it fails.
async fn leader_wallet_inputs(
    aged: &AgedNote,
    epoch_state: &EpochState,
    latest: &NoteSets,
    slot: Slot,
) -> Option<PolWalletInputsData> {
    pol_wallet_inputs_off_loop(aged, &epoch_state.note_sets, latest)
        .await
        .inspect_err(|e| {
            metrics::consensus_proposals_create_failed("private_inputs");
            tracing::error!(
                target: LOG_TARGET,
                "Failed to build the witness of winning note {:?} for {slot:?}: {e}",
                aged.note.note.commitment(),
            );
        })
        .ok()
}

// The wallet half of the PoL witness of `aged`, proven against the `aged_sets`
// and the `latest` note sets. Its paths are read from the nullifier store, so
// it's built on the blocking pool.
async fn pol_wallet_inputs_off_loop(
    aged: &AgedNote,
    aged_sets: &NoteSets,
    latest: &NoteSets,
) -> Result<PolWalletInputsData, DynError> {
    let note = aged.note.clone();
    let aged_path = aged.aged_path.clone();
    let (aged_sets, latest) = (aged_sets.clone(), latest.clone());
    spawn_blocking("logos/chain/leader-witness-blocking", move || {
        pol_wallet_inputs(
            &note.note,
            note.note_set,
            &note.nullifier,
            &aged_path,
            &aged_sets,
            &latest,
        )
    })
    .await
    .map_err(DynError::from)?
    .map_err(DynError::from)
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
    KmsApi(#[from] DynError),
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
/// per won slot; the Blend winning-slot scan does not, since the leadership
/// quota proof only attests that a note was aged, not that it is unspent.
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
/// ahead to pre-compute, so the whole epoch is never materialized here. This
/// task only produces the cheap per-epoch handoffs; it exits when the
/// subscriber drops its stream.
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
        let Some(slot_context) = fetch_slot_context(&cryptarchia_api, &wallet_api, slot).await
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
        tracing::debug!(
            target: LOG_TARGET,
            event = "leader_eligible_notes",
            path = "winning_slot_scan",
            slot = ?slot,
            epoch = ?epoch,
            note_count = eligible_aged.len(),
            total_value = eligible_aged.iter().map(|note| note.note.note.value).sum::<u64>(),
            "Eligible notes for the winning slot scan"
        );
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
        // slot is scanned until the subscriber drives the stream, and it
        // decides how far ahead to pre-compute, so the whole epoch is
        // never materialized here. A scan made stale by an epoch
        // rollover is implicitly abandoned: the subscriber just stops
        // polling it once it moves to the next epoch.
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

        // Wait for the first tick of the next epoch to produce a new winning
        // slot stream and pass it to consumers.
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
/// slot's epoch state, and the wallet's eligible leader UTXOs. Returns `None`
/// if any lookup fails.
pub async fn fetch_slot_context<CryptarchiaService, Wallet, RuntimeServiceId>(
    cryptarchia_api: &CryptarchiaServiceApi<CryptarchiaService>,
    wallet_api: &WalletApi<Wallet, RuntimeServiceId>,
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
    // Filter the wallet's notes by the aged UTXO set of the slot's epoch, not
    // the one of the tip's epoch: on the first slots of an epoch, before any
    // block of it is on the chain, the two differ.
    let eligible_utxos = wallet_api
        .get_leader_aged_notes(Some(wallet_tip), epoch_state.epoch)
        .await
        .ok()?;
    Some(SlotContext {
        wallet_tip,
        epoch_state,
        eligible_aged: eligible_utxos.response,
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
/// decides how far ahead to pre-compute), so the whole epoch is never
/// materialized at once.
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
                let wallet = match pol_wallet_inputs_off_loop(note, &epoch_state.note_sets, latest)
                    .await
                {
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
                    .build_private_inputs(note.key_id.clone(), note.note.note, public_inputs, wallet, 0)
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
    use std::{
        fmt::Formatter,
        num::{NonZero, NonZeroU32},
    };

    use lb_core::{
        era::{AcceptedForkDigests, ForkDigest},
        mantle::{
            GenesisParameters, GenesisTime, Note, Value,
            gas::{Gas, GasProfile},
            note_set::{CommitmentMmr, NoteSetIndex},
            notes::derive_note_nullifier,
            ops::channel::ChannelId,
        },
        proofs::leader_proof::{LeaderPrivate, check_winning},
        sdp::{MinStake, ServiceParameters, ServiceType},
    };
    use lb_cryptarchia_engine::EpochConfig;
    use lb_groth16::Fr;
    use lb_key_management_system_service::keys::{UnsecuredZkKey, ZkKey};
    use lb_ledger::{
        LedgerState,
        config::{BlendPoWConfig, ModulusShift, PoWConfig, RewardPoWConfig},
        mantle::sdp::{
            Config as SdpConfig, ServiceRewardsParameters, rewards::blend::RewardsParameters,
        },
    };
    use lb_time::era::{EpochLength, EraEntriesAfterGenesis, EraSchedule, EraTiming};
    use lb_utils::math::{NonNegativeRatio, PositiveRatio, ProperFraction};
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
        let key_id = KeyId {
            nullifier: "0".into(),
            staking: "0".into(),
        };
        let sk = UnsecuredZkKey::new(Fr::from(0u64));
        let pk = sk.to_public_key();

        let note = Note::new(1000u64, Fr::from(11u64), pk);
        let (_, aged_path) = CommitmentMmr::new()
            .push_with_paths(note.commitment(), &mut [])
            .unwrap();
        let note_sets = LedgerState::from_notes([note], &config, &genesis_parameters())
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
            note_set: NoteSetIndex::Ledger,
            nullifier: derive_note_nullifier(&note.commitment(), &sk.nullifier_key()),
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

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "Both consumers and the frozen-epoch spend use the real KMS"
    )]
    async fn both_leadership_consumers_use_the_same_distinct_kms_credentials() {
        use lb_core::proofs::leader_proof::Groth16LeaderProof;
        use lb_key_management_system_service::{
            backend::{
                KMSBackend as _,
                preload::{PreloadKMSBackend, PreloadKMSBackendSettings},
            },
            keys::{Key, ZkKey, kdf_public_key},
            message::KMSMessage,
        };

        let (config, _, _, _, mut epoch_state) = scan_test_fixtures();
        let nf_sk = Fr::from(7u64);
        let staking_sk = Fr::from(19u64);
        let note = Note {
            stake_public_key: kdf_public_key(&staking_sk),
            ..Note::new(1000, Fr::from(11u64), kdf_public_key(&nf_sk))
        };
        let (_, aged_path) = CommitmentMmr::new()
            .push_with_paths(note.commitment(), &mut [])
            .unwrap();
        let latest = LedgerState::from_notes([note], &config, &genesis_parameters())
            .latest_note_sets()
            .clone();
        epoch_state.note_sets = latest.clone();
        let key_id = KeyId {
            nullifier: "leader-nf".into(),
            staking: "leader-stake".into(),
        };
        let aged = vec![AgedNote {
            note: OwnedNote {
                note,
                note_set: NoteSetIndex::Ledger,
                nullifier: derive_note_nullifier(&note.commitment(), &nf_sk),
                path: aged_path.clone(),
            },
            aged_path,
            key_id: key_id.clone(),
        }];
        let backend = PreloadKMSBackend::new(PreloadKMSBackendSettings {
            keys: [
                (
                    key_id.nullifier.clone(),
                    Key::Zk(ZkKey::from_direct_secret(nf_sk)),
                ),
                (
                    key_id.staking.clone(),
                    Key::Zk(ZkKey::from_direct_secret(staking_sk)),
                ),
            ]
            .into(),
        });
        let (sender, mut receiver) = mpsc::channel(8);
        tokio::spawn(async move {
            while let Some(message) = receiver.recv().await {
                if let KMSMessage::ExecuteMultiple {
                    key_ids,
                    operator,
                    reply_channel,
                } = message
                {
                    drop(reply_channel.send(backend.execute_multiple(&key_ids, operator).await));
                }
            }
        });
        let kms =
            KmsServiceApi::<PreloadKmsService<TestRuntimeServiceId>, TestRuntimeServiceId>::new(
                OutboundRelay::new(sender),
            );
        let eras = single_era(config);
        let start = eras.starting_slot_for_epoch(epoch_state.epoch);
        let mut scan =
            epoch_winning_slots_stream(&eras, epoch_state.clone(), &aged, kms.clone(), start);
        let scanned = loop {
            let future = scan
                .next()
                .await
                .expect("the easy lottery has a winning slot");
            if let Some(private) = future.await {
                break private;
            }
        };
        let slot = Slot::from(scanned.input().chain.slot_number);
        let scanned_public = public_inputs_for_slot(&epoch_state, slot, &latest);
        let scan_proof = Groth16LeaderProof::prove(scanned).unwrap();
        assert!(scan_proof.verify(&scanned_public, Fr::from(0u64)));

        // PoQ uses the frozen epoch inputs even after a later spend. A block
        // proposal must instead construct an unspent witness at the live root.
        let spent = latest
            .clone()
            .update_ledger(
                latest
                    .ledger()
                    .clone()
                    .execute_spending(&[aged[0].note.nullifier])
                    .unwrap(),
            )
            .end_block(2, 1024)
            .unwrap();
        let mut after_spend =
            epoch_winning_slots_stream(&eras, epoch_state.clone(), &aged, kms.clone(), slot);
        assert!(after_spend.next().await.unwrap().await.is_some());
        assert!(
            find_winning_note(&aged, &epoch_state, &spent, slot, &kms)
                .await
                .is_none()
        );

        let winning = find_winning_note(&aged, &epoch_state, &latest, slot, &kms)
            .await
            .unwrap();
        let private = kms
            .build_private_inputs(
                winning.note.key_id,
                note,
                winning.public_inputs,
                winning.wallet_inputs,
                0,
            )
            .await
            .unwrap();
        let proposal_proof = Groth16LeaderProof::prove(private).unwrap();
        assert!(proposal_proof.verify(&winning.public_inputs, Fr::from(0u64)));
    }

    /// A winning note whose witness fails leaves the slot to the next winning
    /// note.
    #[tokio::test]
    async fn a_winning_note_without_a_witness_leaves_the_slot_to_the_next() {
        let (_, kms, eligible, note_sets, epoch_state) = scan_test_fixtures();
        let good = &eligible[0];
        let slot = slot_won_by(good, &epoch_state, &note_sets, &kms).await;
        let aged = [in_unknown_set(good), clone_aged(good)];

        let winning = find_winning_note(&aged, &epoch_state, &note_sets, slot, &kms)
            .await
            .expect("the second note wins the slot");
        assert_eq!(winning.note.aged_path, good.aged_path);
    }

    /// No winning note leads a slot if none has a witness.
    #[tokio::test]
    async fn no_note_wins_a_slot_without_a_witness() {
        let (_, kms, eligible, note_sets, epoch_state) = scan_test_fixtures();
        let slot = slot_won_by(&eligible[0], &epoch_state, &note_sets, &kms).await;
        let aged = [in_unknown_set(&eligible[0])];

        let winning = find_winning_note(&aged, &epoch_state, &note_sets, slot, &kms).await;
        assert!(winning.is_none());
    }

    /// The first slot `aged` wins.
    async fn slot_won_by(
        aged: &AgedNote,
        epoch_state: &EpochState,
        note_sets: &NoteSets,
        kms: &DummyKms,
    ) -> Slot {
        for slot in 0..1000u64 {
            let public_inputs = public_inputs_for_slot(epoch_state, slot.into(), note_sets);
            if kms
                .check_winning_with_key(aged.key_id.clone(), &aged.note.note, &public_inputs)
                .await
                .unwrap()
            {
                return slot.into();
            }
        }
        panic!("the note wins no slot of the first 1000");
    }

    /// `aged` in a channel set the chain doesn't hold: it still wins, but its
    /// witness fails.
    fn in_unknown_set(aged: &AgedNote) -> AgedNote {
        let mut aged = clone_aged(aged);
        aged.note.note_set = NoteSetIndex::Channel(ChannelId::from([7; 32]));
        aged
    }

    fn clone_aged(aged: &AgedNote) -> AgedNote {
        AgedNote {
            note: aged.note.clone(),
            aged_path: aged.aged_path.clone(),
            key_id: aged.key_id.clone(),
        }
    }

    /// A reward config with claiming disabled, standing in for a real
    /// deployment config in tests.
    fn disabled_reward_config() -> RewardPoWConfig {
        RewardPoWConfig {
            minimum_difficulty: ModulusShift::new::<26>(),
            ema_smoothing_factor: 9,
            ema_smoothing_precision: core::num::NonZeroU64::new(10).unwrap(),
            target_claims_per_block: core::num::NonZeroU64::new(100).unwrap(),
            rate_num: 0,
            rate_den: core::num::NonZeroU64::MIN,
            pow_share: 0,
            share_den: core::num::NonZeroU64::MIN,
            slot_window: core::num::NonZeroU64::new(100).unwrap(),
        }
    }

    /// The costs of the deployments' current gas profile.
    const GAS_PROFILE: GasProfile = GasProfile {
        transfer: Gas::new(10),
        program_register: Gas::new(15114),
        channel_config: Gas::new(24),
        channel_inscribe: Gas::new(26),
        channel_deposit: Gas::new(26),
        channel_withdraw: Gas::new(33),
        sdp_declare: Gas::new(1693),
        sdp_withdraw: Gas::new(0),
        sdp_active: Gas::new(15117),
        claim_pow_reward: Gas::new(1506),
        kilobyte: Gas::new(30),
        note_set: Gas::new(8961),
        proof: Gas::new(4177),
        root: Gas::new(2332),
        buffered: Gas::new(60),
        nullifier: Gas::new(1758),
        commitment: Gas::new(58),
        signature: Gas::new(557),
        user_step: Gas::new(28),
        program_deposit: Gas::new(144),
        program_step: Gas::new(13528),
        opening: Gas::new(470),
    };

    /// The genesis parameters of a test chain: the supply cap and the genesis
    /// gas prices of the deployments.
    pub fn genesis_parameters() -> GenesisParameters {
        GenesisParameters {
            chain_id: "test".to_owned().try_into().unwrap(),
            start_time: GenesisTime::new(0),
            epoch_nonce: Fr::from(0u64),
            max_supply: 10_000_000_000_000_000_000,
            pow_reward_pool: 0,
            rewards_reserve: 1_000_000_000_000_000_000,
            storage_gas_price: 1_000_000_000.try_into().unwrap(),
            execution_gas_price: 1_000_000_000.try_into().unwrap(),
        }
    }

    /// The block rewards of the specification, on the supply cap of the
    /// deployments and a block every 30 seconds.
    const BLOCK_REWARDS: lb_ledger::config::BlockRewardsConfig =
        lb_ledger::config::BlockRewardsConfig::new(
            lb_ledger::config::BlockRewardsParameters {
                max_emission_rate: ProperFraction::new(1, NonZero::new(100).unwrap()).unwrap(),
                min_emission_rate: NonNegativeRatio::new(0, NonZero::new(1).unwrap()),
                stake_target: ProperFraction::new(3, NonZero::new(10).unwrap()).unwrap(),
                pooled_fees_target: NonNegativeRatio::new(1, NonZero::new(1).unwrap()),
                stake_deviation_weight: NonNegativeRatio::new(1, NonZero::new(4).unwrap()),
                pooled_fees_weight: NonNegativeRatio::new(1, NonZero::new(1).unwrap()),
                pooled_fees_window: NonZero::new(120).unwrap(),
                blend_share: ProperFraction::new(6, NonZero::new(10).unwrap()).unwrap(),
            },
            10_000_000_000_000_000_000,
            NonZero::new(1).unwrap(),
            ProperFraction::new(1, NonZero::new(30).unwrap()).unwrap(),
        )
        .unwrap();

    pub fn test_config() -> lb_ledger::Config {
        lb_ledger::Config {
            epoch_config: EpochConfig {
                epoch_stake_distribution_stabilization: NonZero::new(3u8).unwrap(),
                epoch_period_nonce_buffer: NonZero::new(3).unwrap(),
                epoch_period_nonce_stabilization: NonZero::new(4).unwrap(),
            },
            consensus_config: lb_cryptarchia_engine::Config::new(
                NonZero::new(5).unwrap(),
                ProperFraction::new(1, 10.try_into().unwrap()).unwrap(),
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
                        },
                    )]
                    .into(),
                ),
                service_rewards_params: ServiceRewardsParameters {
                    blend: RewardsParameters {
                        rounds_per_epoch: NonZero::new(10u64).unwrap(),
                        message_frequency_per_round: PositiveRatio::new(1, NonZeroU32::MIN)
                            .unwrap(),
                        cover_replication_factor: 0,
                        num_blend_layers: NonZero::new(3u64).unwrap(),
                        minimum_network_size: NonZero::new(1u64).unwrap(),
                        data_replication_factor: 0,
                        activity_threshold_sensitivity: 1,
                    },
                },
                min_stake: MinStake { threshold: 1 },
            },
            max_transactions: 1024,
            max_transactions_size: 2 * 1024 * 1024,
            max_op_data_size: 2 * 1024 * 1024 * 7 / 8,
            root_window: 1024,
            channel_withdraw_delay: 86_400.into(),
            execution_market: lb_ledger::config::ExecutionMarketConfig {
                max_gas_per_block: lb_ledger::config::MaxGasPerBlock::new(3_137_798).unwrap(),
                fee_adjustment_rate: ProperFraction::new(1, 8.try_into().unwrap()).unwrap(),
                ema_smoothing_factor: ProperFraction::new(9, 10.try_into().unwrap()).unwrap(),
            },
            storage_market: lb_ledger::config::StorageMarketConfig {
                max_adjustment_factor: ProperFraction::new(1, 8.try_into().unwrap()).unwrap(),
                ema_smoothing_factor: ProperFraction::new(1, 2.try_into().unwrap()).unwrap(),
            },
            block_rewards: BLOCK_REWARDS,
            gas_profile: GAS_PROFILE,
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
            accepted_fork_digests: AcceptedForkDigests::new(vec![ForkDigest::new([0x11; 32])]),
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
        ) -> Result<bool, DynError> {
            let nullifier_key = ZkKey::new(Fr::from(0u64)).nullifier_key();
            Ok(check_winning(
                note,
                *leader_public,
                nullifier_key,
                nullifier_key,
            ))
        }

        async fn build_private_inputs(
            &self,
            _: Self::KeyId,
            _: Note,
            public_inputs: LeaderPublic,
            wallet: PolWalletInputsData,
            reward_value: Value,
        ) -> Result<LeaderPrivate, PrivateInputsError> {
            let nullifier_key = ZkKey::new(Fr::from(0u64)).nullifier_key();
            let wallet = PolWalletInputsData {
                nullifier_key,
                staking_secret_key: nullifier_key,
                ..wallet
            };
            Ok(LeaderPrivate::new(
                public_inputs,
                wallet,
                Fr::from(0u64),
                reward_value,
            ))
        }
    }

    #[derive(Debug)]
    struct TestRuntimeServiceId;

    impl AsServiceId<PreloadKmsService<Self>> for TestRuntimeServiceId {
        const SERVICE_ID: Self = Self;
    }

    impl Display for TestRuntimeServiceId {
        fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
            write!(f, "TestRuntimeServiceId")
        }
    }
}
