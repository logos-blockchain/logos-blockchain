use core::time::Duration;

use lb_core::{
    block::genesis::GenesisBlock,
    era::{Era, EraDigest, ForkDigest},
    header::HeaderId,
    mantle::{
        traits::GenesisTx as _,
        transactions::genesis_tx::{ChainId, GenesisTime},
    },
};
use lb_cryptarchia_engine::{
    Epoch,
    era::{EraEntry, Eras, ErasError},
};
use lb_era_parameters::{EraDefinition, EraParameters, ProtocolNames, v1};
use lb_utils::yaml::{OnUnknownKeys, deserialize_value_from_reader};
use serde::{Deserialize, Serialize};

pub mod era;
pub use era::{EraSchedule, EraScheduleError};

pub const SERIALIZED_DEPLOYMENT: &[u8] = include_bytes!("settings.yaml");

/// Everything that defines a chain: the parameters of each of its eras, and
/// the genesis block they start from.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeploymentSettings {
    pub eras: EraSchedule,
    pub genesis_block: GenesisBlock,
}

impl DeploymentSettings {
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

    /// The digest of the fork this deployment's chain follows while `era` is
    /// in force: of its genesis block, of its chain ID and of the eras of its
    /// schedule up to `era` included, in activation order. The eras scheduled
    /// after `era` are left out, so scheduling a new era changes neither this
    /// digest nor the protocol names derived from it until the new era
    /// activates. `None` if the schedule has no era `era`.
    #[must_use]
    pub fn fork_digest_at_era(&self, era: Era) -> Option<ForkDigest> {
        fork_digest_at_era(self.genesis_id(), &self.chain_id(), self.eras.iter(), era)
    }

    #[must_use]
    pub fn genesis_fork_digest(&self) -> ForkDigest {
        self.fork_digest_at_era(Era::GENESIS)
            .expect("every era schedule has a genesis era")
    }

    /// The protocol and topic names of this deployment's chain while `era` is
    /// in force, derived from its chain ID and from the fork digest of `era`.
    /// `None` if the schedule has no era `era`.
    #[must_use]
    pub fn protocol_names_at_era(&self, era: Era) -> Option<ProtocolNames> {
        self.fork_digest_at_era(era)
            .map(|fork_digest| ProtocolNames::derive(&self.chain_id(), fork_digest))
    }

    #[must_use]
    pub fn genesis_protocol_names(&self) -> ProtocolNames {
        self.protocol_names_at_era(Era::GENESIS)
            .expect("every era schedule has a genesis era")
    }

    /// The parameters of the genesis era, in version 1's layout, the only one
    /// there is.
    #[must_use]
    pub const fn genesis_era_parameters(&self) -> &v1::Parameters {
        match self.eras.genesis() {
            EraParameters::V1(parameters) => parameters,
        }
    }

    /// See [`Self::genesis_era_parameters`].
    pub const fn genesis_era_parameters_mut(&mut self) -> &mut v1::Parameters {
        match self.eras.genesis_mut() {
            EraParameters::V1(parameters) => parameters,
        }
    }

    #[must_use]
    pub const fn genesis_blend_round_duration(&self) -> Duration {
        self.genesis_era_parameters().blend_round_duration()
    }

    /// The schedule resolved: each era with its number, its slot duration and
    /// epoch length, and its definition, its parameters and the digests and
    /// protocol names in force while it is.
    pub fn eras(&self) -> Result<Eras<EraDefinition>, ErasError> {
        let (genesis_id, chain_id) = (self.genesis_id(), self.chain_id());
        let mut era_digests = Vec::with_capacity(self.eras.iter().len());
        let mut entries = Vec::with_capacity(self.eras.iter().len());
        for (first_epoch, parameters) in self.eras.iter() {
            let digest = EraDigest::compute(first_epoch, parameters);
            era_digests.push(digest);
            let fork_digest =
                ForkDigest::compute(genesis_id, &chain_id, era_digests.iter().copied());
            entries.push(EraEntry {
                first_epoch,
                version: parameters.version(),
                slot_duration: parameters.slot_duration(),
                epoch_length: parameters.epoch_length(),
                parameters: EraDefinition {
                    parameters: parameters.clone(),
                    digest,
                    fork_digest,
                    protocol_names: ProtocolNames::derive(&chain_id, fork_digest),
                },
            });
        }
        Eras::new(self.genesis_time().into(), entries)
    }

    /// The schedule resolved, if this release can run it.
    ///
    /// Switching eras while running is not implemented yet, so a schedule of
    /// more than one era is refused: a node would otherwise keep running the
    /// first era past the second one's start, and fork off the chain.
    pub fn runnable_eras(&self) -> Result<Eras<EraDefinition>, UnrunnableDeployment> {
        let scheduled = self.eras.iter().len();
        if scheduled > 1 {
            return Err(UnrunnableDeployment::MultipleEras(scheduled));
        }
        Ok(self.eras()?)
    }
}

/// Why this release cannot run a deployment.
#[derive(Debug, thiserror::Error)]
pub enum UnrunnableDeployment {
    #[error(transparent)]
    Schedule(#[from] ErasError),
    #[error("this release runs single-era schedules only, but the deployment schedules {0} eras")]
    MultipleEras(usize),
}

impl Default for DeploymentSettings {
    fn default() -> Self {
        deserialize_value_from_reader(SERIALIZED_DEPLOYMENT, OnUnknownKeys::Fail)
            .expect("Default deployment settings must be valid.")
    }
}

/// The digest of the fork of the chain `chain_id` from the genesis block
/// `genesis_id` while `era` is in force, given every era of the chain's
/// schedule in activation order: of the eras up to `era` only. `None` if the
/// schedule has fewer eras.
fn fork_digest_at_era<'era, Eras>(
    genesis_id: HeaderId,
    chain_id: &ChainId,
    eras: Eras,
    era: Era,
) -> Option<ForkDigest>
where
    Eras: ExactSizeIterator<Item = (Epoch, &'era EraParameters)>,
{
    // Eras count from 0, so era `n` is in force once the first `n + 1` eras
    // have activated.
    let activated_eras = usize::from(era.into_inner()) + 1;
    if eras.len() < activated_eras {
        return None;
    }
    let era_digests = eras
        .take(activated_eras)
        .map(|(first_epoch, parameters)| EraDigest::compute(first_epoch, parameters));
    Some(ForkDigest::compute(genesis_id, chain_id, era_digests))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use lb_core::era::Era;
    use lb_cryptarchia_engine::{Epoch, era::EraVersion};
    use lb_era_parameters::EraParameters;

    use crate::config::{
        DeploymentSettings,
        deployment::{EraSchedule, UnrunnableDeployment, fork_digest_at_era},
    };

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
        changed
            .genesis_era_parameters_mut()
            .cryptarchia
            .pow_config
            .reward
            .slot_window = changed
            .genesis_era_parameters()
            .cryptarchia
            .pow_config
            .reward
            .slot_window
            .checked_add(1)
            .unwrap();
        assert_ne!(
            changed.genesis_fork_digest(),
            settings.genesis_fork_digest()
        );
    }

    #[test]
    fn scheduling_an_era_changes_no_fork_before_it_activates() {
        // A node whose schedule adds a second era must follow the same fork as
        // a node whose schedule ends at the genesis era, and so speak the same
        // protocols, until the second era activates: protocol names derive
        // from the chain ID and the fork digest alone.
        let settings = DeploymentSettings::default();
        let (genesis_id, chain_id) = (settings.genesis_id(), settings.chain_id());
        let parameters = settings.eras.genesis();
        let one_era = [(Epoch::new(0), parameters)];
        let two_eras = [(Epoch::new(0), parameters), (Epoch::new(100), parameters)];
        let fork_digest = |eras: &[(Epoch, &EraParameters)], era| {
            fork_digest_at_era(genesis_id, &chain_id, eras.iter().copied(), era)
        };
        let second_era = Era::new(1);

        let first_fork = settings.genesis_fork_digest();
        assert_eq!(fork_digest(&one_era, Era::GENESIS), Some(first_fork));
        assert_eq!(fork_digest(&two_eras, Era::GENESIS), Some(first_fork));

        let second_fork = fork_digest(&two_eras, second_era);
        assert_ne!(second_fork, Some(first_fork));
        assert_eq!(fork_digest(&one_era, second_era), None);
        assert!(settings.protocol_names_at_era(second_era).is_none());
    }

    /// The default deployment with a second era, running the same parameters,
    /// from epoch 100.
    fn two_era_settings() -> DeploymentSettings {
        let mut settings = DeploymentSettings::default();
        let genesis = settings.eras.genesis().clone();
        let second = genesis.clone();
        settings.eras = EraSchedule::try_from(BTreeMap::from([
            (Epoch::new(0), genesis),
            (Epoch::new(100), second),
        ]))
        .unwrap();
        settings
    }

    #[test]
    fn the_resolved_schedule_starts_at_genesis_on_the_genesis_fork() {
        // A second era moves neither the start of the genesis era nor the fork
        // it follows.
        let settings = two_era_settings();
        let eras = settings.eras().unwrap();
        let genesis = eras.genesis();
        assert_eq!(genesis.entry.version, EraVersion::V1);
        assert_eq!(
            genesis.entry.parameters.fork_digest,
            DeploymentSettings::default().genesis_fork_digest()
        );
    }

    #[test]
    fn only_single_era_schedules_run() {
        let settings = DeploymentSettings::default();
        let eras = settings.runnable_eras().unwrap();
        assert_eq!(
            eras.genesis()
                .entry
                .parameters
                .protocol_names
                .blend
                .as_ref(),
            settings.genesis_protocol_names().blend.as_ref()
        );

        assert!(matches!(
            two_era_settings().runnable_eras(),
            Err(UnrunnableDeployment::MultipleEras(2))
        ));
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
