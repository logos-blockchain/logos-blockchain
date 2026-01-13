use std::{
    collections::HashMap,
    hash::Hash,
    time::{Duration, Instant},
};

use nomos_utils::{
    bounded_duration::{MinimalBoundedDuration, NANO, SECOND},
    math::NonNegativeF64,
};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MempoolPublishTriggerConfig {
    /// A percentage of shares required for the transaction to be published
    /// after the a `share_duration`.
    pub publish_threshold: NonNegativeF64,
    /// Duration after which the transaction should be published if
    /// `publish_threshold` is reached, or marked as expired if not reached.
    #[serde_as(as = "MinimalBoundedDuration<1, NANO>")]
    pub share_duration: Duration,
    /// A period after which expired states are removed from memory.
    #[serde_as(as = "MinimalBoundedDuration<1, NANO>")]
    pub prune_duration: Duration,
    /// An interval for pruning expired states.
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub prune_interval: Duration,
}

#[derive(Copy, Clone, Debug)]
pub enum ShareEvent {
    Share,
    Transaction { assignations: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareState {
    Complete,
    Incomplete,
    Expired,
}

#[derive(Debug)]
struct ShareEntry {
    count: u16,
    created_at: Instant,
    assignations: u16,
    expired: bool,
}

pub struct MempoolPublishTrigger<Id> {
    config: MempoolPublishTriggerConfig,
    received: HashMap<Id, ShareEntry>,
}

impl<Id: Clone + Hash + Eq> MempoolPublishTrigger<Id> {
    #[must_use]
    pub fn new(config: MempoolPublishTriggerConfig) -> Self {
        Self {
            config,
            received: HashMap::new(),
        }
    }

    pub fn record(&mut self, blob_id: Id, event: ShareEvent) -> ShareState {
        let entry = self.received.entry(blob_id).or_insert_with(|| ShareEntry {
            count: 0,
            assignations: u16::MAX,
            created_at: Instant::now(),
            expired: false,
        });

        if entry.expired {
            return ShareState::Expired;
        }

        match event {
            ShareEvent::Share => {
                entry.count += 1;
            }
            ShareEvent::Transaction { assignations } => {
                entry.assignations = assignations;
            }
        }

        if entry.count >= entry.assignations {
            ShareState::Complete
        } else {
            ShareState::Incomplete
        }
    }

    #[must_use]
    pub fn prune(&mut self, now: Instant) -> Vec<Id> {
        let mut to_publish = Vec::new();

        self.received.retain(|blob_id, state| {
            let elapsed = now.duration_since(state.created_at);

            if elapsed >= self.config.prune_duration {
                return false;
            }

            if !state.expired && elapsed >= self.config.share_duration {
                state.expired = true;

                let count = state.count;
                let threshold = (f64::from(state.assignations)
                    * self.config.publish_threshold.get())
                .ceil() as u16;

                if count >= threshold {
                    to_publish.push(blob_id.clone());
                }
            }

            true
        });

        to_publish
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOB_ID: [u8; 32] = [1; 32];

    impl<Id: Hash + Eq> MempoolPublishTrigger<Id> {
        fn len(&self) -> usize {
            self.received.len()
        }

        fn get_count(&self, blob_id: &Id) -> Option<u16> {
            self.received.get(blob_id).map(|e| e.count)
        }
    }

    fn test_config() -> MempoolPublishTriggerConfig {
        MempoolPublishTriggerConfig {
            publish_threshold: NonNegativeF64::try_from(0.5).unwrap(),
            share_duration: Duration::from_secs(5),
            prune_duration: Duration::from_secs(10),
            prune_interval: Duration::from_secs(5),
        }
    }

    #[test]
    fn test_update_reaches_complete() {
        let mut trigger = MempoolPublishTrigger::new(test_config());

        assert_eq!(
            trigger.record(BLOB_ID, ShareEvent::Transaction { assignations: 2 }),
            ShareState::Incomplete
        );
        assert_eq!(trigger.get_count(&BLOB_ID), Some(0));

        assert_eq!(
            trigger.record(BLOB_ID, ShareEvent::Share),
            ShareState::Incomplete
        );
        assert_eq!(trigger.get_count(&BLOB_ID), Some(1));

        assert_eq!(
            trigger.record(BLOB_ID, ShareEvent::Share),
            ShareState::Complete
        );
        assert_eq!(trigger.get_count(&BLOB_ID), Some(2));

        // Additional updates should still report Complete
        assert_eq!(
            trigger.record(BLOB_ID, ShareEvent::Share),
            ShareState::Complete
        );
        assert_eq!(trigger.get_count(&BLOB_ID), Some(3));
    }

    #[test]
    fn test_prune_and_publish() {
        let mut trigger = MempoolPublishTrigger::new(test_config());
        let now = Instant::now();

        // 6 out of 10 shares received (60%).
        trigger.record(BLOB_ID, ShareEvent::Transaction { assignations: 10 });
        for _ in 0..6 {
            trigger.record(BLOB_ID, ShareEvent::Share);
        }

        let later = now.checked_add(Duration::from_secs(6)).unwrap();
        let to_publish = trigger.prune(later);

        assert_eq!(to_publish, vec![BLOB_ID]);
        assert_eq!(trigger.len(), 1);
    }

    #[test]
    fn test_prune_and_not_publish() {
        let mut trigger = MempoolPublishTrigger::new(test_config());
        let now = Instant::now();

        // 4 out of 10 shares received (40%), which is < 50% threshold.
        trigger.record(BLOB_ID, ShareEvent::Transaction { assignations: 10 });
        for _ in 0..4 {
            trigger.record(BLOB_ID, ShareEvent::Share);
        }

        let later = now.checked_add(test_config().share_duration).unwrap();
        let to_publish = trigger.prune(later);

        assert!(to_publish.is_empty());
        assert_eq!(trigger.len(), 1);
    }

    #[test]
    fn test_prune_removes_old_entry() {
        let mut trigger = MempoolPublishTrigger::new(test_config());
        let now = Instant::now();

        trigger.record(BLOB_ID, ShareEvent::Transaction { assignations: 10 });
        trigger.record(BLOB_ID, ShareEvent::Share);
        assert_eq!(trigger.len(), 1);

        let later = now.checked_add(Duration::from_secs(11)).unwrap();
        let to_publish = trigger.prune(later);

        assert!(to_publish.is_empty());
        assert_eq!(trigger.len(), 0);
    }

    #[test]
    fn test_update_on_expired_entry() {
        let now = Instant::now();
        let mut trigger = MempoolPublishTrigger::new(test_config());
        trigger.record(BLOB_ID, ShareEvent::Transaction { assignations: 10 });
        trigger.record(BLOB_ID, ShareEvent::Share);

        let later = now.checked_add(Duration::from_secs(6)).unwrap();
        drop(trigger.prune(later));

        assert_eq!(
            trigger.record(BLOB_ID, ShareEvent::Share),
            ShareState::Expired
        );
    }

    #[test]
    fn test_prune_publish_is_once() {
        let now = Instant::now();
        let mut trigger = MempoolPublishTrigger::new(test_config());
        trigger.record(BLOB_ID, ShareEvent::Transaction { assignations: 10 });
        for _ in 0..5 {
            trigger.record(BLOB_ID, ShareEvent::Share);
        }

        let later = now.checked_add(Duration::from_secs(6)).unwrap();
        assert_eq!(trigger.prune(later), vec![BLOB_ID]);
        assert!(trigger.prune(later).is_empty());
    }
}
