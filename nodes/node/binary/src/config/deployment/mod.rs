use core::{num::NonZero, time::Duration};

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
#[cfg(test)]
use lb_cryptarchia_engine::Epoch;
use lb_cryptarchia_engine::{
    Slot,
    era::{EraEntriesAfterGenesis, EraEntry},
};
#[cfg(test)]
use lb_era_parameters::EraChanges;
use lb_era_parameters::{
    EraDefinition, EraParameters, ProtocolNames,
    blend::{BlendParameters, v1 as blend_v1},
    cryptarchia::{CryptarchiaParameters, v1 as cryptarchia_v1},
    time::{TimeParameters, v1 as time_v1},
};
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
    #[cfg(test)]
    #[must_use]
    pub fn fork_digest_at_era(&self, era: Era) -> Option<ForkDigest> {
        let eras = self.eras.resolve().ok()?;
        let declared = eras
            .iter()
            .map(|(first_epoch, changes, _)| (*first_epoch, changes));
        fork_digest_at_era(self.genesis_id(), &self.chain_id(), declared, era)
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
    pub fn protocol_names_in_force(&self) -> Result<ProtocolNames, EraScheduleError> {
        let eras = self.era_schedule()?;
        let now = eras
            .slot_at(time::OffsetDateTime::now_utc())
            .unwrap_or(Slot::genesis());
        Ok(eras.at_slot(now).entry.parameters.protocol_names.clone())
    }

    /// The sections of the genesis era's parameters, each in its version 1,
    /// the only one each has.
    #[must_use]
    pub const fn genesis_era_parameters(&self) -> V1Sections<'_> {
        let EraParameters {
            blend: BlendParameters::V1(blend),
            cryptarchia: CryptarchiaParameters::V1(cryptarchia),
            time: TimeParameters::V1(time),
            blocks: _,
        } = self.eras.genesis();
        V1Sections {
            blend,
            cryptarchia,
            time,
        }
    }

    /// See [`Self::genesis_era_parameters`].
    pub const fn genesis_era_parameters_mut(&mut self) -> V1SectionsMut<'_> {
        let EraParameters {
            blend: BlendParameters::V1(blend),
            cryptarchia: CryptarchiaParameters::V1(cryptarchia),
            time: TimeParameters::V1(time),
            blocks: _,
        } = self.eras.genesis_mut();
        V1SectionsMut {
            blend,
            cryptarchia,
            time,
        }
    }

    #[must_use]
    pub const fn genesis_blend_round_duration(&self) -> Duration {
        let parameters = self.genesis_era_parameters();
        parameters
            .blend
            .round_duration(&parameters.time.slot_duration)
    }

    /// The schedule resolved: each era with its number, the version of its
    /// blocks, its slot duration and epoch length, and its definition: its
    /// parameters, and the digests and protocol names in force while it is.
    ///
    /// # Errors
    ///
    /// If an era's sections cannot run together or its changes cannot follow
    /// the era before it, or if an era starts beyond the slots or the time
    /// this node can represent.
    pub fn era_schedule(
        &self,
    ) -> Result<lb_cryptarchia_engine::era::EraSchedule<EraDefinition>, EraScheduleError> {
        let (genesis_id, chain_id) = (self.genesis_id(), self.chain_id());
        let eras = self.eras.resolve()?;
        let mut era_digests = Vec::with_capacity(eras.len());
        // In activation order: an era's digest is over what it declares, and
        // its fork digest over the digests of the eras up to it.
        let mut entries = eras.into_iter().map(|(first_epoch, declared, parameters)| {
            let digest = EraDigest::compute(first_epoch, &declared);
            era_digests.push(digest);
            let fork_digest =
                ForkDigest::compute(genesis_id, &chain_id, era_digests.iter().copied());
            let entry = EraEntry {
                block_version: parameters.block_version(),
                slot_duration: parameters.slot_duration(),
                epoch_length_in_slots: parameters.epoch_length(),
                transition_slots: parameters.transition_slots(),
                parameters: EraDefinition {
                    parameters,
                    digest,
                    fork_digest,
                    protocol_names: ProtocolNames::derive(&chain_id, fork_digest),
                },
            };
            (first_epoch, entry)
        });
        let (_, genesis) = entries.next().expect("a schedule has a genesis era");
        let after_genesis = entries.map(|(first_epoch, entry)| {
            let first_epoch = NonZero::new(first_epoch.into_inner())
                .expect("an era after the genesis era starts after epoch 0");
            (first_epoch, entry)
        });
        let after_genesis = EraEntriesAfterGenesis::try_from_iter(after_genesis)
            .expect("a schedule has at most `MAX_ERAS_AFTER_GENESIS` eras after genesis");
        Ok(lb_cryptarchia_engine::era::EraSchedule::new(
            self.genesis_time().into(),
            genesis,
            after_genesis,
        )?)
    }
}

/// The sections of an era's parameters, each in its version 1.
pub struct V1Sections<'era> {
    pub blend: &'era blend_v1::Settings,
    pub cryptarchia: &'era cryptarchia_v1::Settings,
    pub time: &'era time_v1::Settings,
}

/// See [`V1Sections`].
pub struct V1SectionsMut<'era> {
    pub blend: &'era mut blend_v1::Settings,
    pub cryptarchia: &'era mut cryptarchia_v1::Settings,
    pub time: &'era mut time_v1::Settings,
}

impl Default for DeploymentSettings {
    fn default() -> Self {
        deserialize_value_from_reader(SERIALIZED_DEPLOYMENT, OnUnknownKeys::Fail)
            .expect("Default deployment settings must be valid.")
    }
}

/// The digest of the fork of the chain `chain_id` from the genesis block
/// `genesis_id` while `era` is in force, given what every era of the chain's
/// schedule declares, in activation order: of the eras up to `era` only.
/// `None` if the schedule has fewer eras.
#[cfg(test)]
fn fork_digest_at_era<'era, Eras>(
    genesis_id: HeaderId,
    chain_id: &ChainId,
    eras: Eras,
    era: Era,
) -> Option<ForkDigest>
where
    Eras: ExactSizeIterator<Item = (Epoch, &'era EraChanges)>,
{
    // Eras count from 0, so era `n` is in force once the first `n + 1` eras
    // have activated.
    let activated_eras = usize::from(era.into_inner()) + 1;
    if eras.len() < activated_eras {
        return None;
    }
    let era_digests = eras
        .take(activated_eras)
        .map(|(first_epoch, declared)| EraDigest::compute(first_epoch, declared));
    Some(ForkDigest::compute(genesis_id, chain_id, era_digests))
}

#[cfg(test)]
mod tests {
    use core::num::NonZero;
    use std::collections::BTreeMap;

    use lb_core::era::Era;
    use lb_cryptarchia_engine::Epoch;
    use lb_era_parameters::{EraChanges, cryptarchia::CryptarchiaParameters};

    use crate::config::{
        DeploymentSettings,
        deployment::{EraSchedule, fork_digest_at_era},
    };

    /// What an era declares that changes the `PoW` slot window of the genesis
    /// era of `settings`, and nothing else.
    fn slot_window_change(settings: &DeploymentSettings) -> EraChanges {
        let mut cryptarchia = settings.genesis_era_parameters().cryptarchia.clone();
        cryptarchia.pow_config.reward.slot_window = cryptarchia
            .pow_config
            .reward
            .slot_window
            .checked_add(1)
            .unwrap();
        EraChanges {
            blend: None,
            blocks: None,
            cryptarchia: Some(CryptarchiaParameters::V1(cryptarchia)),
            time: None,
        }
    }

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
        let genesis = EraChanges::from(settings.eras.genesis().clone());
        let second = slot_window_change(&settings);
        let one_era = [(Epoch::new(0), &genesis)];
        let two_eras = [(Epoch::new(0), &genesis), (Epoch::new(100), &second)];
        let fork_digest = |eras: &[(Epoch, &EraChanges)], era| {
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

    /// The default deployment with a second era from epoch 100, which changes
    /// the `PoW` slot window.
    fn two_era_settings() -> DeploymentSettings {
        let mut settings = DeploymentSettings::default();
        let second = slot_window_change(&settings);
        settings.eras = EraSchedule::new(
            settings.eras.genesis().clone(),
            BTreeMap::from([(NonZero::new(100).unwrap(), second)]),
        )
        .unwrap();
        settings
    }

    #[test]
    fn the_resolved_schedule_starts_at_genesis_on_the_genesis_fork() {
        // A second era moves neither the start of the genesis era nor the fork
        // it follows.
        let settings = two_era_settings();
        let eras = settings.era_schedule().unwrap();
        let genesis = eras.genesis();
        assert_eq!(
            genesis.entry.parameters.fork_digest,
            DeploymentSettings::default().genesis_fork_digest()
        );
    }

    #[test]
    fn every_era_of_a_schedule_runs_on_a_fork_of_its_own() {
        let settings = two_era_settings();
        let eras = settings.era_schedule().unwrap();
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
