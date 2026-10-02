use core::{fmt, iter, time::Duration};
use std::collections::BTreeMap;

use lb_cryptarchia_engine::Epoch;
use lb_ledger::mantle::sdp::rewards::blend::RewardsParameters;
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{Error as _, MapAccess, Visitor},
};

use crate::config::{
    blend::deployment::Settings as BlendDeploymentSettings,
    cryptarchia::deployment::Settings as CryptarchiaDeploymentSettings,
    time::deployment::Settings as TimeDeploymentSettings,
};

const GENESIS_EPOCH: Epoch = Epoch::new(0);

/// The eras of a chain, each keyed by the epoch it starts at. An era's
/// parameters are in force from that epoch until the next era starts.
///
/// A schedule is (de)serialized as a map from first epochs to era parameters,
/// and built from a `BTreeMap`, which keeps first epochs unique and ordered.
/// The first era must start at genesis. Deserialization also requires the eras
/// to be listed by strictly increasing first epoch, which rules out a repeated
/// epoch too.
///
/// Only a single era is supported for now, so a schedule of more than one era
/// is rejected, and the schedule holds that one era directly.
#[derive(Serialize, Debug, Clone)]
#[serde(into = "BTreeMap<Epoch, EraParameters>")]
pub struct EraSchedule {
    // Right now we support a single era starting at genesis, so from the input map we only store
    // the genesis era parameters.
    genesis_era: EraParameters,
}

impl EraSchedule {
    /// A schedule made of a single era, starting at genesis.
    #[must_use]
    pub const fn new_genesis(parameters: EraParameters) -> Self {
        Self {
            genesis_era: parameters,
        }
    }

    /// The parameters of the era in force: the only era of the schedule, for
    /// now.
    #[must_use]
    pub const fn genesis_era_parameters(&self) -> &EraParameters {
        &self.genesis_era
    }

    pub const fn genesis_era_parameters_mut(&mut self) -> &mut EraParameters {
        &mut self.genesis_era
    }

    #[must_use]
    pub fn into_genesis_era_parameters(self) -> EraParameters {
        self.genesis_era
    }

    /// Every era of the schedule with the epoch it starts at, in activation
    /// order: only the genesis era, for now.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (Epoch, &EraParameters)> {
        iter::once((GENESIS_EPOCH, &self.genesis_era))
    }
}

impl From<EraSchedule> for BTreeMap<Epoch, EraParameters> {
    fn from(schedule: EraSchedule) -> Self {
        Self::from([(GENESIS_EPOCH, schedule.genesis_era)])
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
}

impl TryFrom<BTreeMap<Epoch, EraParameters>> for EraSchedule {
    type Error = EraScheduleError;

    /// Builds a schedule from eras keyed by their first epoch. The map keeps
    /// them unique and ordered, so only the first one needs checking: it must
    /// start at genesis.
    fn try_from(mut eras: BTreeMap<Epoch, EraParameters>) -> Result<Self, Self::Error> {
        let Some((first_epoch, genesis_era)) = eras.pop_first() else {
            return Err(EraScheduleError::Empty);
        };
        if first_epoch != GENESIS_EPOCH {
            return Err(EraScheduleError::FirstEraAfterGenesis(first_epoch));
        }
        // Check remaining entries in the map.
        if !eras.is_empty() {
            return Err(EraScheduleError::MultipleEras(eras.len() + 1));
        }
        Ok(Self { genesis_era })
    }
}

impl<'de> Deserialize<'de> for EraSchedule {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(EraScheduleVisitor)
    }
}

/// Reads an era schedule, refusing an era whose first epoch does not strictly
/// follow the previous era's, before decoding its parameters. Read straight
/// into a map, such eras would instead be sorted, or a repeated epoch would
/// keep only its last era.
struct EraScheduleVisitor;

impl<'de> Visitor<'de> for EraScheduleVisitor {
    type Value = EraSchedule;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "a map from first epochs, strictly increasing from epoch {}, to era parameters",
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
        EraSchedule::try_from(eras).map_err(A::Error::custom)
    }
}

/// The parameters an era defines: everything every node on the chain must
/// agree on while the era is in force.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EraParameters {
    pub blend: BlendDeploymentSettings,
    pub cryptarchia: CryptarchiaDeploymentSettings,
    pub time: TimeDeploymentSettings,
}

impl EraParameters {
    #[must_use]
    pub const fn blend_round_duration(&self) -> Duration {
        self.blend.round_duration(&self.time.slot_duration)
    }

    #[must_use]
    pub fn blend_reward_params(&self) -> RewardsParameters {
        self.blend.rewards_params(&self.cryptarchia, &self.time)
    }
}

#[cfg(test)]
mod tests {
    use core::fmt::Write as _;
    use std::collections::BTreeMap;

    use lb_cryptarchia_engine::Epoch;

    use super::{EraParameters, EraSchedule, EraScheduleError};
    use crate::config::DeploymentSettings;

    fn parameters() -> EraParameters {
        DeploymentSettings::default()
            .eras
            .into_genesis_era_parameters()
    }

    /// A YAML schedule with one era per first epoch, in the given order.
    fn yaml(first_epochs: &[u32]) -> String {
        let era = serde_yaml::to_string(&parameters()).unwrap();
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
        serde_yaml::from_str::<EraSchedule>(&yaml(first_epochs))
            .unwrap_err()
            .to_string()
    }

    fn first_epochs(schedule: &EraSchedule) -> Vec<Epoch> {
        BTreeMap::from(schedule.clone()).keys().copied().collect()
    }

    /// Constructs a schedule from a map with one era per first epoch.
    fn construct(first_epochs: &[u32]) -> Result<EraSchedule, EraScheduleError> {
        EraSchedule::try_from(
            first_epochs
                .iter()
                .map(|first_epoch| (Epoch::new(*first_epoch), parameters()))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    #[test]
    fn a_single_era_starting_at_genesis_is_accepted() {
        let schedule: EraSchedule = serde_yaml::from_str(&yaml(&[0])).unwrap();
        assert_eq!(first_epochs(&schedule), [Epoch::new(0)]);
        assert_eq!(first_epochs(&construct(&[0]).unwrap()), [Epoch::new(0)]);
    }

    #[test]
    fn an_empty_schedule_is_rejected() {
        let error = serde_yaml::from_str::<EraSchedule>("{}").unwrap_err();
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
    fn the_schedule_round_trips_as_a_map_keyed_by_first_epoch() {
        let schedule = EraSchedule::new_genesis(parameters());

        let yaml = serde_yaml::to_string(&schedule).unwrap();
        assert!(yaml.starts_with("0:\n"), "{yaml}");
        let decoded: EraSchedule = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(first_epochs(&decoded), [Epoch::new(0)]);

        let json = serde_json::to_string(&schedule).unwrap();
        assert!(json.starts_with(r#"{"0":"#), "{json}");
        let decoded: EraSchedule = serde_json::from_str(&json).unwrap();
        assert_eq!(first_epochs(&decoded), [Epoch::new(0)]);
    }
}
