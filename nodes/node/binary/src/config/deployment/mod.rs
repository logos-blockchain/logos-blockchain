use core::{num::NonZero, time::Duration};
use std::collections::BTreeMap;

use lb_chain_service::TipEra;
use lb_core::{
    block::genesis::GenesisBlock,
    era::{AcceptedForkDigests, EraDigest, ForkDigest},
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

use crate::config::{
    deployment::era::ruleset::{EraRuleset, v1},
    time::check_transition_periods,
};

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
/// block, and the eras are checked and resolved as it is read: loading a
/// schedule that does not resolve, or whose eras cannot run, panics.
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
    /// If an era's values cannot run, alone or after the era before it, an era
    /// starts beyond the slots or the time this node can represent, or an era
    /// ends within the transition period it opens.
    #[must_use]
    pub fn new(eras: &EraDeclarations, genesis_block: GenesisBlock) -> Self {
        // Before resolving the eras, which reads their epoch lengths.
        eras.validate();
        let (genesis_id, genesis_time) = (
            genesis_block.header().id(),
            genesis_block.genesis_tx().genesis_parameters().start_time,
        );
        let eras_after_genesis = eras.eras_after_genesis();
        let mut era_digests = Vec::with_capacity(eras_after_genesis.len() + 1);
        let mut accepted_fork_digests = Vec::with_capacity(eras_after_genesis.len() + 1);
        let mut previous_ruleset: Option<EraRuleset> = None;
        // Called in activation order: the fork digest of an era is over the
        // digests of the eras up to it, and an era accepts the fork digests
        // of the eras before it until one refuses the transactions of those.
        let mut definition = |first_epoch: Epoch, ruleset: &EraRuleset| {
            let digest = EraDigest::compute(first_epoch, ruleset);
            era_digests.push(digest);
            let fork_digest = ForkDigest::compute(genesis_id, era_digests.iter().copied());
            if previous_ruleset
                .as_ref()
                .is_some_and(|previous| ruleset.refuses_earlier_transactions(previous))
            {
                accepted_fork_digests.clear();
            }
            accepted_fork_digests.push(fork_digest);
            previous_ruleset = Some(ruleset.clone());
            EraDefinition {
                ruleset: ruleset.clone(),
                digest,
                fork_digest,
                accepted_fork_digests: AcceptedForkDigests::new(accepted_fork_digests.clone()),
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
        let eras = EraSchedule::new(genesis_time.into(), genesis, after_genesis)
            .unwrap_or_else(|error| panic!("The eras cannot be resolved: {error}."));
        check_transition_periods(&eras);
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

    /// Checks that this deployment gives the slot of the node's saved tip the
    /// fork digest the tip was applied under.
    ///
    /// # Panics
    ///
    /// If it gives the slot another fork digest: the deployment changes an era
    /// the node has already executed, so the node would run on state executed
    /// under other rules.
    pub fn assert_keeps_tip_era(&self, tip_era: TipEra) {
        let TipEra { slot, fork_digest } = tip_era;
        let scheduled = self.eras.at_slot(slot).parameters.fork_digest;
        assert_eq!(
            scheduled,
            fork_digest,
            "The node's tip, at slot {}, was applied under fork {fork_digest}, but this deployment \
             schedules that slot under fork {scheduled}: it changes an era the node has already \
             executed. Run the release the node ran with, or remove the node's state and sync \
             again.",
            slot.into_inner()
        );
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
    /// If [`Self::new`] does on the changed deployment.
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
    /// The fork digests a transaction may carry in this era.
    pub accepted_fork_digests: AcceptedForkDigests,
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
    use core::num::NonZero;
    use std::collections::{BTreeMap, BTreeSet};

    use lb_chain_service::TipEra;
    use lb_core::era::{AcceptedForkDigests, ForkDigest};
    use lb_time::{Epoch, Era, Slot, era::EpochLength as _};
    use lb_utils::{math::NonNegativeRatio, yaml::INCLUDE_TAG};
    use serde_yaml::Value;

    use super::{EraDeclarations, EraDefinitions, SERIALIZED_DEPLOYMENT, SERIALIZED_ERAS};
    use crate::config::{
        DeploymentSettings,
        deployment::era::ruleset::{
            EraRuleset,
            v1::{self, cryptarchia::EpochConfig},
        },
    };

    /// The default deployment's genesis block, starting `eras`, each keyed by
    /// its first epoch.
    fn deployment(eras: impl IntoIterator<Item = (u32, EraRuleset)>) -> DeploymentSettings {
        let EraDefinitions { genesis_block, .. } = DeploymentSettings::default().into();
        let eras = EraDeclarations::try_from(
            eras.into_iter()
                .map(|(first_epoch, ruleset)| (Epoch::new(first_epoch), ruleset))
                .collect::<BTreeMap<_, _>>(),
        )
        .unwrap();
        DeploymentSettings::new(&eras, genesis_block)
    }

    /// The default deployment's genesis era, changed by `change`.
    fn ruleset(change: impl FnOnce(&mut v1::Parameters)) -> EraRuleset {
        let mut parameters = DeploymentSettings::default()
            .genesis_era_parameters()
            .clone();
        change(&mut parameters);
        EraRuleset::V1(parameters)
    }

    /// The default deployment, with a second era from epoch 10 that runs the
    /// genesis era's ruleset.
    fn two_eras() -> DeploymentSettings {
        let genesis = ruleset(|_| ());
        deployment([(0, genesis.clone()), (10, genesis)])
    }

    /// A genesis era at f = 1/20, whose transition period lasts ⌊1/f⌋ = 20 of
    /// its one-second slots, then two eras of 12-slot epochs (3 phases of
    /// ⌊k/f⌋ = 4 slots), from epoch 10 and from `third_era_epoch`.
    fn eras_after_a_transition_period(third_era_epoch: u32) -> DeploymentSettings {
        let genesis = ruleset(|parameters| {
            parameters.cryptarchia.slot_activation_coeff =
                NonNegativeRatio::new(1, NonZero::new(20).unwrap())
                    .try_into()
                    .unwrap();
        });
        let short_epochs = ruleset(|parameters| {
            let cryptarchia = &mut parameters.cryptarchia;
            cryptarchia.security_param = NonZero::new(2).unwrap();
            cryptarchia.slot_activation_coeff = NonNegativeRatio::new(1, NonZero::new(2).unwrap())
                .try_into()
                .unwrap();
            cryptarchia.uncle_reference_window_in_block = 1;
            cryptarchia.epoch_config = EpochConfig {
                epoch_stake_distribution_stabilization: NonZero::new(1).unwrap(),
                epoch_period_nonce_buffer: NonZero::new(1).unwrap(),
                epoch_period_nonce_stabilization: NonZero::new(1).unwrap(),
            };
        });
        deployment([
            (0, genesis),
            (10, short_epochs.clone()),
            (third_era_epoch, short_epochs),
        ])
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
    fn an_era_that_outlasts_the_transition_period_it_opens_is_accepted() {
        // Era 1 lasts 24 slots.
        drop(eras_after_a_transition_period(12));
    }

    #[test]
    #[should_panic(expected = "Era 1 lasts 12 slots, not more than the 20 slots")]
    fn an_era_that_ends_within_the_transition_period_it_opens_is_refused() {
        drop(eras_after_a_transition_period(11));
    }

    #[test]
    fn a_deployment_that_appends_an_era_keeps_the_era_of_the_tip() {
        let default = DeploymentSettings::default();
        let genesis_era = default.era_schedule().genesis();
        // In epoch 5, which both deployments give to the genesis era.
        let tip_era = TipEra {
            slot: Slot::new(5 * genesis_era.parameters.epoch_length().get()),
            fork_digest: genesis_era.parameters.fork_digest,
        };
        two_eras().assert_keeps_tip_era(tip_era);
    }

    #[test]
    #[should_panic(expected = "it changes an era the node has already executed")]
    fn a_deployment_that_changes_the_era_of_the_tip_is_refused() {
        let two_eras = two_eras();
        let era_1 = two_eras.era_schedule().get(Era::new(1)).unwrap();
        // A tip in era 1, a slot the default deployment gives to the genesis era.
        let tip_era = TipEra {
            slot: era_1.first_slot,
            fork_digest: era_1.parameters.fork_digest,
        };
        DeploymentSettings::default().assert_keeps_tip_era(tip_era);
    }

    #[test]
    fn an_era_accepts_the_fork_digests_of_the_eras_before_it() {
        let two_eras = two_eras();
        let genesis_era = two_eras.era_schedule().genesis().parameters;
        let era_1 = two_eras.era_schedule().get(Era::new(1)).unwrap().parameters;

        assert_eq!(
            genesis_era.accepted_fork_digests,
            AcceptedForkDigests::new(vec![genesis_era.fork_digest])
        );
        assert_eq!(
            era_1.accepted_fork_digests,
            AcceptedForkDigests::new(vec![genesis_era.fork_digest, era_1.fork_digest])
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
