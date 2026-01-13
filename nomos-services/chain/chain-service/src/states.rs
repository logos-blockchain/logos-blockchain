use std::{collections::HashSet, time::SystemTime};

use groth16::{Field as _, Fr};
use nomos_core::header::{Header, HeaderId};
use nomos_ledger::LedgerState;
use overwatch::{DynError, services::state::ServiceState};
use serde::{Deserialize, Serialize};

use crate::{Cryptarchia, CryptarchiaSettings, Error, StartingState};

#[derive(Clone, Serialize, Deserialize)]
pub struct CryptarchiaConsensusState {
    pub(crate) tip: HeaderId,
    pub(crate) lib: HeaderId,
    pub(crate) lib_ledger_state: LedgerState,
    pub(crate) lib_block_length: u64,
    pub(crate) genesis_id: HeaderId,
    /// Set of blocks that have been pruned from the engine but have not yet
    /// been deleted from the persistence layer because of some unexpected
    /// error.
    pub(crate) storage_blocks_to_remove: HashSet<HeaderId>,
    /// Last engine state and timestamp for offline grace period tracking
    pub(crate) last_engine_state: Option<LastEngineState>,
}

impl CryptarchiaConsensusState {
    /// Re-create the [`CryptarchiaConsensusState`]
    /// given the cryptarchia engine and ledger state.
    ///
    /// Furthermore, it allows to specify blocks deleted from the cryptarchia
    /// engine (hence not tracked anymore) but that should be deleted from the
    /// persistence layer.
    pub(crate) fn from_cryptarchia_and_unpruned_blocks(
        cryptarchia: &Cryptarchia,
        storage_blocks_to_remove: HashSet<HeaderId>,
    ) -> Result<Self, DynError> {
        let lib = cryptarchia.consensus.lib_branch();
        let Some(lib_ledger_state) = cryptarchia.ledger.state(&lib.id()).cloned() else {
            return Err(DynError::from(
                "Ledger state associated with LIB not found, something is corrupted",
            ));
        };
        let lib_block_length = lib.length();

        Ok(Self {
            tip: cryptarchia.consensus.tip_branch().id(),
            lib: lib.id(),
            genesis_id: cryptarchia.genesis_id,
            lib_ledger_state,
            lib_block_length,
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
        let (lib_id, genesis_id, lib_ledger_state) = match &settings.starting_state {
            StartingState::Genesis { genesis_tx } => {
                let lib_id = Header::genesis(genesis_tx).id();
                let ledger = LedgerState::from_genesis_tx(
                    genesis_tx.clone(),
                    &settings.config,
                    Fr::ZERO, // TODO: recover from genesis tx
                )?;
                (lib_id, lib_id, ledger)
            }
            StartingState::Lib {
                lib_id,
                genesis_id,
                lib_ledger_state,
            } => (*lib_id, *genesis_id, lib_ledger_state.as_ref().clone()),
        };

        Ok(Self {
            tip: lib_id,
            lib: lib_id,
            lib_ledger_state,
            lib_block_length: 0,
            genesis_id,
            storage_blocks_to_remove: HashSet::new(),
            last_engine_state: None,
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct LastEngineState {
    pub timestamp: SystemTime,
    pub state: cryptarchia_engine::State,
}

#[cfg(test)]
mod tests {
    use std::{
        num::{NonZero, NonZeroU64},
        sync::Arc,
    };

    use cryptarchia_engine::State::Bootstrapping;
    use nomos_core::sdp::{MinStake, ServiceParameters, ServiceType};
    use nomos_ledger::mantle::sdp::{ServiceRewardsParameters, rewards};
    use nomos_utils::math::NonNegativeF64;

    use super::*;

    #[test]
    #[expect(clippy::too_many_lines, reason = "A big test case")]
    fn save_prunable_forks() {
        let genesis_header_id: HeaderId = [0; 32].into();
        // We don't prune fork stemming from the block before the current tip.
        let security_param: NonZero<u32> = 2.try_into().unwrap();
        let cryptarchia_engine_config = cryptarchia_engine::Config {
            security_param,
            active_slot_coeff: 0f64,
        };
        let ledger_config = nomos_ledger::Config {
            epoch_config: cryptarchia_engine::EpochConfig {
                epoch_stake_distribution_stabilization: 1.try_into().unwrap(),
                epoch_period_nonce_buffer: 1.try_into().unwrap(),
                epoch_period_nonce_stabilization: 1.try_into().unwrap(),
            },
            consensus_config: cryptarchia_engine_config,
            sdp_config: nomos_ledger::mantle::sdp::Config {
                service_params: Arc::new(
                    [
                        (
                            ServiceType::BlendNetwork,
                            ServiceParameters {
                                lock_period: 10,
                                inactivity_period: 20,
                                retention_period: 100,
                                timestamp: 0,
                                session_duration: 10,
                            },
                        ),
                        (
                            ServiceType::DataAvailability,
                            ServiceParameters {
                                lock_period: 10,
                                inactivity_period: 20,
                                retention_period: 100,
                                timestamp: 0,
                                session_duration: 10,
                            },
                        ),
                    ]
                    .into(),
                ),
                service_rewards_params: ServiceRewardsParameters {
                    blend: rewards::blend::RewardsParameters {
                        rounds_per_session: NonZeroU64::new(10).unwrap(),
                        message_frequency_per_round: NonNegativeF64::try_from(1.0).unwrap(),
                        num_blend_layers: NonZeroU64::new(3).unwrap(),
                        minimum_network_size: NonZeroU64::new(1).unwrap(),
                    },
                },
                min_stake: MinStake {
                    threshold: 1,
                    timestamp: 0,
                },
            },
        };

        let (cryptarchia_engine, pruned_blocks) = {
            // Boostrapping mode since we are pursposefully adding old forks to test the
            // recovery mechanism.
            let mut cryptarchia = cryptarchia_engine::Cryptarchia::<_>::from_lib(
                genesis_header_id,
                cryptarchia_engine_config,
                Bootstrapping,
            );

            //      b4 - b5
            //    /
            // b0 - b1 - b2 - b3 == local chain tip
            //    \    \    \
            //      b6   b7   b8
            //
            // Add 3 more blocks to canonical chain. `b0`, `b1`, `b2`, and `b3` represent
            // the canonical chain now.
            cryptarchia = cryptarchia
                .receive_block([1; 32].into(), genesis_header_id, 1.into())
                .expect("Block 1 to be added successfully on top of block 0.")
                .0
                .receive_block([2; 32].into(), [1; 32].into(), 2.into())
                .expect("Block 2 to be added successfully on top of block 1.")
                .0
                .receive_block([3; 32].into(), [2; 32].into(), 3.into())
                .expect("Block 3 to be added successfully on top of block 2.")
                .0;
            // Add a 2-block fork from genesis
            cryptarchia = cryptarchia
                .receive_block([4; 32].into(), genesis_header_id, 1.into())
                .expect("Block 4 to be added successfully on top of block 0.")
                .0
                .receive_block([5; 32].into(), [4; 32].into(), 2.into())
                .expect("Block 5 to be added successfully on top of block 4.")
                .0;
            // Add a second single-block fork from genesis
            cryptarchia = cryptarchia
                .receive_block([6; 32].into(), genesis_header_id, 1.into())
                .expect("Block 6 to be added successfully on top of block 0.")
                .0;
            // Add a single-block fork from the block after genesis (block `1`)
            cryptarchia = cryptarchia
                .receive_block([7; 32].into(), [1; 32].into(), 2.into())
                .expect("Block 7 to be added successfully on top of block 1.")
                .0;
            // Add a single-block fork from the second block after genesis (block `2`)
            cryptarchia = cryptarchia
                .receive_block([8; 32].into(), [2; 32].into(), 3.into())
                .expect("Block 8 to be added successfully on top of block 2.")
                .0;

            cryptarchia.online()
        };

        // Empty ledger state.
        let ledger_state = nomos_ledger::Ledger::new(
            cryptarchia_engine.lib(),
            LedgerState::from_utxos([], &ledger_config),
            ledger_config,
        );

        // Build [`CryptarchiaConsensusState`] with the pruned blocks.
        let pruned_stale_blocks = pruned_blocks
            .stale_blocks()
            .copied()
            .collect::<HashSet<_>>();
        let recovery_state = CryptarchiaConsensusState::from_cryptarchia_and_unpruned_blocks(
            &Cryptarchia {
                ledger: ledger_state,
                consensus: cryptarchia_engine.clone(),
                genesis_id: genesis_header_id,
            },
            pruned_stale_blocks.clone(),
        )
        .unwrap();

        assert_eq!(recovery_state.tip, cryptarchia_engine.tip());
        assert_eq!(recovery_state.lib, cryptarchia_engine.lib());
        assert_eq!(
            &recovery_state.storage_blocks_to_remove,
            &pruned_stale_blocks
        );
    }
}
