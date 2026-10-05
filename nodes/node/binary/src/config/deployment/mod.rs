use core::time::Duration;

#[cfg(test)]
use lb_core::era::Era;
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
    era::{EraEntriesAfterGenesis, EraEntry, Eras, ErasError},
};
use lb_era_parameters::{EraDefinition, EraParameters, ProtocolNames, v1};
use lb_utils::yaml::{OnUnknownKeys, deserialize_value_from_reader};
use serde::{Deserialize, Serialize};

pub mod era;
use era::GENESIS_EPOCH;
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
    #[cfg(test)]
    #[must_use]
    pub fn fork_digest_at_era(&self, era: Era) -> Option<ForkDigest> {
        fork_digest_at_era(self.genesis_id(), &self.chain_id(), self.eras.iter(), era)
    }

    #[cfg(test)]
    #[must_use]
    pub fn genesis_fork_digest(&self) -> ForkDigest {
        self.fork_digest_at_era(Era::GENESIS)
            .expect("every era schedule has a genesis era")
    }

    /// The protocol and topic names of this deployment's chain while `era` is
    /// in force, derived from its chain ID and from the fork digest of `era`.
    /// `None` if the schedule has no era `era`.
    #[cfg(test)]
    #[must_use]
    pub fn protocol_names_at_era(&self, era: Era) -> Option<ProtocolNames> {
        self.fork_digest_at_era(era)
            .map(|fork_digest| ProtocolNames::derive(&self.chain_id(), fork_digest))
    }

    /// The protocol and topic names of this deployment's chain now: those of
    /// the era in force by the wall clock.
    pub fn protocol_names_in_force(&self) -> Result<ProtocolNames, ErasError> {
        let eras = self.eras()?;
        let now = eras
            .slot_at(time::OffsetDateTime::now_utc())
            .unwrap_or(Slot::genesis());
        Ok(eras.at_slot(now).entry.parameters.protocol_names.clone())
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
        let mut era_digests = Vec::with_capacity(self.eras.after_genesis().len() + 1);
        // Called in activation order: the fork digest of an era is over the
        // digests of the eras up to it.
        let mut entry = |first_epoch: Epoch, parameters: &EraParameters| {
            let digest = EraDigest::compute(first_epoch, parameters);
            era_digests.push(digest);
            let fork_digest =
                ForkDigest::compute(genesis_id, &chain_id, era_digests.iter().copied());
            EraEntry {
                version: parameters.version(),
                slot_duration: parameters.slot_duration(),
                epoch_length_in_slots: parameters.epoch_length(),
                transition_slots: parameters.transition_slots(),
                parameters: EraDefinition {
                    parameters: parameters.clone(),
                    digest,
                    fork_digest,
                    protocol_names: ProtocolNames::derive(&chain_id, fork_digest),
                },
            }
        };
        let genesis = entry(GENESIS_EPOCH, self.eras.genesis());
        let after_genesis = self
            .eras
            .after_genesis()
            .iter()
            .map(|(&first_epoch, parameters)| {
                (
                    first_epoch,
                    entry(Epoch::new(first_epoch.get()), parameters),
                )
            });
        let after_genesis = EraEntriesAfterGenesis::try_from_iter(after_genesis)
            .expect("a schedule has at most `MAX_ERAS_AFTER_GENESIS` eras after genesis");
        Eras::new(self.genesis_time().into(), genesis, after_genesis)
    }
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
#[cfg(test)]
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
        deployment::{EraSchedule, fork_digest_at_era},
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
    fn every_era_of_a_schedule_runs_on_a_fork_of_its_own() {
        let settings = two_era_settings();
        let eras = settings.eras().unwrap();
        let [genesis, second] = [Era::GENESIS, Era::new(1)].map(|era| eras.get(era).unwrap());

        assert_eq!(second.first_epoch, Epoch::new(100));
        assert_ne!(
            genesis.entry.parameters.fork_digest,
            second.entry.parameters.fork_digest
        );
        assert_ne!(
            genesis.entry.parameters.protocol_names.blend.as_ref(),
            second.entry.parameters.protocol_names.blend.as_ref()
        );
        assert_eq!(
            second.entry.parameters.protocol_names.blend.as_ref(),
            settings
                .protocol_names_at_era(Era::new(1))
                .unwrap()
                .blend
                .as_ref()
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
