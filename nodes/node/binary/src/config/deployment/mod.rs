use core::time::Duration;

use lb_core::{
    block::genesis::GenesisBlock,
    era::{EraDigest, ForkDigest},
    header::HeaderId,
    mantle::{
        traits::GenesisTx as _,
        transactions::genesis_tx::{ChainId, GenesisTime},
    },
};
use lb_cryptarchia_engine::{
    Epoch, Slot,
    era::{EraEntriesAfterGenesis, EraEntry},
};
use lb_utils::yaml::{OnUnknownKeys, deserialize_value_from_reader};
use serde::{Deserialize, Serialize};

pub mod era;
use era::GENESIS_EPOCH;
pub use era::{EraDeclaration, EraDeclarations, EraScheduleError};
mod protocols;
pub use protocols::ProtocolScope;

use crate::config::deployment::era::parameters::{EraParameters, v1};

pub const SERIALIZED_DEPLOYMENT: &[u8] = include_bytes!("settings.yaml");

type EraSchedule = lb_cryptarchia_engine::era::EraSchedule<EraDefinition>;

/// An era schedule as its file declares it.
#[derive(Serialize, Deserialize)]
struct EraDefinitions {
    eras: EraDeclarations,
    genesis_block: GenesisBlock,
}

/// Everything that defines a chain: the eras of its schedule, resolved against
/// the genesis block they start from.
///
/// It is read and written as its file declares it, its eras then its genesis
/// block, and the eras are resolved as it is read: a schedule that does not
/// resolve is refused at load.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(from = "EraDefinitions", into = "EraDefinitions")]
pub struct DeploymentSettings {
    eras: EraSchedule,
    genesis_block: GenesisBlock,
}

impl DeploymentSettings {
    /// The deployment whose chain starts from `genesis_block` and runs the eras
    /// `eras` declares, resolved: where each era starts, and its digest and
    /// fork digest.
    ///
    /// # Errors
    ///
    /// If an era starts beyond the slots or the time this node can represent.
    #[must_use]
    pub fn new(eras: &EraDeclarations, genesis_block: GenesisBlock) -> Self {
        let genesis_inscription = genesis_block.genesis_tx().cryptarchia_parameter();
        let (genesis_id, chain_id, genesis_time) = (
            genesis_block.header().id(),
            genesis_inscription.chain_id,
            genesis_inscription.genesis_time,
        );
        let mut era_digests = Vec::with_capacity(1);
        // Called in activation order: the fork digest of an era is over the
        // digests of the eras up to it.
        let mut entry = |first_epoch: Epoch, era: &EraDeclaration| {
            let digest = EraDigest::compute(first_epoch, era);
            era_digests.push(digest);
            let fork_digest =
                ForkDigest::compute(genesis_id, &chain_id, era_digests.iter().copied());
            EraEntry {
                block_version: era.block_version,
                slot_duration: era.parameters.slot_duration(),
                epoch_length_in_slots: era.parameters.epoch_length(),
                parameters: EraDefinition {
                    parameters: era.parameters.clone(),
                    digest,
                    fork_digest,
                },
            }
        };
        // Only single-era schedules are supported for now.
        let genesis = entry(GENESIS_EPOCH, eras.genesis_era());
        let eras = EraSchedule::new(
            genesis_time.into(),
            genesis,
            EraEntriesAfterGenesis::empty(),
        )
        .unwrap();
        Self {
            eras,
            genesis_block,
        }
    }

    /// The eras of this deployment's chain, resolved.
    #[must_use]
    pub const fn era_schedule(&self) -> &EraSchedule {
        &self.eras
    }

    /// The genesis block this deployment's chain starts from.
    #[must_use]
    pub const fn genesis_block(&self) -> &GenesisBlock {
        &self.genesis_block
    }

    /// The chain this deployment targets, read off the genesis inscription.
    #[must_use]
    pub fn chain_id(&self) -> ChainId {
        self.genesis_block
            .genesis_tx()
            .cryptarchia_parameter()
            .chain_id
    }

    /// When this deployment's chain starts, read off the genesis inscription.
    #[must_use]
    pub fn genesis_time(&self) -> GenesisTime {
        self.genesis_block
            .genesis_tx()
            .cryptarchia_parameter()
            .genesis_time
    }

    /// The ID of the genesis block this deployment's chain starts from.
    #[must_use]
    pub fn genesis_id(&self) -> HeaderId {
        self.genesis_block.header().id()
    }

    #[must_use]
    pub const fn genesis_fork_digest(&self) -> ForkDigest {
        self.eras.genesis().entry.parameters.fork_digest
    }

    /// The fork digest of the era in force by the wall clock, which names the
    /// fork-bound protocols of this deployment's chain now.
    #[must_use]
    pub fn fork_digest_in_force(&self) -> ForkDigest {
        let now = self
            .eras
            .slot_at(time::OffsetDateTime::now_utc())
            .unwrap_or(Slot::genesis());
        self.eras.at_slot(now).entry.parameters.fork_digest
    }

    /// The parameters of the genesis era, in version 1's layout, the only one
    /// there is.
    #[must_use]
    pub const fn genesis_era_parameters(&self) -> &v1::Parameters {
        match &self.eras.genesis().entry.parameters.parameters {
            EraParameters::V1(parameters) => parameters,
        }
    }

    /// Changes the parameters of the genesis era with `update`, and resolves
    /// the schedule again, so the digests of every era follow the change.
    ///
    /// # Panics
    ///
    /// Never: a schedule of the genesis era alone always resolves.
    pub fn update_genesis_era_parameters<Update>(&mut self, update: Update)
    where
        Update: FnOnce(&mut v1::Parameters),
    {
        let EraDefinitions {
            mut eras,
            genesis_block,
        } = EraDefinitions::from(self.clone());
        let EraParameters::V1(parameters) = &mut eras.genesis_era_mut().parameters;
        update(parameters);
        *self = Self::new(&eras, genesis_block);
    }

    #[must_use]
    pub const fn genesis_blend_round_duration(&self) -> Duration {
        self.genesis_era_parameters().blend_round_duration()
    }
}

impl From<EraDefinitions> for DeploymentSettings {
    fn from(
        EraDefinitions {
            eras,
            genesis_block,
        }: EraDefinitions,
    ) -> Self {
        Self::new(&eras, genesis_block)
    }
}

/// The eras as the file declares them: each era's block version and
/// parameters, keyed by the epoch it starts at.
impl From<DeploymentSettings> for EraDefinitions {
    fn from(
        DeploymentSettings {
            eras,
            genesis_block,
        }: DeploymentSettings,
    ) -> Self {
        // Only single-era schedules are supported for now.
        let genesis = eras.genesis();
        Self {
            eras: EraDeclarations::new_genesis(EraDeclaration {
                block_version: genesis.entry.block_version,
                parameters: genesis.entry.parameters.parameters.clone(),
            }),
            genesis_block,
        }
    }
}

/// An era as the node runs it: its parameters, its digest, and the fork digest
/// in force while it is, which names its protocols. Its block version is in its
/// entry of the schedule.
#[derive(Clone, Debug)]
pub struct EraDefinition {
    pub parameters: EraParameters,
    pub digest: EraDigest,
    /// The digest of the eras up to this one, in activation order.
    pub fork_digest: ForkDigest,
}

impl Default for DeploymentSettings {
    fn default() -> Self {
        deserialize_value_from_reader(SERIALIZED_DEPLOYMENT, OnUnknownKeys::Fail)
            .expect("Default deployment settings must be valid.")
    }
}

#[cfg(test)]
mod tests {
    use crate::config::DeploymentSettings;

    #[test]
    fn default_initialization() {
        drop(DeploymentSettings::default());
    }

    #[test]
    fn serialize_deserialize_yaml() {
        let settings = DeploymentSettings::default();
        let as_str = serde_yaml::to_string(&settings).unwrap();
        let _recovered: DeploymentSettings = serde_yaml::from_str(&as_str).unwrap();
    }

    #[test]
    fn the_fork_digest_survives_a_round_trip_through_yaml() {
        // Not pinned to a value: the default deployment changes at every
        // genesis ceremony. What must hold is that the digests depend only on
        // the settings, not on how they were loaded.
        let settings = DeploymentSettings::default();
        let recovered: DeploymentSettings =
            serde_yaml::from_str(&serde_yaml::to_string(&settings).unwrap()).unwrap();
        assert_eq!(
            recovered.genesis_fork_digest(),
            settings.genesis_fork_digest()
        );
    }

    #[test]
    fn the_fork_digest_commits_to_the_era_parameters() {
        let settings = DeploymentSettings::default();
        let mut changed = settings.clone();
        changed.update_genesis_era_parameters(|parameters| {
            let reward = &mut parameters.cryptarchia.pow_config.reward;
            reward.slot_window = reward.slot_window.checked_add(1).unwrap();
        });
        assert_ne!(
            changed.genesis_fork_digest(),
            settings.genesis_fork_digest()
        );
    }

    #[test]
    fn genesis_epoch_reward_matches_the_payout_rate() {
        // `epoch_reward_genesis` is not free: it must be the `sigma_e` the
        // first epoch boundary would compute for the genesis pool, or genesis
        // and steady state disagree. That is
        // `W0 * rate_num / (rate_den * target_claim_per_block * N_b)`, and
        // `N_b` follows from the consensus schedule — so changing
        // `security_param` or `slot_activation_coeff` moves this value too.
        let settings = DeploymentSettings::default();
        let cryptarchia = &settings.genesis_era_parameters().cryptarchia;
        let reward = &cryptarchia.pow_config.reward;
        let denominator = u128::from(reward.rate_den.get())
            * u128::from(reward.target_claim_per_block.get())
            * u128::from(cryptarchia.expected_blocks_per_epoch().get());
        assert_eq!(
            u128::from(reward.epoch_reward_genesis),
            u128::from(reward.reward_pool_genesis) * u128::from(reward.rate_num) / denominator,
        );
    }
}
