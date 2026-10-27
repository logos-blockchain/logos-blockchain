use core::fmt;
use std::collections::BTreeMap;

use ::serde::{
    Deserialize, Deserializer, Serialize,
    de::{Error as _, MapAccess, Visitor},
};
use lb_cryptarchia_engine::Epoch;

use crate::config::deployment::era::ruleset::{EraRuleset, EraRulesetTransitionError};

pub mod ruleset;

mod codec;

pub const GENESIS_EPOCH: Epoch = Epoch::genesis();

/// The eras of a chain, each keyed by the epoch it starts at. An era is in
/// force from that epoch until the next era starts.
///
/// A schedule is (de)serialized as a map from first epochs to era
/// rulesets, and built from a `BTreeMap`, which keeps first epochs unique
/// and ordered. The first era must start at genesis. Deserialization also
/// requires the eras to be listed by strictly increasing first epoch, which
/// rules out a repeated epoch too.
///
/// Only a single era is supported for now, so a schedule of more than one era
/// is rejected, and the schedule holds that one era directly.
#[derive(Serialize, Debug, Clone)]
#[serde(into = "BTreeMap<Epoch, EraRuleset>")]
pub struct EraDeclarations {
    // Right now we support a single era starting at genesis, so from the input map we only store
    // the genesis era.
    genesis_era_ruleset: EraRuleset,
}

impl EraDeclarations {
    /// A schedule made of a single era, starting at genesis.
    #[must_use]
    pub const fn new_genesis(ruleset: EraRuleset) -> Self {
        Self {
            genesis_era_ruleset: ruleset,
        }
    }

    /// The era in force: the only era of the schedule, for now.
    #[must_use]
    pub const fn genesis_era(&self) -> &EraRuleset {
        &self.genesis_era_ruleset
    }

    pub const fn genesis_era_mut(&mut self) -> &mut EraRuleset {
        &mut self.genesis_era_ruleset
    }

    #[must_use]
    pub fn into_genesis_era(self) -> EraRuleset {
        self.genesis_era_ruleset
    }
}

impl From<EraDeclarations> for BTreeMap<Epoch, EraRuleset> {
    fn from(schedule: EraDeclarations) -> Self {
        Self::from([(GENESIS_EPOCH, schedule.genesis_era_ruleset)])
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EraScheduleError {
    #[error("the era schedule must contain at least one era")]
    Empty,
    #[error(
        "the first era must start at epoch {}, not at epoch {}",
        GENESIS_EPOCH.into_inner(),
        .0.into_inner()
    )]
    FirstEraAfterGenesis(Epoch),
    #[error(
        "eras must be listed by strictly increasing first epoch, but epoch {} follows epoch {}",
        .next.into_inner(),
        .previous.into_inner()
    )]
    OutOfOrder { previous: Epoch, next: Epoch },
    #[error("schedules of more than one era are not supported yet, got {0} eras")]
    MultipleEras(usize),
    #[error(
        "the era starting at epoch {} cannot follow the era before it: {reason}",
        .first_epoch.into_inner()
    )]
    InvalidEraTransition {
        first_epoch: Epoch,
        reason: EraRulesetTransitionError,
    },
}

impl TryFrom<BTreeMap<Epoch, EraRuleset>> for EraDeclarations {
    type Error = EraScheduleError;

    /// Builds a schedule from eras keyed by their first epoch. The map keeps
    /// them unique and ordered, so only the first one needs checking: it must
    /// start at genesis, and run a listed combination of versions. Every
    /// transition between consecutive eras is also checked.
    fn try_from(mut era_declarations: BTreeMap<Epoch, EraRuleset>) -> Result<Self, Self::Error> {
        let Some((&first_epoch, _)) = era_declarations.first_key_value() else {
            return Err(EraScheduleError::Empty);
        };
        if first_epoch != GENESIS_EPOCH {
            return Err(EraScheduleError::FirstEraAfterGenesis(first_epoch));
        }

        // Check era transitions.
        for ((_, previous), (&first_epoch, next)) in
            era_declarations.iter().zip(era_declarations.iter().skip(1))
        {
            previous.validate_successor(next).map_err(|reason| {
                EraScheduleError::InvalidEraTransition {
                    first_epoch,
                    reason,
                }
            })?;
        }

        let (_, genesis_era) = era_declarations
            .pop_first()
            .expect("the schedule has a genesis era");
        // Check remaining entries in the map. We still don't accept more than a single
        // era definition, so we bail out if anything else than the genesis era
        // definition is provided, at the moment.
        if !era_declarations.is_empty() {
            return Err(EraScheduleError::MultipleEras(era_declarations.len() + 1));
        }
        Ok(Self::new_genesis(genesis_era))
    }
}

impl<'de> Deserialize<'de> for EraDeclarations {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(EraScheduleVisitor)
    }
}

/// Reads an era schedule, refusing an era whose first epoch does not strictly
/// follow the previous era's, before decoding its ruleset. Read straight
/// into a map, such eras would instead be sorted, or a repeated epoch would
/// keep only its last era.
struct EraScheduleVisitor;

impl<'de> Visitor<'de> for EraScheduleVisitor {
    type Value = EraDeclarations;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "a map from first epochs, strictly increasing from epoch {}, to era rulesets",
            GENESIS_EPOCH.into_inner()
        )
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut eras = BTreeMap::new();
        while let Some(next) = map.next_key::<Epoch>()? {
            if let Some((&previous, _)) = eras.last_key_value()
                && next <= previous
            {
                return Err(A::Error::custom(EraScheduleError::OutOfOrder {
                    previous,
                    next,
                }));
            }
            eras.insert(next, map.next_value()?);
        }
        EraDeclarations::try_from(eras).map_err(A::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use core::fmt::Write as _;
    use std::collections::BTreeMap;

    use lb_cryptarchia_engine::Epoch;
    use lb_utils::math::NonNegativeRatio;

    use super::{EraDeclarations, EraScheduleError};
    use crate::config::{
        DeploymentSettings,
        deployment::era::ruleset::{
            EraRuleset,
            v1::{UnvalidatedCryptarchiaDeploymentSettings, cryptarchia::Error},
        },
    };

    fn ruleset() -> EraRuleset {
        DeploymentSettings::default()
            .era_schedule()
            .genesis()
            .parameters
            .ruleset
            .clone()
    }

    /// A YAML schedule with one era per first epoch, in the given order.
    fn yaml(first_epochs: &[u32]) -> String {
        let era = serde_yaml::to_string(&ruleset()).unwrap();
        let mut yaml = String::new();
        for first_epoch in first_epochs {
            writeln!(yaml, "{first_epoch}:").unwrap();
            for line in era.lines() {
                writeln!(yaml, "  {line}").unwrap();
            }
        }
        yaml
    }

    fn rejection(first_epochs: &[u32]) -> String {
        serde_yaml::from_str::<EraDeclarations>(&yaml(first_epochs))
            .unwrap_err()
            .to_string()
    }

    fn first_epochs(schedule: &EraDeclarations) -> Vec<Epoch> {
        BTreeMap::from(schedule.clone()).keys().copied().collect()
    }

    /// Constructs a schedule from a map with one era per first epoch.
    fn construct(first_epochs: &[u32]) -> Result<EraDeclarations, EraScheduleError> {
        EraDeclarations::try_from(
            first_epochs
                .iter()
                .map(|first_epoch| (Epoch::new(*first_epoch), ruleset()))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    #[test]
    fn a_single_era_starting_at_genesis_is_accepted() {
        let schedule: EraDeclarations = serde_yaml::from_str(&yaml(&[0])).unwrap();
        assert_eq!(first_epochs(&schedule), [Epoch::new(0)]);
        assert_eq!(first_epochs(&construct(&[0]).unwrap()), [Epoch::new(0)]);
    }

    #[test]
    fn an_empty_schedule_is_rejected() {
        let error = serde_yaml::from_str::<EraDeclarations>("{}").unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&EraScheduleError::Empty.to_string())
        );
        assert_eq!(construct(&[]).unwrap_err(), EraScheduleError::Empty);
    }

    #[test]
    fn a_first_era_after_genesis_is_rejected() {
        let expected = EraScheduleError::FirstEraAfterGenesis(Epoch::new(3));
        assert!(rejection(&[3]).contains(&expected.to_string()));
        assert_eq!(construct(&[3]).unwrap_err(), expected);
    }

    #[test]
    fn a_repeated_first_epoch_is_rejected() {
        let expected = EraScheduleError::OutOfOrder {
            previous: Epoch::new(0),
            next: Epoch::new(0),
        };
        assert!(rejection(&[0, 0]).contains(&expected.to_string()));
    }

    #[test]
    fn eras_out_of_order_are_rejected() {
        let expected = EraScheduleError::OutOfOrder {
            previous: Epoch::new(10),
            next: Epoch::new(5),
        };
        assert!(rejection(&[0, 10, 5]).contains(&expected.to_string()));
    }

    #[test]
    fn more_than_one_era_is_rejected() {
        let expected = EraScheduleError::MultipleEras(2);
        assert!(rejection(&[0, 10]).contains(&expected.to_string()));
        assert_eq!(construct(&[0, 10]).unwrap_err(), expected);
    }

    #[test]
    fn invalid_uncle_ref_window_in_era_transition() {
        // `s_0 = 3*⌊10*20⌋ = 600`
        // `W_1/f_1 = 7*20 = 140` > s_0 / 5: NOT ALLOWED
        assert_eq!(
            construct_eras([(0, ruleset_with(10, 20, 6)), (10, ruleset_with(30, 20, 7))])
                .unwrap_err(),
            EraScheduleError::InvalidEraTransition {
                first_epoch: Epoch::new(10),
                reason: Error::UncleReferenceWindowBeyondPreviousSlotSecurityParam {
                    window: 140,
                    previous_slot_security_param: 600,
                }
                .into(),
            }
        );

        // `s_0 = 3*⌊10*20⌋ = 600`
        // `W_1/f_1 = 6*40 = 240` > s_0 / 5: NOT ALLOWED
        assert_eq!(
            construct_eras([(0, ruleset_with(10, 20, 6)), (10, ruleset_with(30, 40, 6))])
                .unwrap_err(),
            EraScheduleError::InvalidEraTransition {
                first_epoch: Epoch::new(10),
                reason: Error::UncleReferenceWindowBeyondPreviousSlotSecurityParam {
                    window: 240,
                    previous_slot_security_param: 600,
                }
                .into(),
            }
        );
    }

    /// The default ruleset with `k`, `f = 1/f_denominator` and `W` replaced.
    fn ruleset_with(security_param: u32, f_denominator: u32, window: u32) -> EraRuleset {
        let mut ruleset = ruleset();
        let EraRuleset::V1(parameters) = &mut ruleset;

        let mut unvalidated =
            UnvalidatedCryptarchiaDeploymentSettings::from(parameters.cryptarchia.clone());
        unvalidated.security_param = security_param.try_into().unwrap();
        unvalidated.slot_activation_coeff =
            NonNegativeRatio::new(1, f_denominator.try_into().unwrap());
        unvalidated.uncle_reference_window_in_block = window.try_into().unwrap();

        parameters.cryptarchia = unvalidated.try_into().unwrap();
        ruleset
    }

    fn construct_eras(
        eras: impl IntoIterator<Item = (u32, EraRuleset)>,
    ) -> Result<EraDeclarations, EraScheduleError> {
        EraDeclarations::try_from(
            eras.into_iter()
                .map(|(first_epoch, ruleset)| (Epoch::new(first_epoch), ruleset))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    #[test]
    fn the_schedule_round_trips_as_a_map_keyed_by_first_epoch() {
        let schedule = EraDeclarations::new_genesis(ruleset());

        let yaml = serde_yaml::to_string(&schedule).unwrap();
        assert!(yaml.starts_with("0: !V1\n"), "{yaml}");
        let decoded: EraDeclarations = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(first_epochs(&decoded), [Epoch::new(0)]);

        let json = serde_json::to_string(&schedule).unwrap();
        assert!(json.starts_with(r#"{"0":"#), "{json}");
        let decoded: EraDeclarations = serde_json::from_str(&json).unwrap();
        assert_eq!(first_epochs(&decoded), [Epoch::new(0)]);
    }

    #[test]
    fn an_era_is_its_ruleset_tagged_with_its_version() {
        let yaml = serde_yaml::to_string(&ruleset()).unwrap();
        assert!(yaml.starts_with("!V1\nblend:\n"), "{yaml}");
    }

    #[test]
    fn a_ruleset_without_a_known_version_is_rejected() {
        let yaml = serde_yaml::to_string(&ruleset()).unwrap();
        let untagged = yaml.replacen("!V1\n", "", 1);
        assert!(serde_yaml::from_str::<EraRuleset>(&untagged).is_err());
        let unknown = yaml.replacen("!V1\n", "!V0\n", 1);
        assert!(serde_yaml::from_str::<EraRuleset>(&unknown).is_err());
    }
}
