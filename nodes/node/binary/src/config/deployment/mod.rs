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
use lb_libp2p::protocol_name::ProtocolScope;
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
    deployment::era::ruleset::{EraRuleset, v2},
    time::check_transition_periods,
};

/// Why a deployment cannot run: an era whose values the node refuses, alone or
/// after the era before it, eras that cannot be resolved, or a change to an
/// era the node has already executed.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct InvalidDeployment(String);

impl InvalidDeployment {
    pub(crate) const fn new(reason: String) -> Self {
        Self(reason)
    }
}

/// Returns an [`InvalidDeployment`] for the reason the remaining arguments
/// format unless `condition` holds: the `assert!` of the deployment's checks.
macro_rules! ensure {
    ($condition:expr, $($reason:tt)+) => {
        if !$condition {
            return Err($crate::config::deployment::InvalidDeployment::new(format!($($reason)+)));
        }
    };
}
pub(crate) use ensure;

/// The main file of the default deployment, which includes each of its eras
/// from its own file.
const SERIALIZED_DEPLOYMENT: &str = include_str!("settings.yaml");
/// The era files the default deployment includes, each keyed by the path its
/// main file includes it by: every `.yaml` file in `era/schedule/`, as the
/// build script lists them.
const SERIALIZED_ERAS: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/serialized_eras.rs"));

/// The main file of the testnet release 0.3.1 started, and the era files it
/// includes.
const SERIALIZED_TESTNET_DEPLOYMENT: &str = include_str!("testnet/settings.yaml");
const SERIALIZED_TESTNET_ERAS: &[(&str, &str)] = &[
    (
        "era/schedule/0.yaml",
        include_str!("testnet/era/schedule/0.yaml"),
    ),
    (
        "era/schedule/14.yaml",
        include_str!("testnet/era/schedule/14.yaml"),
    ),
];

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
/// schedule that does not resolve, or whose eras cannot run, fails.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(try_from = "EraDefinitions", into = "EraDefinitions")]
pub struct DeploymentSettings {
    eras: EraSchedule,
    genesis_block: GenesisBlock,
}

impl DeploymentSettings {
    /// The testnet release 0.3.1 started: its genesis block, and its genesis
    /// era under the rules of releases 0.3.1 to 0.3.6.
    #[must_use]
    pub fn testnet() -> Self {
        deserialize_embedded_value(
            SERIALIZED_TESTNET_DEPLOYMENT,
            SERIALIZED_TESTNET_ERAS,
            OnUnknownKeys::Fail,
        )
        .expect("The testnet deployment settings must be valid.")
    }

    /// The deployment whose chain starts from `genesis_block` and runs the eras
    /// `eras` declares, resolved: where each era starts, and its digest and
    /// fork digest.
    ///
    /// # Errors
    ///
    /// If an era's values cannot run, alone or after the era before it, an era
    /// starts beyond the slots or the time this node can represent, or an era
    /// ends within the transition period it opens.
    pub fn new(
        eras: &EraDeclarations,
        genesis_block: GenesisBlock,
    ) -> Result<Self, InvalidDeployment> {
        // Before resolving the eras, which reads their epoch lengths.
        eras.validate()?;
        let genesis_block = with_genesis_era_parameters(genesis_block, eras.genesis_era())?;
        let (genesis_id, genesis_time, chain_id) = {
            let genesis_parameters = genesis_block.genesis_tx().genesis_parameters();
            (
                genesis_block.header().id(),
                genesis_parameters.start_time,
                genesis_parameters.chain_id,
            )
        };
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
            // An era keeps the fork digest the releases that activated it gave
            // it, and with it the names of its protocols.
            let fork_digest = if ruleset.is_legacy() {
                ForkDigest::compute_legacy(genesis_id, &chain_id, era_digests.iter().copied())
            } else {
                ForkDigest::compute(genesis_id, era_digests.iter().copied())
            };
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
                // The transactions of a legacy era carry no fork digest.
                accepted_fork_digests: if ruleset.is_legacy() {
                    AcceptedForkDigests::NoneCarried
                } else {
                    AcceptedForkDigests::new(accepted_fork_digests.clone())
                },
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
        let eras =
            EraSchedule::new(genesis_time.into(), genesis, after_genesis).map_err(|error| {
                InvalidDeployment::new(format!("The eras cannot be resolved: {error}."))
            })?;
        check_transition_periods(&eras)?;
        Ok(Self {
            eras,
            genesis_block,
        })
    }

    /// What the names of Kademlia and identify are bound to: this deployment's
    /// chain, on every fork of it.
    ///
    /// The genesis block ID, but for a chain with a legacy genesis era, which
    /// keeps the names the releases that started it gave it: bound to the
    /// chain ID its genesis block inscribes.
    #[must_use]
    pub fn chain_scope(&self) -> ProtocolScope {
        if self.eras.genesis().parameters.ruleset.is_legacy() {
            ProtocolScope::LegacyChain(self.chain_id())
        } else {
            ProtocolScope::Chain(self.genesis_id())
        }
    }

    /// The first slot whose transactions are bound to a fork: the first slot
    /// of the era that follows the legacy era, the first slot of the chain if
    /// it has no legacy era, and none while the legacy era is the last.
    #[must_use]
    pub fn first_master_slot(&self) -> Slot {
        self.eras
            .iter()
            .find(|era| !era.parameters.ruleset.is_legacy())
            .map_or(Slot::new(u64::MAX), |era| era.first_slot)
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
    /// # Errors
    ///
    /// If it gives the slot another fork digest: the deployment starts from
    /// another genesis block or changes an era the node has already executed,
    /// so the node would run on state executed under other rules.
    pub fn check_keeps_tip_era(&self, tip_era: TipEra) -> Result<(), InvalidDeployment> {
        let TipEra { slot, fork_digest } = tip_era;
        let scheduled = self.eras.at_slot(slot).parameters.fork_digest;
        ensure!(
            scheduled == fork_digest,
            "The node's tip, at slot {}, was applied under fork {fork_digest}, but this deployment \
             schedules that slot under fork {scheduled}: it starts from another genesis block or \
             changes an era the node has already executed. Run the release the node ran with, or \
             remove the node's state and sync again.",
            slot.into_inner()
        );
        Ok(())
    }

    /// The parameters of the genesis era, in the latest ruleset's layout.
    #[must_use]
    pub const fn genesis_era_parameters(&self) -> &v2::Parameters {
        self.eras.genesis().parameters.ruleset.parameters()
    }

    /// Changes the parameters of the genesis era with `update`, and resolves
    /// the schedule again, so the digests of every era follow the change.
    ///
    /// # Errors
    ///
    /// If [`Self::new`] does on the changed deployment, leaving this one as it
    /// was, or if the genesis era runs an earlier ruleset, whose parameters
    /// are declared in another layout.
    pub fn update_genesis_era_parameters<Update>(
        &mut self,
        update: Update,
    ) -> Result<(), InvalidDeployment>
    where
        Update: FnOnce(&mut v2::Parameters),
    {
        let EraDefinitions {
            mut eras,
            genesis_block,
        } = EraDefinitions::from(self.clone());
        let EraRuleset::V2(parameters) = eras.genesis_era_mut() else {
            return Err(InvalidDeployment::new(
                "The genesis era runs an earlier ruleset, whose parameters cannot be changed in \
                 the latest ruleset's layout."
                    .to_owned(),
            ));
        };
        update(parameters);
        *self = Self::new(&eras, genesis_block)?;
        Ok(())
    }

    #[must_use]
    pub const fn genesis_blend_round_duration(&self) -> Duration {
        self.genesis_era_parameters().blend_round_duration()
    }
}

/// `genesis_block`, with the genesis parameters the genesis era declares in
/// its place if it runs the legacy ruleset, whose genesis block does not
/// inscribe them all.
///
/// # Errors
///
/// If the genesis block is not of the genesis era's ruleset: a legacy era
/// starts from a legacy genesis transaction, and no other era does.
fn with_genesis_era_parameters(
    genesis_block: GenesisBlock,
    genesis_era: &EraRuleset,
) -> Result<GenesisBlock, InvalidDeployment> {
    let legacy_genesis = genesis_block.genesis_tx().is_legacy();
    match genesis_era {
        EraRuleset::V1(parameters) => {
            ensure!(
                legacy_genesis,
                "The genesis era runs the legacy ruleset, but the genesis transaction carries a \
                 fork digest, which no transaction of that ruleset does."
            );
            genesis_block
                .with_0_3_1_uninscribed_params(
                    parameters.pow_reward_pool(),
                    parameters.consensus_excluded_keys(),
                )
                .map_err(|error| InvalidDeployment::new(error.to_string()))
        }
        EraRuleset::V2(_) => {
            ensure!(
                !legacy_genesis,
                "The genesis transaction carries no fork digest, which only the legacy ruleset \
                 allows, but the genesis era runs another."
            );
            Ok(genesis_block)
        }
    }
}

impl TryFrom<EraDefinitions> for DeploymentSettings {
    type Error = InvalidDeployment;

    fn try_from(
        EraDefinitions {
            eras,
            genesis_block,
        }: EraDefinitions,
    ) -> Result<Self, Self::Error> {
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

impl EraDefinition {
    /// The fork digest the transactions made in the era carry: none in a
    /// legacy era, whose transactions carry no fork digest.
    #[must_use]
    pub const fn tx_fork_digest(&self) -> Option<ForkDigest> {
        if self.ruleset.is_legacy() {
            None
        } else {
            Some(self.fork_digest)
        }
    }
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
    use lb_core::{
        block::genesis::GenesisBlock,
        era::{AcceptedForkDigests, ForkDigest},
        mantle::traits::{GenesisTx as _, Hashable as _},
    };
    use lb_libp2p::protocol_name::ProtocolScope;
    use lb_time::{Epoch, Era, Slot, era::EpochLength as _};
    use lb_utils::{math::NonNegativeRatio, yaml::INCLUDE_TAG};
    use serde_yaml::Value;
    use time::OffsetDateTime;

    use super::{
        EraDeclarations, EraDefinitions, InvalidDeployment, SERIALIZED_DEPLOYMENT, SERIALIZED_ERAS,
    };
    use crate::config::{
        DeploymentSettings,
        deployment::era::ruleset::{
            EraRuleset,
            v2::{self, cryptarchia::EpochConfig},
        },
    };

    /// The default deployment's genesis block, starting `eras`, each keyed by
    /// its first epoch.
    fn deployment(
        eras: impl IntoIterator<Item = (u32, EraRuleset)>,
    ) -> Result<DeploymentSettings, InvalidDeployment> {
        let EraDefinitions { genesis_block, .. } = DeploymentSettings::default().into();
        deployment_from(genesis_block, eras)
    }

    /// `genesis_block`, starting `eras`, each keyed by its first epoch.
    fn deployment_from(
        genesis_block: GenesisBlock,
        eras: impl IntoIterator<Item = (u32, EraRuleset)>,
    ) -> Result<DeploymentSettings, InvalidDeployment> {
        let eras = EraDeclarations::try_from(
            eras.into_iter()
                .map(|(first_epoch, ruleset)| (Epoch::new(first_epoch), ruleset))
                .collect::<BTreeMap<_, _>>(),
        )
        .unwrap();
        DeploymentSettings::new(&eras, genesis_block)
    }

    /// The default deployment's genesis era, changed by `change`.
    fn ruleset(change: impl FnOnce(&mut v2::Parameters)) -> EraRuleset {
        let mut parameters = DeploymentSettings::default()
            .genesis_era_parameters()
            .clone();
        change(&mut parameters);
        EraRuleset::V2(parameters)
    }

    /// The default deployment, with a second era from epoch 10 that runs the
    /// genesis era's ruleset.
    fn two_eras() -> DeploymentSettings {
        let genesis = ruleset(|_| ());
        deployment([(0, genesis.clone()), (10, genesis)]).unwrap()
    }

    /// A genesis era at f = 1/20, whose transition period lasts ⌊1/f⌋ = 20 of
    /// its one-second slots, then two eras of 12-slot epochs (3 phases of
    /// ⌊k/f⌋ = 4 slots), from epoch 10 and from `third_era_epoch`.
    fn eras_after_a_transition_period(
        third_era_epoch: u32,
    ) -> Result<DeploymentSettings, InvalidDeployment> {
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
        drop(eras_after_a_transition_period(12).unwrap());
    }

    #[test]
    fn an_era_that_ends_within_the_transition_period_it_opens_is_refused() {
        let reason = eras_after_a_transition_period(11).unwrap_err().to_string();
        assert!(
            reason.contains("Era 1 lasts 12 slots, not more than the 20 slots"),
            "{reason}"
        );
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
        two_eras().check_keeps_tip_era(tip_era).unwrap();
    }

    #[test]
    fn a_deployment_that_changes_the_era_of_the_tip_is_refused() {
        let two_eras = two_eras();
        let era_1 = two_eras.era_schedule().get(Era::new(1)).unwrap();
        // A tip in era 1, a slot the default deployment gives to the genesis
        // era.
        let tip_era = TipEra {
            slot: era_1.first_slot,
            fork_digest: era_1.parameters.fork_digest,
        };
        let reason = DeploymentSettings::default()
            .check_keeps_tip_era(tip_era)
            .unwrap_err()
            .to_string();
        assert!(
            reason.contains("changes an era the node has already executed"),
            "{reason}"
        );
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

    /// The testnet release 0.3.1 started, followed from epoch `first_epoch` by
    /// an era of the default deployment's ruleset.
    fn testnet_then_latest_ruleset(first_epoch: u32) -> DeploymentSettings {
        let EraDefinitions {
            eras,
            genesis_block,
        } = DeploymentSettings::testnet().into();
        let eras = EraDeclarations::try_from(BTreeMap::from([
            (Epoch::new(0), eras.genesis_era().clone()),
            (Epoch::new(first_epoch), ruleset(|_| ())),
        ]))
        .unwrap();
        DeploymentSettings::new(&eras, genesis_block).unwrap()
    }

    // The values the tests below pin were printed by release 0.3.6 for its
    // default deployment, not by this release.

    #[test]
    fn the_testnet_starts_from_the_genesis_block_of_release_0_3() {
        let testnet = DeploymentSettings::testnet();
        assert_eq!(
            testnet.genesis_id().to_string(),
            "0x6e82a56ced487ec9ef2442c8734465704d184719b499f03213138993535fb8d7"
        );
        assert_eq!(
            hex::encode(testnet.genesis_block().genesis_tx().hash().0),
            "aa524374ed489bf31abca33d38658c4366dd7a3e16f7ce187fb585f5dd3e6a8a"
        );
        assert_eq!(AsRef::<str>::as_ref(&testnet.chain_id()), "0.3.1");
        assert_eq!(
            OffsetDateTime::from(testnet.genesis_time()).unix_timestamp(),
            1_791_556_200
        );
    }

    #[test]
    fn the_testnet_genesis_era_has_the_digests_of_release_0_3() {
        let testnet = DeploymentSettings::testnet();
        let genesis_era = testnet.era_schedule().genesis().parameters;
        assert_eq!(
            genesis_era.digest.to_string(),
            "0xbb7a9de2dee794d771c95b48da7e9078e3ee24340d8dd1118ecb5382edba5022"
        );
        assert_eq!(
            genesis_era.fork_digest.to_string(),
            "0xed7f4300b476aa6d704edd05d53394ad53c1c27553643883ba29478b52471ab6"
        );
        // 300,000 one-second slots.
        assert_eq!(genesis_era.epoch_length().get(), 300_000);
    }

    #[test]
    fn the_testnet_moves_to_the_latest_ruleset_at_epoch_14() {
        let testnet = DeploymentSettings::testnet();
        let schedule = testnet.era_schedule();
        assert_eq!(schedule.iter().count(), 2);
        let era_1 = schedule.get(Era::new(1)).unwrap();

        assert_eq!(era_1.first_epoch, Epoch::new(14));
        assert_eq!(era_1.first_slot, Slot::new(4_200_000));
        // 2026-11-27T05:10:00Z.
        assert_eq!(era_1.start_time.unix_timestamp(), 1_795_756_200);
        assert_eq!(testnet.first_master_slot(), Slot::new(4_200_000));

        let definition = era_1.parameters;
        assert!(!definition.ruleset.is_legacy());
        assert!(!definition.accepted_fork_digests.accepts(None));
        // k = 120 at f = 1/30: 36,000 one-second slots.
        assert_eq!(definition.epoch_length().get(), 36_000);
        let cryptarchia = &definition.ruleset.parameters().cryptarchia;
        assert_eq!(cryptarchia.pow_reward_pool.0, Some(5_940_000_000_000_000));
        assert_eq!(cryptarchia.pow_config.reward.rate_num, 1);
        assert_eq!(cryptarchia.pow_config.reward.rate_den.get(), 100);
        // The genesis era keeps the pool its genesis block gives it.
        let genesis = schedule.genesis().parameters.ruleset.parameters();
        assert!(genesis.cryptarchia.pow_reward_pool.is_unset());
    }

    #[test]
    fn a_legacy_chain_keeps_the_discovery_names_of_release_0_3() {
        // What release 0.3.6 prints for its Kademlia and identify protocols:
        // named after the chain ID its genesis block inscribes.
        let testnet = DeploymentSettings::testnet();
        assert_eq!(
            testnet.chain_scope().to_string_with_name("kad"),
            "/logos-blockchain/0.3.1/kad"
        );
        assert_eq!(
            testnet.chain_scope().to_string_with_name("identify"),
            "/logos-blockchain/0.3.1/identify"
        );
        // Any other chain names them after its genesis block ID.
        let default = DeploymentSettings::default();
        assert_eq!(
            default.chain_scope().to_string_with_name("kad"),
            ProtocolScope::Chain(default.genesis_id()).to_string_with_name("kad")
        );
        assert_ne!(
            testnet.chain_scope().to_string_with_name("kad"),
            ProtocolScope::Chain(testnet.genesis_id()).to_string_with_name("kad")
        );
    }

    #[test]
    fn the_testnet_genesis_gives_the_parameters_its_inscription_leaves_out() {
        let testnet = DeploymentSettings::testnet();
        let genesis_parameters = testnet.genesis_block().genesis_tx().genesis_parameters();
        assert_eq!(genesis_parameters.pow_reward_pool, 12_600_000_000_000);
        let excluded: Vec<String> = genesis_parameters
            .consensus_excluded_keys
            .iter()
            .map(|key| hex::encode(lb_groth16::fr_to_bytes(key.as_fr())))
            .collect();
        assert_eq!(
            excluded,
            ["c2a6a4a0981d5bdcf8ddeb8d7934fd8c5510efeb1053f613b45871670b6f7b19"]
        );
    }

    #[test]
    fn the_testnet_survives_a_round_trip_through_yaml() {
        let testnet = DeploymentSettings::testnet();
        let recovered: DeploymentSettings =
            serde_yaml::from_str(&serde_yaml::to_string(&testnet).unwrap()).unwrap();
        assert_eq!(recovered.genesis_id(), testnet.genesis_id());
        assert_eq!(
            recovered.era_schedule().genesis().parameters.fork_digest,
            testnet.era_schedule().genesis().parameters.fork_digest
        );
        let era_1 = |settings: &DeploymentSettings| {
            settings
                .era_schedule()
                .get(Era::new(1))
                .unwrap()
                .parameters
                .fork_digest
        };
        assert_eq!(era_1(&recovered), era_1(&testnet));
    }

    #[test]
    fn a_legacy_era_accepts_only_transactions_without_a_fork_digest() {
        let settings = testnet_then_latest_ruleset(20);
        let genesis_era = settings.era_schedule().genesis().parameters;
        let era_1 = settings.era_schedule().get(Era::new(1)).unwrap().parameters;

        assert_eq!(
            genesis_era.accepted_fork_digests,
            AcceptedForkDigests::NoneCarried
        );
        assert!(genesis_era.accepted_fork_digests.accepts(None));
        // The era after it refuses the transactions of the legacy era, and
        // accepts neither its fork digest nor a transaction without one.
        assert_eq!(
            era_1.accepted_fork_digests,
            AcceptedForkDigests::new(vec![era_1.fork_digest])
        );
        assert!(!era_1.accepted_fork_digests.accepts(None));
    }

    #[test]
    fn an_era_after_a_legacy_one_leaves_the_legacy_fork_digest_unchanged() {
        let settings = testnet_then_latest_ruleset(20);
        let schedule = settings.era_schedule();
        let (genesis, era_1) = (schedule.genesis(), schedule.get(Era::new(1)).unwrap());

        assert_eq!(
            genesis.parameters.fork_digest,
            DeploymentSettings::testnet()
                .era_schedule()
                .genesis()
                .parameters
                .fork_digest
        );
        // Its own fork digest no longer hashes the chain ID.
        assert_eq!(
            era_1.parameters.fork_digest,
            ForkDigest::compute(
                settings.genesis_id(),
                [genesis.parameters.digest, era_1.parameters.digest].into_iter()
            )
        );
        assert_eq!(era_1.first_slot, Slot::new(20 * 300_000));
    }

    #[test]
    fn a_legacy_era_after_another_era_is_refused() {
        let EraDefinitions {
            eras,
            genesis_block,
        } = DeploymentSettings::testnet().into();
        let legacy = eras.genesis_era().clone();
        let eras = EraDeclarations::try_from(BTreeMap::from([
            (Epoch::new(0), legacy.clone()),
            (Epoch::new(20), legacy),
        ]))
        .unwrap();
        let reason = DeploymentSettings::new(&eras, genesis_block)
            .unwrap_err()
            .to_string();
        assert!(
            reason.contains("An era of ruleset 1 cannot follow another era"),
            "{reason}"
        );
    }

    #[test]
    fn a_genesis_block_of_another_ruleset_than_the_genesis_era_is_refused() {
        let EraDefinitions { eras: legacy, .. } = DeploymentSettings::testnet().into();
        let EraDefinitions { genesis_block, .. } = DeploymentSettings::default().into();
        let reason = DeploymentSettings::new(&legacy, genesis_block)
            .unwrap_err()
            .to_string();
        assert!(reason.contains("carries a fork digest"), "{reason}");

        let EraDefinitions { genesis_block, .. } = DeploymentSettings::testnet().into();
        let reason = deployment_from(genesis_block, [(0, ruleset(|_| ()))])
            .unwrap_err()
            .to_string();
        assert!(reason.contains("carries no fork digest"), "{reason}");
    }

    #[test]
    fn the_fork_digest_commits_to_the_era_parameters() {
        let settings = DeploymentSettings::default();
        let mut changed = settings.clone();
        changed
            .update_genesis_era_parameters(|parameters| {
                let reward = &mut parameters.cryptarchia.pow_config.reward;
                reward.slot_window = reward.slot_window.checked_add(1).unwrap();
            })
            .unwrap();
        let genesis_fork_digest = |settings: &DeploymentSettings| {
            settings.era_schedule().genesis().parameters.fork_digest
        };
        assert_ne!(
            genesis_fork_digest(&changed),
            genesis_fork_digest(&settings)
        );
    }
}
