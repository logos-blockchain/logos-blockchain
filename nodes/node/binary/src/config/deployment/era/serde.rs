use core::{fmt, num::NonZero};
use std::collections::btree_map::BTreeMap;

use lb_chain_service::Epoch;
use lb_era_parameters::{EraChanges, EraParameters};
use serde::de::{Error as _, MapAccess, Visitor};

use crate::config::deployment::{EraSchedule, EraScheduleError, era::GENESIS_EPOCH};

/// Reads an era schedule: the genesis era, which declares every section of its
/// parameters, then what each era after it changes. Refuses an era whose first
/// epoch does not strictly follow the previous era's before decoding it. Read
/// straight into a map, such eras would instead be sorted, or a repeated epoch
/// would keep only its last era.
pub(super) struct EraScheduleVisitor;

impl<'de> Visitor<'de> for EraScheduleVisitor {
    type Value = EraSchedule;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "a map from first epochs, strictly increasing from epoch {}, to what each era declares",
            GENESIS_EPOCH.into_inner()
        )
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let Some(first_epoch) = map.next_key::<Epoch>()? else {
            return Err(A::Error::custom(EraScheduleError::Empty));
        };
        if first_epoch != GENESIS_EPOCH {
            return Err(A::Error::custom(EraScheduleError::FirstEraAfterGenesis(
                first_epoch,
            )));
        }
        let genesis: EraParameters = map.next_value()?;

        let mut after_genesis = BTreeMap::new();
        let mut previous = first_epoch;
        while let Some(next) = map.next_key::<Epoch>()? {
            if next <= previous {
                return Err(A::Error::custom(EraScheduleError::OutOfOrder {
                    previous,
                    next,
                }));
            }
            let first_epoch = NonZero::new(next.into_inner())
                .expect("an era after the genesis era starts after epoch 0");
            after_genesis.insert(first_epoch, map.next_value::<EraChanges>()?);
            previous = next;
        }
        EraSchedule::new(genesis, after_genesis).map_err(A::Error::custom)
    }
}
