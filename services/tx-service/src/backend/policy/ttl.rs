use std::{
    collections::{BTreeMap, HashMap},
    hash::Hash,
    time::Duration,
};

use indexmap::IndexMap;
use lb_core::mantle::traits::Hashable;
use lb_time::{Era, era::EraSchedule};

use crate::backend::{
    AdmissionTime, IncludedItem,
    policy::{BlockSlots, EvictionPolicy, PolicyClock},
};

/// Evicts transactions that stayed in the pool longer than the TTL of the era
/// in force, regardless of pool fill.
///
/// Holds no state: a transaction's age is read from the admission time the
/// pool keeps for it.
pub struct TtlPolicy {
    settings: EraSchedule<TtlSettings>,
}

impl TtlPolicy {
    #[must_use]
    pub const fn new(settings: EraSchedule<TtlSettings>) -> Self {
        Self { settings }
    }

    /// Keys in `pending_txs` whose age reached the TTL of the era in force.
    ///
    /// `pending_txs` is admission-ordered with non-decreasing timestamps, so
    /// expired keys are always at the front and the scan stops at the first
    /// non-expired entry.
    #[must_use]
    pub fn expired<Key>(
        &self,
        pending_txs: &IndexMap<Key, AdmissionTime>,
        clock: &PolicyClock,
    ) -> Vec<Key>
    where
        Key: Hash + Eq + Clone,
    {
        let settings = TtlSettings::of_era(&self.settings, clock.era);

        pending_txs
            .iter()
            .take_while(|(_, admitted_at)| settings.is_expired(**admitted_at, clock.now_millis))
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Keys in `retired_txs` to release: those whose age reached the TTL plus
    /// the retirement grace of the era in force, so a retired transaction
    /// stays resolvable for as long as a leader may still select it, wherever
    /// it was retired.
    #[must_use]
    pub fn due_for_release<Key>(
        &self,
        retired_txs: &BTreeMap<Key, AdmissionTime>,
        clock: &PolicyClock,
    ) -> Vec<Key>
    where
        Key: Clone,
    {
        let settings = TtlSettings::of_era(&self.settings, clock.era);

        retired_txs
            .iter()
            .filter(|(_, admitted_at)| settings.is_due_for_release(**admitted_at, clock.now_millis))
            .map(|(key, _)| key.clone())
            .collect()
    }
}

/// Settings for [`TtlPolicy`] in an era.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TtlSettings {
    /// How long a pending transaction may stay in the pool before it is
    /// retired.
    pub tx_ttl: Duration,
    /// How long a retired transaction no canonical block carries stays
    /// resolvable after its age reaches the TTL.
    pub retirement_grace: Duration,
}

impl TtlSettings {
    /// The settings of `era` in `schedule`.
    ///
    /// # Panics
    ///
    /// If `schedule` has no `era`: the era in force always comes from the
    /// schedule the settings were built from.
    #[must_use]
    pub fn of_era(schedule: &EraSchedule<Self>, era: Era) -> Self {
        *schedule
            .get(era)
            .expect("the era in force must be in the schedule")
            .parameters
    }

    /// Whether a transaction admitted at `admitted_at` reached the TTL at
    /// `now_millis`.
    #[must_use]
    pub fn is_expired(&self, admitted_at: AdmissionTime, now_millis: u64) -> bool {
        u128::from(now_millis.saturating_sub(admitted_at)) >= self.tx_ttl.as_millis()
    }

    /// Whether a retired transaction admitted at `admitted_at` reached the TTL
    /// plus the retirement grace at `now_millis`.
    #[must_use]
    pub fn is_due_for_release(&self, admitted_at: AdmissionTime, now_millis: u64) -> bool {
        u128::from(now_millis.saturating_sub(admitted_at))
            >= (self.tx_ttl + self.retirement_grace).as_millis()
    }
}

impl<Tx, BlockId> EvictionPolicy<Tx, BlockId> for TtlPolicy
where
    Tx: Hashable,
    Tx::Hash: Hash + Eq + Clone,
{
    // Admission times are the pool's state, so there is nothing to track.
    fn on_add(
        &mut self,
        _key: &Tx::Hash,
        _tx: &Tx,
        _clock: &PolicyClock,
        _block_slots: &BlockSlots<BlockId>,
    ) {
    }

    fn on_release(&mut self, _key: &Tx::Hash) {}

    fn select_retirements(
        &self,
        clock: &PolicyClock,
        pending_txs: &IndexMap<Tx::Hash, AdmissionTime>,
    ) -> Vec<Tx::Hash> {
        self.expired(pending_txs, clock)
    }

    fn select_releases(
        &self,
        clock: &PolicyClock,
        retired_txs: &BTreeMap<Tx::Hash, AdmissionTime>,
        _included_txs: &HashMap<Tx::Hash, IncludedItem>,
    ) -> Vec<Tx::Hash> {
        self.due_for_release(retired_txs, clock)
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZero;

    use lb_time::era::{EraEntriesAfterGenesis, EraTiming};
    use time::OffsetDateTime;

    use super::*;

    const MINUTE: u64 = 60_000;

    /// A transaction admitted in the genesis era expires and is released under
    /// the shorter TTL and grace of era 1 once era 1 is in force.
    #[test]
    fn expiry_and_release_follow_the_era_in_force() {
        let policy = TtlPolicy::new(schedule(&[
            TtlSettings {
                tx_ttl: Duration::from_hours(1),
                retirement_grace: Duration::from_mins(10),
            },
            TtlSettings {
                tx_ttl: Duration::from_mins(30),
                retirement_grace: Duration::from_mins(5),
            },
        ]));
        let pending = IndexMap::from([(1u8, 0)]);
        let retired = BTreeMap::from([(1u8, 0)]);

        // 31 minutes old: past era 1's TTL (30m) but not the genesis era's (1h).
        let genesis = clock(31 * MINUTE, Era::genesis());
        let era_1 = clock(31 * MINUTE, Era::new(1));
        assert_eq!(policy.expired(&pending, &genesis), Vec::<u8>::new());
        assert_eq!(policy.expired(&pending, &era_1), vec![1]);
        assert_eq!(policy.due_for_release(&retired, &era_1), Vec::<u8>::new());

        // 35 minutes old: past era 1's TTL and grace (30m + 5m) but not the
        // genesis era's (1h + 10m).
        let genesis = clock(35 * MINUTE, Era::genesis());
        let era_1 = clock(35 * MINUTE, Era::new(1));
        assert_eq!(policy.due_for_release(&retired, &genesis), Vec::<u8>::new());
        assert_eq!(policy.due_for_release(&retired, &era_1), vec![1]);
    }

    /// A transaction expires once its age reaches the TTL, and is due for
    /// release once it reaches the TTL plus the grace, not only past them.
    #[test]
    fn expiry_and_release_start_when_the_age_reaches_them() {
        let settings = TtlSettings {
            tx_ttl: Duration::from_mins(30),
            retirement_grace: Duration::from_mins(5),
        };

        assert!(!settings.is_expired(0, 30 * MINUTE - 1));
        assert!(settings.is_expired(0, 30 * MINUTE));
        assert!(!settings.is_due_for_release(0, 35 * MINUTE - 1));
        assert!(settings.is_due_for_release(0, 35 * MINUTE));
    }

    /// A schedule of 100-slot epochs whose era `i`, from epoch `i`, has the
    /// settings `eras[i]`.
    fn schedule(eras: &[TtlSettings]) -> EraSchedule<TtlSettings> {
        let timing = EraTiming {
            slot_duration: Duration::from_secs(1),
            epoch_length: NonZero::new(100).expect("non-zero"),
        };
        let after_genesis = EraEntriesAfterGenesis::try_from_iter((1..eras.len()).map(|era| {
            let first_epoch = u32::try_from(era).expect("few eras");
            (NonZero::new(first_epoch).expect("non-zero"), timing)
        }))
        .expect("few eras");
        EraSchedule::new(OffsetDateTime::UNIX_EPOCH, timing, after_genesis)
            .expect("a representable schedule")
            .map(|era| eras[usize::from(era.era.into_inner())])
    }

    const fn clock(now_millis: u64, era: Era) -> PolicyClock {
        PolicyClock {
            now_millis,
            era,
            slot: None,
        }
    }
}
