use core::fmt;
use std::collections::btree_map::BTreeMap;

use lb_chain_service::Epoch;
use serde::de::{Error as _, MapAccess, Visitor};

use crate::config::deployment::{EraSchedule, EraScheduleError, era::GENESIS_EPOCH};

/// Reads an era schedule, refusing an era whose first epoch does not strictly
/// follow the previous era's, before decoding its parameters. Read straight
/// into a map, such eras would instead be sorted, or a repeated epoch would
/// keep only its last era.
pub(super) struct EraScheduleVisitor;

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
