use std::{collections::HashSet, sync::Arc, time::SystemTime};

use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};
use lb_core::{era::ForkDigest, header::HeaderId};
use lb_cryptarchia_engine::{EraSchedule, Slot};
use lb_ledger::LedgerState;
use lb_services_utils::overwatch::RecoveryData;
use lb_storage_service::{
    KeyValuePair,
    recovery::{StorageRecoverySettings as _, recovery_key},
};
use lb_utils::tokio::task::spawn_blocking;
use overwatch::{DynError, services::state::ServiceState};
use serde::{Deserialize, Serialize};

use crate::{Cryptarchia, CryptarchiaSettings, Error, StartingState};

// About an hour at the testnet template's one-second slots and 1/30 activation.
const CHECKPOINT_INTERVAL_BLOCKS: u64 = 120;
pub const RECOVERY_PROGRESS_KEY_SUFFIX: &[u8] = b"cryptarchia-progress-v1";

/// The slot of the chain's tip, and the fork digest its era has under the
/// schedule the tip was applied with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipEra {
    pub slot: Slot,
    pub fork_digest: ForkDigest,
}

impl TipEra {
    fn at(slot: Slot, fork_digests: &EraSchedule<ForkDigest>) -> Self {
        Self {
            slot,
            fork_digest: *fork_digests.at_slot(slot).parameters,
        }
    }
}

/// The last saved tip era, read without consuming the recovery data the
/// chain service will load when it starts.
///
/// # Errors
///
/// If the saved state cannot be read or decoded.
pub fn saved_tip_era(recovery_data: &RecoveryData) -> Result<Option<TipEra>, DynError> {
    let decode_error = |error| -> DynError {
        format!(
            "The saved chain state cannot be read ({error}). Remove the node's state and sync \
             again."
        )
        .into()
    };

    if let Some(bytes) = recovery_data.get(&recovery_key(RECOVERY_PROGRESS_KEY_SUFFIX))? {
        let progress = RecoveryProgress::from_bytes(&bytes).map_err(decode_error)?;
        return Ok(Some(progress.tip_era));
    }

    let Some(bytes) = recovery_data.get(&recovery_key(CryptarchiaSettings::RECOVERY_KEY_SUFFIX))?
    else {
        return Ok(None);
    };

    let state = CryptarchiaConsensusState::from_bytes(&bytes).map_err(decode_error)?;
    Ok(Some(state.tip_era))
}

/// Full LIB checkpoint. Its field layout matches existing node databases.
#[derive(Clone, Serialize, Deserialize)]
pub struct CryptarchiaConsensusState {
    pub(crate) tip: HeaderId,
    pub(crate) tip_era: TipEra,
    pub(crate) lib: HeaderId,
    pub(crate) lib_ledger_state: LedgerState,
    pub(crate) lib_block_length: u64,
    pub(crate) lib_block_slot: Slot,
    /// The uncle slots in the LIB header.
    /// These are unlikely to be used for future uncle selections,
    /// given the depth of LIB. But, we save them because any blocks
    /// in cryptarchia-engine must be restored to the same state as before
    /// shutdown.
    pub(crate) lib_block_uncle_slots: lb_cryptarchia_engine::UncleSlots,
    pub(crate) genesis_id: HeaderId,
    /// Set of blocks that have been pruned from the engine but have not yet
    /// been deleted from the persistence layer because of some unexpected
    /// error.
    pub(crate) storage_blocks_to_remove: HashSet<HeaderId>,
    /// Last engine state and timestamp for offline grace period tracking
    pub(crate) last_engine_state: Option<LastEngineState>,
}

impl CryptarchiaConsensusState {
    pub(crate) fn load(settings: &CryptarchiaSettings) -> Result<Self, Error> {
        match settings
            .recovery_data
            .take(&recovery_key(CryptarchiaSettings::RECOVERY_KEY_SUFFIX))
            .map_err(|error| Error::Storage(error.to_string()))?
        {
            Some(bytes) => Ok(Self::from_bytes(&bytes)?),
            None => Self::from_settings(settings),
        }
    }

    #[must_use]
    pub const fn tip(&self) -> HeaderId {
        self.tip
    }

    /// Re-create the [`CryptarchiaConsensusState`]
    /// given the cryptarchia engine and ledger state.
    ///
    /// Furthermore, it allows to specify blocks deleted from the cryptarchia
    /// engine (hence not tracked anymore) but that should be deleted from the
    /// persistence layer.
    pub(crate) fn from_cryptarchia_and_unpruned_blocks(
        cryptarchia: &Cryptarchia,
        storage_blocks_to_remove: HashSet<HeaderId>,
        fork_digests: &EraSchedule<ForkDigest>,
    ) -> Result<Self, Error> {
        let lib = cryptarchia.consensus.lib_branch();
        let Some(lib_ledger_state) = cryptarchia.ledger.state(&lib.id()).cloned() else {
            return Err(Error::Storage(format!(
                "Ledger state for LIB {} is missing",
                lib.id()
            )));
        };
        let lib_block_length = lib.length();
        let lib_block_slot = lib.slot();
        let lib_block_uncle_slots = lib.uncle_slots().clone();

        let tip = cryptarchia.consensus.tip_branch();
        Ok(Self {
            tip: tip.id(),
            tip_era: TipEra::at(tip.slot(), fork_digests),
            lib: lib.id(),
            genesis_id: cryptarchia.genesis_id,
            lib_ledger_state,
            lib_block_length,
            lib_block_slot,
            lib_block_uncle_slots,
            storage_blocks_to_remove,
            last_engine_state: Some(LastEngineState {
                timestamp: SystemTime::now(),
                state: *cryptarchia.consensus.state(),
            }),
        })
    }
}

impl ServiceState for CryptarchiaConsensusState {
    type Settings = CryptarchiaSettings;
    type Error = Error;

    fn from_settings(
        settings: &<Self as ServiceState>::Settings,
    ) -> Result<Self, <Self as ServiceState>::Error> {
        let StartingState::Genesis { genesis_block } = &settings.starting_state;
        let genesis_id = genesis_block.header().id();
        let (genesis_config, genesis_fork_digest) = settings.era_schedule.genesis().parameters;
        let (lib_ledger_state, _events) =
            LedgerState::from_genesis_tx(genesis_block.genesis_tx().clone(), genesis_config)?;

        Ok(Self {
            tip: genesis_id,
            tip_era: TipEra {
                slot: Slot::genesis(),
                fork_digest: *genesis_fork_digest,
            },
            lib: genesis_id,
            lib_ledger_state,
            lib_block_length: 0,
            lib_block_slot: Slot::default(),
            lib_block_uncle_slots: lb_cryptarchia_engine::UncleSlots::default(),
            genesis_id,
            storage_blocks_to_remove: HashSet::new(),
            last_engine_state: None,
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct LastEngineState {
    pub timestamp: SystemTime,
    pub state: lb_cryptarchia_engine::State,
}

#[derive(Serialize, Deserialize)]
pub struct RecoveryProgress {
    pub(crate) tip: HeaderId,
    tip_era: TipEra,
    pub(crate) lib: HeaderId,
    pub(crate) lib_block_length: u64,
    pub(crate) storage_blocks_to_remove: HashSet<HeaderId>,
    pub(crate) last_engine_state: Option<LastEngineState>,
}

impl RecoveryProgress {
    pub(crate) fn from_checkpoint(checkpoint: &CryptarchiaConsensusState) -> Self {
        Self {
            tip: checkpoint.tip,
            tip_era: checkpoint.tip_era,
            lib: checkpoint.lib,
            lib_block_length: checkpoint.lib_block_length,
            storage_blocks_to_remove: checkpoint.storage_blocks_to_remove.clone(),
            last_engine_state: checkpoint.last_engine_state.clone(),
        }
    }

    pub(crate) fn load(
        checkpoint: &CryptarchiaConsensusState,
        recovery_data: &RecoveryData,
    ) -> Result<Self, Error> {
        let Some(bytes) = recovery_data
            .take(&recovery_key(RECOVERY_PROGRESS_KEY_SUFFIX))
            .map_err(|error| Error::Storage(error.to_string()))?
        else {
            return Ok(Self::from_checkpoint(checkpoint));
        };

        Ok(Self::from_bytes(&bytes)?)
    }

    fn from_chain(
        cryptarchia: &Cryptarchia,
        storage_blocks_to_remove: HashSet<HeaderId>,
        fork_digests: &EraSchedule<ForkDigest>,
    ) -> Self {
        let tip = cryptarchia.tip_branch();

        Self {
            tip: tip.id(),
            tip_era: TipEra::at(tip.slot(), fork_digests),
            lib: cryptarchia.lib(),
            lib_block_length: cryptarchia.lib_branch().length(),
            storage_blocks_to_remove,
            last_engine_state: Some(LastEngineState {
                timestamp: SystemTime::now(),
                state: *cryptarchia.state(),
            }),
        }
    }
}

pub struct RecoveryRecorder {
    checkpoint_lib_height: Option<u64>,
    fork_digests: Arc<EraSchedule<ForkDigest>>,
}

impl RecoveryRecorder {
    pub(crate) fn new(
        checkpoint: &CryptarchiaConsensusState,
        fork_digests: Arc<EraSchedule<ForkDigest>>,
    ) -> Self {
        Self {
            checkpoint_lib_height: checkpoint
                .last_engine_state
                .is_some()
                .then_some(checkpoint.lib_block_length),
            fork_digests,
        }
    }

    pub(crate) async fn prepare(
        &self,
        cryptarchia: &Cryptarchia,
        storage_blocks_to_remove: HashSet<HeaderId>,
    ) -> Result<(Vec<KeyValuePair>, Option<u64>), Error> {
        let mut entries = Vec::new();
        let lib_height = cryptarchia.lib_branch().length();
        let checkpoint_due = self
            .checkpoint_lib_height
            .is_none_or(|height| lib_height.saturating_sub(height) >= CHECKPOINT_INTERVAL_BLOCKS);

        let checkpoint_lib_height = if checkpoint_due {
            let state = CryptarchiaConsensusState::from_cryptarchia_and_unpruned_blocks(
                cryptarchia,
                storage_blocks_to_remove.clone(),
                &self.fork_digests,
            )?;

            let bytes = spawn_blocking("logos/chain/encode-checkpoint", move || state.to_bytes())
                .await
                .map_err(|error| {
                    Error::Storage(format!("Could not encode chain checkpoint: {error}"))
                })??;

            entries.push(KeyValuePair {
                key: recovery_key(CryptarchiaSettings::RECOVERY_KEY_SUFFIX),
                value: bytes,
            });

            Some(lib_height)
        } else {
            None
        };

        let progress =
            RecoveryProgress::from_chain(cryptarchia, storage_blocks_to_remove, &self.fork_digests);
        entries.push(KeyValuePair {
            key: recovery_key(RECOVERY_PROGRESS_KEY_SUFFIX),
            value: progress.to_bytes()?,
        });

        Ok((entries, checkpoint_lib_height))
    }

    pub(crate) const fn committed(&mut self, checkpoint_lib_height: Option<u64>) {
        if let Some(height) = checkpoint_lib_height {
            self.checkpoint_lib_height = Some(height);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZero, NonZeroU32, NonZeroU64};

    use lb_core::{
        era::AcceptedForkDigests,
        sdp::{MinStake, ServiceParameters, ServiceType},
    };
    use lb_cryptarchia_engine::{State::Bootstrapping, UncleSlots};
    use lb_ledger::{
        ConsensusExcludedKeys,
        config::{BlendPoWConfig, ModulusShift, PoWConfig, RewardPoWConfig},
        mantle::sdp::{ServiceRewardsParameters, rewards},
    };
    use lb_utils::math::{NonNegativeRatio, PositiveRatio, ProperFraction};

    use super::*;
    use crate::tests::single_era;

    const FORK_DIGEST: [u8; 32] = [0x22; 32];

    /// The same fork digest for every era of `eras`.
    fn fork_digests<Parameters>(eras: &EraSchedule<Parameters>) -> EraSchedule<ForkDigest> {
        eras.map(|_| ForkDigest::from(FORK_DIGEST))
    }

    fn recovery_test_chain() -> Cryptarchia {
        let config = crate::tests::ledger_config(NonZero::<u32>::MIN);
        let ledger = LedgerState::from_utxos([], &config, 0, &ConsensusExcludedKeys::empty());

        Cryptarchia::from_genesis(
            HeaderId::from([0; 32]),
            ledger,
            single_era(config),
            Bootstrapping,
        )
    }

    #[tokio::test]
    async fn checkpoints_roll_over_by_finalized_block_count() {
        let chain = recovery_test_chain();
        let fork_digests = Arc::new(fork_digests(chain.ledger.era_schedule()));
        let mut saved = CryptarchiaConsensusState::from_cryptarchia_and_unpruned_blocks(
            &chain,
            HashSet::new(),
            &fork_digests,
        )
        .unwrap();
        saved.last_engine_state = None;
        let mut fresh = RecoveryRecorder::new(&saved, Arc::clone(&fork_digests));

        let (entries, checkpoint_height) = fresh.prepare(&chain, HashSet::new()).await.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(checkpoint_height, Some(0));

        fresh.committed(checkpoint_height);
        let (entries, checkpoint_height) = fresh.prepare(&chain, HashSet::new()).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(checkpoint_height, None);

        saved.last_engine_state = Some(LastEngineState {
            timestamp: SystemTime::UNIX_EPOCH,
            state: Bootstrapping,
        });
        let mut restarted = RecoveryRecorder::new(&saved, fork_digests);

        let at_height = |id, height| {
            Cryptarchia::from_lib(
                HeaderId::from([id; 32]),
                saved.lib,
                saved.lib_ledger_state.clone(),
                saved.genesis_id,
                Arc::clone(chain.ledger.era_schedule()),
                Bootstrapping,
                Slot::new(height),
                height,
                UncleSlots::default(),
            )
        };
        let below_interval = at_height(1, CHECKPOINT_INTERVAL_BLOCKS - 1);

        let (entries, checkpoint_height) = restarted
            .prepare(&below_interval, HashSet::new())
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(checkpoint_height, None);

        let at_interval = at_height(2, CHECKPOINT_INTERVAL_BLOCKS);
        let past_interval = at_height(3, CHECKPOINT_INTERVAL_BLOCKS + 1);

        // Preparing a checkpoint can be retried until the write is confirmed.
        for candidate in [&at_interval, &at_interval, &past_interval] {
            let (entries, checkpoint_height) =
                restarted.prepare(candidate, HashSet::new()).await.unwrap();
            assert_eq!(entries.len(), 2);
            assert_eq!(checkpoint_height, Some(candidate.lib_branch().length()));
        }

        restarted.committed(Some(CHECKPOINT_INTERVAL_BLOCKS));
        let (entries, checkpoint_height) = restarted
            .prepare(&past_interval, HashSet::new())
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(checkpoint_height, None);
    }

    #[test]
    fn loads_progress_or_uses_the_legacy_checkpoint() {
        let chain = recovery_test_chain();
        let legacy = CryptarchiaConsensusState::from_cryptarchia_and_unpruned_blocks(
            &chain,
            HashSet::new(),
            &fork_digests(chain.ledger.era_schedule()),
        )
        .unwrap();
        let checkpoint_key = recovery_key(CryptarchiaSettings::RECOVERY_KEY_SUFFIX).to_vec();
        let checkpoint_bytes = legacy.to_bytes().unwrap();
        let legacy_data =
            RecoveryData::new([(checkpoint_key.clone(), checkpoint_bytes.clone())].into());

        let recovered = RecoveryProgress::load(&legacy, &legacy_data).unwrap();
        assert_eq!(recovered.tip, legacy.tip);
        assert_eq!(recovered.lib, legacy.lib);
        assert_eq!(saved_tip_era(&legacy_data).unwrap(), Some(legacy.tip_era));

        let mut progress = RecoveryProgress::from_checkpoint(&legacy);
        progress.tip = [1; 32].into();
        progress.tip_era = TipEra {
            slot: Slot::from(100),
            fork_digest: ForkDigest::from([0x33; 32]),
        };
        let progress_key = recovery_key(RECOVERY_PROGRESS_KEY_SUFFIX).to_vec();
        let data = RecoveryData::new(
            [
                (checkpoint_key, checkpoint_bytes),
                (progress_key, progress.to_bytes().unwrap()),
            ]
            .into(),
        );

        assert_eq!(saved_tip_era(&data).unwrap(), Some(progress.tip_era));

        let recovered = RecoveryProgress::load(&legacy, &data).unwrap();
        assert_eq!(recovered.tip, progress.tip);
        assert_eq!(recovered.tip_era, progress.tip_era);
    }

    /// A reward config with claiming disabled, standing in for a real
    /// deployment config in tests.
    fn disabled_reward_config() -> RewardPoWConfig {
        RewardPoWConfig {
            minimum_difficulty: ModulusShift::new::<26>(),
            ema_smoothing_factor: 9,
            ema_smoothing_precision: NonZeroU64::new(10).unwrap(),
            target_claims_per_block: NonZeroU64::new(100).unwrap(),
            rate_num: 0,
            rate_den: NonZeroU64::MIN,
            pow_share: 0,
            share_den: NonZeroU64::MIN,
            slot_window: NonZeroU64::new(100).unwrap(),
        }
    }

    #[test]
    #[expect(clippy::too_many_lines, reason = "Test function")]
    fn save_prunable_forks() {
        let genesis_header_id: HeaderId = [0; 32].into();
        // We don't prune fork stemming from the block before the current tip.
        let security_param: NonZero<u32> = 2.try_into().unwrap();
        let cryptarchia_engine_config = lb_cryptarchia_engine::Config::new(
            security_param,
            ProperFraction::new(1, 10.try_into().unwrap()).unwrap(),
            1f64.try_into().expect("1 > 0"),
            12,
            4,
        );
        let epoch_config = lb_cryptarchia_engine::EpochConfig {
            epoch_stake_distribution_stabilization: 1.try_into().unwrap(),
            epoch_period_nonce_buffer: 1.try_into().unwrap(),
            epoch_period_nonce_stabilization: 1.try_into().unwrap(),
        };
        let epoch_length =
            epoch_config.epoch_length(cryptarchia_engine_config.base_period_length());

        let ledger_config = lb_ledger::Config {
            epoch_config,
            consensus_config: cryptarchia_engine_config,
            sdp_config: lb_ledger::mantle::sdp::Config {
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
                    blend: rewards::blend::RewardsParameters {
                        rounds_per_epoch: epoch_length.try_into().unwrap(),
                        message_frequency_per_round: PositiveRatio::new(1, NonZeroU32::MIN)
                            .unwrap(),
                        cover_replication_factor: 0,
                        num_blend_layers: NonZeroU64::new(3).unwrap(),
                        minimum_network_size: NonZeroU64::new(1).unwrap(),
                        data_replication_factor: 0,
                        activity_threshold_sensitivity: 1,
                    },
                },
                min_stake: MinStake { threshold: 1 },
            },
            max_transactions: 1024,
            max_transactions_size: 2 * 1024 * 1024,
            max_op_data_size: 2 * 1024 * 1024 * 7 / 8,
            execution_market: lb_ledger::config::ExecutionMarketConfig {
                max_gas_per_block: lb_ledger::config::MaxGasPerBlock::new(3_193_460).unwrap(),
                fee_adjustment_rate: ProperFraction::new(1, 8.try_into().unwrap()).unwrap(),
                ema_smoothing_factor: ProperFraction::new(9, 10.try_into().unwrap()).unwrap(),
            },
            storage_market: lb_ledger::config::StorageMarketConfig {
                max_adjustment_factor: ProperFraction::new(1, 8.try_into().unwrap()).unwrap(),
                ema_smoothing_factor: ProperFraction::new(1, 2.try_into().unwrap()).unwrap(),
            },
            block_rewards: lb_ledger::config::BlockRewardsConfig::new(
                3_000_000_000,
                120_000_000.try_into().unwrap(),
                10_512,
                NonNegativeRatio::new(62_500, 657.try_into().unwrap()),
                ProperFraction::new(6, 10.try_into().unwrap()).unwrap(),
            )
            .unwrap(),
            gas_profile: lb_core::mantle::gas::GasProfile {
                transfer: 590.into(),
                channel_config: 56.into(),
                channel_inscribe: 56.into(),
                channel_deposit: 590.into(),
                channel_withdraw: 56.into(),
                channel_transfer: 56.into(),
                sdp_declare: 646.into(),
                sdp_withdraw: 590.into(),
                sdp_active: 590.into(),
                leader_claim: 580.into(),
                claim_pow_reward: 1.into(),
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
            accepted_fork_digests: AcceptedForkDigests::new(vec![ForkDigest::new([0x11; 32])]),
        };

        let (cryptarchia_engine, pruned_blocks) = {
            // Boostrapping mode since we are pursposefully adding old forks to
            // test the recovery mechanism.
            let mut cryptarchia = lb_cryptarchia_engine::Cryptarchia::<_>::from_genesis(
                genesis_header_id,
                Arc::new(
                    single_era(ledger_config.clone())
                        .map(|era| era.parameters.consensus_config.clone()),
                ),
                Bootstrapping,
            );

            //      b4 - b5
            //    /
            // b0 - b1 - b2 - b3 == local chain tip
            //    \    \    \
            //      b6   b7   b8
            //
            // Add 3 more blocks to canonical chain. `b0`, `b1`, `b2`, and `b3`
            // represent the canonical chain now.
            cryptarchia
                .receive_block(
                    [1; 32].into(),
                    genesis_header_id,
                    1.into(),
                    UncleSlots::default(),
                )
                .expect("Block 1 to be added successfully on top of block 0.");
            cryptarchia
                .receive_block(
                    [2; 32].into(),
                    [1; 32].into(),
                    2.into(),
                    UncleSlots::default(),
                )
                .expect("Block 2 to be added successfully on top of block 1.");
            cryptarchia
                .receive_block(
                    [3; 32].into(),
                    [2; 32].into(),
                    3.into(),
                    UncleSlots::default(),
                )
                .expect("Block 3 to be added successfully on top of block 2.");
            // Add a 2-block fork from genesis
            cryptarchia
                .receive_block(
                    [4; 32].into(),
                    genesis_header_id,
                    1.into(),
                    UncleSlots::default(),
                )
                .expect("Block 4 to be added successfully on top of block 0.");
            cryptarchia
                .receive_block(
                    [5; 32].into(),
                    [4; 32].into(),
                    2.into(),
                    UncleSlots::default(),
                )
                .expect("Block 5 to be added successfully on top of block 4.");
            // Add a second single-block fork from genesis
            cryptarchia
                .receive_block(
                    [6; 32].into(),
                    genesis_header_id,
                    1.into(),
                    UncleSlots::default(),
                )
                .expect("Block 6 to be added successfully on top of block 0.");
            // Add a single-block fork from the block after genesis (block `1`)
            cryptarchia
                .receive_block(
                    [7; 32].into(),
                    [1; 32].into(),
                    2.into(),
                    UncleSlots::default(),
                )
                .expect("Block 7 to be added successfully on top of block 1.");
            // Add a single-block fork from the second block after genesis
            // (block `2`)
            cryptarchia
                .receive_block(
                    [8; 32].into(),
                    [2; 32].into(),
                    3.into(),
                    UncleSlots::default(),
                )
                .expect("Block 8 to be added successfully on top of block 2.");

            cryptarchia.online()
        };

        // Empty ledger state.
        let ledger_state = lb_ledger::Ledger::new(
            cryptarchia_engine.lib(),
            LedgerState::from_utxos([], &ledger_config, 0, &ConsensusExcludedKeys::empty()),
            single_era(ledger_config),
        );

        // Build [`CryptarchiaConsensusState`] with the pruned blocks.
        let pruned_stale_blocks = pruned_blocks
            .stale_blocks()
            .copied()
            .collect::<HashSet<_>>();
        let fork_digests = fork_digests(ledger_state.era_schedule());
        let recovery_state = CryptarchiaConsensusState::from_cryptarchia_and_unpruned_blocks(
            &Cryptarchia {
                ledger: ledger_state,
                consensus: cryptarchia_engine.clone(),
                genesis_id: genesis_header_id,
            },
            pruned_stale_blocks.clone(),
            &fork_digests,
        )
        .unwrap();

        assert_eq!(recovery_state.tip, cryptarchia_engine.tip());
        assert_eq!(
            recovery_state.tip_era,
            TipEra {
                slot: cryptarchia_engine.tip_branch().slot(),
                fork_digest: ForkDigest::from(FORK_DIGEST),
            }
        );
        assert_eq!(recovery_state.lib, cryptarchia_engine.lib());
        assert_eq!(
            &recovery_state.storage_blocks_to_remove,
            &pruned_stale_blocks
        );
    }

    #[test]
    #[expect(clippy::too_many_lines, reason = "Test function")]
    fn restore_preserves_info() {
        let genesis_header_id: HeaderId = [0; 32].into();

        let security_param: NonZero<u32> = 2.try_into().unwrap();
        let cryptarchia_engine_config = lb_cryptarchia_engine::Config::new(
            security_param,
            ProperFraction::new(1, 10.try_into().unwrap()).unwrap(),
            1f64.try_into().expect("1 > 0"),
            12,
            4,
        );
        let epoch_config = lb_cryptarchia_engine::EpochConfig {
            epoch_stake_distribution_stabilization: 1.try_into().unwrap(),
            epoch_period_nonce_buffer: 1.try_into().unwrap(),
            epoch_period_nonce_stabilization: 1.try_into().unwrap(),
        };
        let epoch_length =
            epoch_config.epoch_length(cryptarchia_engine_config.base_period_length());

        let ledger_config = lb_ledger::Config {
            epoch_config,
            consensus_config: cryptarchia_engine_config,
            sdp_config: lb_ledger::mantle::sdp::Config {
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
                    blend: rewards::blend::RewardsParameters {
                        rounds_per_epoch: epoch_length.try_into().unwrap(),
                        message_frequency_per_round: PositiveRatio::new(1, NonZeroU32::MIN)
                            .unwrap(),
                        cover_replication_factor: 0,
                        num_blend_layers: NonZeroU64::new(3).unwrap(),
                        minimum_network_size: NonZeroU64::new(1).unwrap(),
                        data_replication_factor: 0,
                        activity_threshold_sensitivity: 1,
                    },
                },
                min_stake: MinStake { threshold: 1 },
            },
            max_transactions: 1024,
            max_transactions_size: 2 * 1024 * 1024,
            max_op_data_size: 2 * 1024 * 1024 * 7 / 8,
            execution_market: lb_ledger::config::ExecutionMarketConfig {
                max_gas_per_block: lb_ledger::config::MaxGasPerBlock::new(3_193_460).unwrap(),
                fee_adjustment_rate: ProperFraction::new(1, 8.try_into().unwrap()).unwrap(),
                ema_smoothing_factor: ProperFraction::new(9, 10.try_into().unwrap()).unwrap(),
            },
            storage_market: lb_ledger::config::StorageMarketConfig {
                max_adjustment_factor: ProperFraction::new(1, 8.try_into().unwrap()).unwrap(),
                ema_smoothing_factor: ProperFraction::new(1, 2.try_into().unwrap()).unwrap(),
            },
            block_rewards: lb_ledger::config::BlockRewardsConfig::new(
                3_000_000_000,
                120_000_000.try_into().unwrap(),
                10_512,
                NonNegativeRatio::new(62_500, 657.try_into().unwrap()),
                ProperFraction::new(6, 10.try_into().unwrap()).unwrap(),
            )
            .unwrap(),
            gas_profile: lb_core::mantle::gas::GasProfile {
                transfer: 590.into(),
                channel_config: 56.into(),
                channel_inscribe: 56.into(),
                channel_deposit: 590.into(),
                channel_withdraw: 56.into(),
                channel_transfer: 56.into(),
                sdp_declare: 646.into(),
                sdp_withdraw: 590.into(),
                sdp_active: 590.into(),
                leader_claim: 580.into(),
                claim_pow_reward: 1.into(),
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
            accepted_fork_digests: AcceptedForkDigests::new(vec![ForkDigest::new([0x11; 32])]),
        };

        // Build a chain: b0 (genesis) - b1 - b2 - b3 - b4 - b5
        // With security_param=2, going online will advance LIB.
        let mut engine = lb_cryptarchia_engine::Cryptarchia::<HeaderId>::from_genesis(
            genesis_header_id,
            Arc::new(
                single_era(ledger_config.clone())
                    .map(|era| era.parameters.consensus_config.clone()),
            ),
            Bootstrapping,
        );
        let block_ids: Vec<HeaderId> = (1..=5u8).map(|i| [i; 32].into()).collect();
        let mut parent = genesis_header_id;
        for (i, &block_id) in block_ids.iter().enumerate() {
            let slot = (i as u64 + 1).into();
            let uncle_slots = UncleSlots::try_new(vec![Slot::from(i as u64)], 4).unwrap();
            engine
                .receive_block(block_id, parent, slot, uncle_slots)
                .unwrap_or_else(|_| panic!("Block {block_id} should be added successfully"));
            parent = block_id;
        }

        // Go online to advance LIB past genesis.
        let (engine, _pruned) = engine.online();
        let lib_id = engine.lib();
        assert_ne!(
            lib_id, genesis_header_id,
            "LIB should have advanced past genesis"
        );

        // Build the full Cryptarchia (with ledger) to get info() before save.
        let original = Cryptarchia {
            consensus: engine.clone(),
            ledger: lb_ledger::Ledger::new(
                lib_id,
                LedgerState::from_utxos([], &ledger_config, 0, &ConsensusExcludedKeys::empty()),
                single_era(ledger_config.clone()),
            ),
            genesis_id: genesis_header_id,
        };
        let info_before = original.info();

        // Save state (simulates what happens before shutdown).
        let saved_state = CryptarchiaConsensusState::from_cryptarchia_and_unpruned_blocks(
            &original,
            HashSet::new(),
            &fork_digests(original.ledger.era_schedule()),
        )
        .unwrap();
        let encoded_state = saved_state.to_bytes().unwrap();
        let saved_state = CryptarchiaConsensusState::from_bytes(&encoded_state).unwrap();

        // Restore (simulates initialize_cryptarchia on restart):
        // Create a new Cryptarchia from the saved LIB with its slot and length.
        let mut restored = Cryptarchia::from_lib(
            saved_state.lib,
            engine.lib_branch().parent(),
            saved_state.lib_ledger_state.clone(),
            saved_state.genesis_id,
            single_era(ledger_config),
            *engine.state(),
            saved_state.lib_block_slot,
            saved_state.lib_block_length,
            saved_state.lib_block_uncle_slots.clone(),
        );

        // Check the restore LIB is the same as before.
        let restored_lib = restored.consensus.lib_branch();
        let original_lib = engine.lib_branch();
        assert_eq!(restored_lib.id(), original_lib.id());
        assert_eq!(restored_lib.parent(), original_lib.parent());
        assert_eq!(restored_lib.slot(), original_lib.slot());
        assert_eq!(restored_lib.length(), original_lib.length());
        assert_eq!(restored_lib.uncle_slots(), original_lib.uncle_slots());

        // Replay blocks between LIB and tip (as initialize_cryptarchia does).
        // Walk from tip back to LIB to find the blocks to replay.
        let mut blocks_to_replay = Vec::new();
        let mut current = saved_state.tip;
        while current != saved_state.lib {
            let branch = engine.branches().get(&current).unwrap();
            blocks_to_replay.push((current, branch.slot()));
            current = branch.parent();
        }
        blocks_to_replay.reverse();
        for (block_id, slot) in blocks_to_replay {
            let branch = engine.branches().get(&block_id).unwrap();
            let parent_id = branch.parent();
            let uncle_slots = branch.uncle_slots().clone();
            restored
                .consensus
                .receive_block(block_id, parent_id, slot, uncle_slots)
                .unwrap_or_else(|_| panic!("Replay of {block_id} should succeed"));
            assert_eq!(
                restored.consensus.branches().get(&block_id).unwrap(),
                engine.branches().get(&block_id).unwrap()
            );
        }

        let info_after = restored.info();

        assert_eq!(info_before.tip, info_after.tip);
        assert_eq!(info_before.lib, info_after.lib);
        assert_eq!(info_before.slot, info_after.slot);
        assert_eq!(
            info_before.height, info_after.height,
            "Height must be preserved across restart: before={}, after={}",
            info_before.height, info_after.height
        );
    }
}
