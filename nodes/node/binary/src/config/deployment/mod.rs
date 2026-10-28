use core::{num::NonZero, time::Duration};
use std::collections::BTreeMap;

use lb_core::{
    block::genesis::GenesisBlock,
    era::{EraDigest, ForkDigest},
    header::HeaderId,
    mantle::{
        traits::GenesisTx as _,
        transactions::genesis_tx::{ChainId, GenesisTime},
    },
};
use lb_time::{
    Epoch, Slot,
    era::{EpochLength, EraEntriesAfterGenesis, SlotDuration},
};
use lb_utils::yaml::{OnUnknownKeys, deserialize_embedded_value};
use serde::{Deserialize, Serialize};

pub mod era;
use era::GENESIS_EPOCH;
pub use era::{EraDeclarations, EraScheduleError};

use crate::config::deployment::era::ruleset::{EraRuleset, v1};

/// The main file of the default deployment, which includes each of its eras
/// from its own file.
const SERIALIZED_DEPLOYMENT: &str = include_str!("settings.yaml");
/// The era files the default deployment includes, each keyed by the path its
/// main file includes it by: every `.yaml` file in `era/schedule/`, as the
/// build script lists them.
const SERIALIZED_ERAS: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/serialized_eras.rs"));

type EraSchedule = lb_time::era::EraSchedule<EraDefinition>;

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
    /// # Panics
    ///
    /// If an era starts beyond the slots or the time this node can represent.
    #[must_use]
    pub fn new(eras: &EraDeclarations, genesis_block: GenesisBlock) -> Self {
        let (genesis_id, genesis_time) = (
            genesis_block.header().id(),
            genesis_block.genesis_tx().genesis_parameters().start_time,
        );
        let eras_after_genesis = eras.eras_after_genesis();
        let mut era_digests = Vec::with_capacity(eras_after_genesis.len() + 1);
        // Called in activation order: the fork digest of an era is over the
        // digests of the eras up to it.
        let mut definition = |first_epoch: Epoch, ruleset: &EraRuleset| {
            let digest = EraDigest::compute(first_epoch, ruleset);
            era_digests.push(digest);
            let fork_digest = ForkDigest::compute(genesis_id, era_digests.iter().copied());
            EraDefinition {
                ruleset: ruleset.clone(),
                digest,
                fork_digest,
            }
        };
        let genesis = definition(GENESIS_EPOCH, eras.genesis_era());
        let after_genesis = EraEntriesAfterGenesis::try_from_iter(eras_after_genesis.iter().map(
            |(&first_epoch, ruleset)| {
                (
                    first_epoch,
                    definition(Epoch::new(first_epoch.get()), ruleset),
                )
            },
        ))
        .expect("The resolved eras are as many as the declared ones.");
        let eras = EraSchedule::new(genesis_time.into(), genesis, after_genesis).unwrap();
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
            .genesis_parameters()
            .chain_id
    }

    /// When this deployment's chain starts, read off the genesis inscription.
    #[must_use]
    pub fn genesis_time(&self) -> GenesisTime {
        self.genesis_block
            .genesis_tx()
            .genesis_parameters()
            .start_time
    }

    /// The ID of the genesis block this deployment's chain starts from.
    #[must_use]
    pub fn genesis_id(&self) -> HeaderId {
        self.genesis_block.header().id()
    }

    /// The fork digest of the era in force by the wall clock, which names the
    /// fork-bound protocols of this deployment's chain now: the genesis era's
    /// before genesis.
    #[must_use]
    pub fn fork_digest_in_force(&self) -> ForkDigest {
        self.eras
            .at_slot(self.slot_now().unwrap_or(Slot::genesis()))
            .parameters
            .fork_digest
    }

    fn slot_now(&self) -> Option<Slot> {
        self.eras.slot_at(time::OffsetDateTime::now_utc())
    }

    /// The parameters of the genesis era, in version 1's layout, the only one
    /// there is.
    #[must_use]
    pub const fn genesis_era_parameters(&self) -> &v1::Parameters {
        match &self.eras.genesis().parameters.ruleset {
            EraRuleset::V1(parameters) => parameters,
        }
    }

    /// Changes the parameters of the genesis era with `update`, and resolves
    /// the schedule again, so the digests of every era follow the change.
    ///
    /// # Panics
    ///
    /// If the change makes a later era start beyond the slots or the time this
    /// node can represent.
    pub fn update_genesis_era_parameters<Update>(&mut self, update: Update)
    where
        Update: FnOnce(&mut v1::Parameters),
    {
        let EraDefinitions {
            mut eras,
            genesis_block,
        } = EraDefinitions::from(self.clone());
        let EraRuleset::V1(parameters) = eras.genesis_era_mut();
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

/// The eras as the file declares them: each era's ruleset, keyed by the epoch
/// it starts at.
impl From<DeploymentSettings> for EraDefinitions {
    fn from(
        DeploymentSettings {
            eras,
            genesis_block,
        }: DeploymentSettings,
    ) -> Self {
        let declarations = eras
            .iter()
            .map(|era| (era.first_epoch, era.parameters.ruleset.clone()))
            .collect::<BTreeMap<_, _>>();
        Self {
            eras: EraDeclarations::try_from(declarations)
                .expect("A resolved schedule declares its eras from genesis on."),
            genesis_block,
        }
    }
}

/// An era as the node runs it: its ruleset, its digest, and the fork digest in
/// force while it is, which names its protocols.
#[derive(Clone, Debug)]
pub struct EraDefinition {
    pub ruleset: EraRuleset,
    pub digest: EraDigest,
    /// The digest of the eras up to this one, in activation order.
    pub fork_digest: ForkDigest,
}

impl SlotDuration for EraDefinition {
    fn slot_duration(&self) -> Duration {
        self.ruleset.slot_duration()
    }
}

impl EpochLength for EraDefinition {
    fn epoch_length(&self) -> NonZero<u64> {
        self.ruleset.epoch_length()
    }
}

impl Default for DeploymentSettings {
    fn default() -> Self {
        deserialize_embedded_value(SERIALIZED_DEPLOYMENT, SERIALIZED_ERAS, OnUnknownKeys::Fail)
            .expect("Default deployment settings must be valid.")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use lb_core::era::ForkDigest;
    use lb_time::{Epoch, Era};
    use lb_utils::yaml::INCLUDE_TAG;
    use serde_yaml::Value;

    use super::{EraDeclarations, EraDefinitions, SERIALIZED_DEPLOYMENT, SERIALIZED_ERAS};
    use crate::config::DeploymentSettings;

    /// The default deployment, with a second era from epoch 10 that runs the
    /// genesis era's ruleset.
    fn two_eras() -> DeploymentSettings {
        let EraDefinitions {
            eras,
            genesis_block,
        } = DeploymentSettings::default().into();
        let ruleset = eras.genesis_era().clone();
        let eras = EraDeclarations::try_from(BTreeMap::from([
            (Epoch::new(0), ruleset.clone()),
            (Epoch::new(10), ruleset),
        ]))
        .unwrap();
        DeploymentSettings::new(&eras, genesis_block)
    }

    #[test]
    fn default_initialization() {
        drop(DeploymentSettings::default());
    }

    #[test]
    fn the_default_deployment_includes_each_era_file_by_its_era_number() {
        let main: Value = serde_yaml::from_str(SERIALIZED_DEPLOYMENT).unwrap();
        let mut includes_by_first_epoch: Vec<(u64, &str)> = main["eras"]
            .as_mapping()
            .unwrap()
            .iter()
            .map(|(first_epoch, era)| match era {
                Value::Tagged(include) if include.tag == INCLUDE_TAG => (
                    first_epoch.as_u64().unwrap(),
                    include.value.as_str().unwrap(),
                ),
                other => panic!("each era is included from its own file, not {other:?}"),
            })
            .collect();
        includes_by_first_epoch.sort_unstable();
        let included: Vec<&str> = includes_by_first_epoch
            .into_iter()
            .map(|(_, path)| path)
            .collect();

        // Eras are numbered in the order they start.
        let era_files: Vec<String> = (0..included.len())
            .map(|era_number| format!("era/schedule/{era_number}.yaml"))
            .collect();
        assert_eq!(included, era_files);
        // The build script embeds every era file in the directory.
        let embedded: BTreeSet<&str> = SERIALIZED_ERAS.iter().map(|(path, _)| *path).collect();
        assert_eq!(embedded, included.into_iter().collect());
    }

    #[test]
    fn serialize_deserialize_yaml() {
        let settings = DeploymentSettings::default();
        let as_str = serde_yaml::to_string(&settings).unwrap();
        let _recovered: DeploymentSettings = serde_yaml::from_str(&as_str).unwrap();
    }

    #[test]
    fn every_era_survives_a_round_trip_through_yaml() {
        // Not pinned to values: the default deployment changes at every
        // genesis ceremony. What must hold is that the digests depend only on
        // the settings, not on how they were loaded.
        let settings = two_eras();
        let recovered: DeploymentSettings =
            serde_yaml::from_str(&serde_yaml::to_string(&settings).unwrap()).unwrap();
        let fork_digests = |settings: &DeploymentSettings| {
            settings
                .era_schedule()
                .iter()
                .map(|era| era.parameters.fork_digest)
                .collect::<Vec<_>>()
        };
        assert_eq!(fork_digests(&settings).len(), 2);
        assert_eq!(fork_digests(&recovered), fork_digests(&settings));
    }

    #[test]
    fn each_era_has_the_fork_digest_of_the_eras_up_to_it() {
        let settings = two_eras();
        let schedule = settings.era_schedule();
        let (genesis, second) = (schedule.genesis(), schedule.get(Era::new(1)).unwrap());

        assert_eq!(second.first_epoch, Epoch::new(10));
        // A later era leaves the fork digests of the eras before it unchanged.
        assert_eq!(
            genesis.parameters.fork_digest,
            DeploymentSettings::default()
                .era_schedule()
                .genesis()
                .parameters
                .fork_digest
        );
        assert_eq!(
            second.parameters.fork_digest,
            ForkDigest::compute(
                settings.genesis_id(),
                [genesis.parameters.digest, second.parameters.digest].into_iter()
            )
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
        let genesis_fork_digest = |settings: &DeploymentSettings| {
            settings.era_schedule().genesis().parameters.fork_digest
        };
        assert_ne!(
            genesis_fork_digest(&changed),
            genesis_fork_digest(&settings)
        );
    }
}
