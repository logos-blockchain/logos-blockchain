use std::{fmt::Debug, num::NonZeroU64};

use async_trait::async_trait;
use lb_core::header::HeaderId;
use lb_ledger::{IntentStatus, LedgerState};
use overwatch::DynError;
use serde::{Deserialize, Serialize};

/// Tracks the status of a submitted intent on the ledger.
///
/// See [`IntentTracker::handle_tip`] for details.
pub struct IntentTracker<Intent, Provider> {
    intent: Intent,
    config: Config,
    /// The last tip seen.
    last_tip: Option<HeaderId>,
    /// Tip changes since the submission or the last status check.
    tip_changes: u64,
    /// Number of status checks so far.
    status_checks: u64,
    /// API to fetch the ledger state.
    ledger_state_provider: Provider,
}

/// Configs for the intent tracker.
///
/// See [`IntentTracker::handle_tip`] for details.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    /// Interval between status checks of a submitted activity, in tip changes.
    pub status_check_interval_in_tip_changes: NonZeroU64,
    /// Max number of status checks for a submitted activity.
    pub max_status_checks: NonZeroU64,
}

impl<Intent, Provider> IntentTracker<Intent, Provider> {
    pub const fn new(
        intent: Intent,
        config: Config,
        tip: Option<HeaderId>,
        ledger_state_provider: Provider,
    ) -> Self {
        Self {
            intent,
            config,
            last_tip: tip,
            tip_changes: 0,
            status_checks: 0,
            ledger_state_provider,
        }
    }
}

impl<Intent, Provider> IntentTracker<Intent, Provider>
where
    Intent: lb_ledger::Intent<Error: Send + Sync + 'static> + Clone,
    Provider: LedgerStateProvider,
{
    /// Feeds a new tip to the tracker. A tip the tracker has already seen is
    /// ignored.
    ///
    /// Every [`Config::status_check_interval_in_tip_changes`] tip changes,
    /// the tracker checks the status of the intent against the tip ledger.
    ///
    /// If it is not yet time to check the intent status, this function returns
    /// [`Outcome::WaitingforMoreTipChanges`].
    /// If the status is checked successfully, this function returns
    /// [`Outcome::StatusChecked`], so that the caller can handle it
    /// accordingly.
    /// If the status check is failed, this function returns
    /// [`Outcome::StatusCheckFailed`].
    /// In all of the above cases, the caller can keep calling ths function
    /// to continue the tracking.
    ///
    /// After [`Config::max_status_checks`] checks, this function always returns
    /// [`Outcome::Exhaust`] without any status check. The caller can stop
    /// calling this function in this case.
    pub async fn handle_tip(&mut self, tip: HeaderId) -> Result<Outcome<Intent>, Error> {
        if self.status_checks >= self.config.max_status_checks.get() {
            return Ok(Outcome::Exhausted);
        }

        if self.last_tip.replace(tip) == Some(tip) {
            return Ok(Outcome::WaitingforMoreTipChanges); // tip unchanged
        }

        self.tip_changes = self.tip_changes.saturating_add(1);
        if self.tip_changes < self.config.status_check_interval_in_tip_changes.get() {
            return Ok(Outcome::WaitingforMoreTipChanges); // not time to check the status yet
        }

        self.tip_changes = 0;
        self.status_checks = self.status_checks.saturating_add(1);

        let ledger = match self.ledger_state_provider.get(tip).await {
            Ok(Some(ledger)) => ledger,
            Ok(None) => return Err(Error::LedgerStateNotFound(tip)),
            Err(e) => {
                return Err(Error::LedgerStateProvider(Box::new(e)));
            }
        };

        match self.intent.status(&ledger) {
            Ok(status) => Ok(Outcome::StatusChecked {
                intent: self.intent.clone(),
                status,
            }),
            Err(e) => Err(Error::StatusCheckFailed(Box::new(e))),
        }
    }
}

pub enum Outcome<Intent> {
    StatusChecked {
        intent: Intent,
        status: IntentStatus,
    },
    WaitingforMoreTipChanges,
    Exhausted,
}

#[async_trait]
pub trait LedgerStateProvider {
    type Error: std::error::Error + Send + Sync + 'static;

    async fn get(&self, block: HeaderId) -> Result<Option<LedgerState>, Self::Error>;
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ledger state not found for header {0}")]
    LedgerStateNotFound(HeaderId),
    #[error("ledger state provider error: {0}")]
    LedgerStateProvider(DynError),
    #[error("intent status check failed: {0}")]
    StatusCheckFailed(DynError),
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        convert::Infallible,
        num::NonZero,
        sync::{Arc, Mutex},
    };

    use lb_core::sdp::{MinStake, ServiceParameters, ServiceType};
    use lb_ledger::{
        Intent,
        config::{BlendPoWConfig, ModulusShift, PoWConfig, RewardPoWConfig},
        mantle::sdp::{ServiceRewardsParameters, rewards},
    };
    use lb_utils::math::{NonNegativeRatio, PositiveF64};

    use super::*;

    #[tokio::test]
    async fn unchanged_tip_never_trigger_status_check() {
        let mut tracker = tracker(IntentStatus::NotApplied, 2, 3);
        let tip = tracker.last_tip.unwrap();

        let out = tracker.handle_tip(tip).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        assert_eq!(tracker.last_tip, Some(tip));
        assert_eq!(tracker.tip_changes, 0);
        assert_eq!(tracker.status_checks, 0);
    }

    #[tokio::test]
    async fn status_check_is_triggered_after_interval() {
        let mut tracker = tracker(IntentStatus::Applied, 2, 3);

        let out = tracker.handle_tip(tip(1)).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        assert_eq!(tracker.status_checks, 0);
        assert_eq!(tracker.tip_changes, 1);

        let out = tracker.handle_tip(tip(2)).await.unwrap();
        expect_applied(&out);
        assert_eq!(tracker.status_checks, 1);
        assert_eq!(tracker.tip_changes, 0); // was reset

        let out = tracker.handle_tip(tip(3)).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        assert_eq!(tracker.status_checks, 1);
        assert_eq!(tracker.tip_changes, 1);

        let out = tracker.handle_tip(tip(4)).await.unwrap();
        expect_applied(&out);
        assert_eq!(tracker.status_checks, 2);
        assert_eq!(tracker.tip_changes, 0); // was reset

        let out = tracker.handle_tip(tip(5)).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        assert_eq!(tracker.status_checks, 2);
        assert_eq!(tracker.tip_changes, 1);

        let out = tracker.handle_tip(tip(6)).await.unwrap();
        expect_applied(&out);
        assert_eq!(tracker.status_checks, 3);
        assert_eq!(tracker.tip_changes, 0); // was reset

        let out = tracker.handle_tip(tip(7)).await.unwrap();
        assert!(matches!(out, Outcome::Exhausted));
    }

    #[tokio::test]
    async fn resubmit_if_intent_not_applied() {
        let mut tracker = tracker(IntentStatus::NotApplied, 2, 3);

        let out = tracker.handle_tip(tip(1)).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));

        let out = tracker.handle_tip(tip(2)).await.unwrap();
        expect_not_applied(&out);

        let out = tracker.handle_tip(tip(3)).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));

        let out = tracker.handle_tip(tip(4)).await.unwrap();
        expect_not_applied(&out);

        let out = tracker.handle_tip(tip(5)).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));

        let out = tracker.handle_tip(tip(6)).await.unwrap();
        expect_not_applied(&out);

        let out = tracker.handle_tip(tip(7)).await.unwrap();
        assert!(matches!(out, Outcome::Exhausted));
    }

    #[tokio::test]
    async fn resubmit_after_applied_intent_reverted() {
        let status = Arc::new(Mutex::new(Some(IntentStatus::Applied)));
        let mut tracker = tracker_with(MockIntent(Arc::clone(&status)), 2, 3);

        let out = tracker.handle_tip(tip(1)).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        let out = tracker.handle_tip(tip(2)).await.unwrap();
        expect_applied(&out);

        // e.g., a reorg reverted the applied intent.
        *status.lock().unwrap() = Some(IntentStatus::NotApplied);
        let out = tracker.handle_tip(tip(1)).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        let out = tracker.handle_tip(tip(2)).await.unwrap();
        expect_not_applied(&out);
    }

    #[tokio::test]
    async fn keep_tracking_if_status_check_failed() {
        let status = Arc::new(Mutex::new(None));
        let mut tracker = tracker_with(MockIntent(Arc::clone(&status)), 2, 3);

        let out = tracker.handle_tip(tip(1)).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        tracker.handle_tip(tip(2)).await.err().unwrap();

        let out = tracker.handle_tip(tip(3)).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        tracker.handle_tip(tip(4)).await.err().unwrap();

        let out = tracker.handle_tip(tip(5)).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        tracker.handle_tip(tip(6)).await.err().unwrap();

        let out = tracker.handle_tip(tip(7)).await.unwrap();
        assert!(matches!(out, Outcome::Exhausted));
    }

    #[tokio::test]
    async fn resubmit_after_status_check_becomes_available_again() {
        let status = Arc::new(Mutex::new(None));
        let mut tracker = tracker_with(MockIntent(Arc::clone(&status)), 2, 3);

        let out = tracker.handle_tip(tip(1)).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        tracker.handle_tip(tip(2)).await.err().unwrap();

        // e.g., the intent became applicable after a chain reorg.
        *status.lock().unwrap() = Some(IntentStatus::NotApplied);
        let out = tracker.handle_tip(tip(3)).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        let out = tracker.handle_tip(tip(4)).await.unwrap();
        expect_not_applied(&out);
    }

    #[tokio::test]
    async fn status_check_counts_error() {
        let mut tracker = tracker(IntentStatus::Applied, 2, 3);

        let out = tracker.handle_tip(tip(1)).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        assert_eq!(tracker.status_checks, 0);

        let unknown_tip = tip(99);
        let Err(Error::LedgerStateNotFound(_)) = tracker.handle_tip(unknown_tip).await else {
            panic!("expected error");
        };
        assert_eq!(tracker.status_checks, 1);
        assert_eq!(tracker.tip_changes, 0); // was reset
    }

    type MockTracker = IntentTracker<MockIntent, MockLedgerStateProvider>;

    fn tracker(status: IntentStatus, interval: u64, max: u64) -> MockTracker {
        tracker_with(
            MockIntent(Arc::new(Mutex::new(Some(status)))),
            interval,
            max,
        )
    }

    fn tracker_with(intent: MockIntent, interval: u64, max: u64) -> MockTracker {
        IntentTracker::new(
            intent,
            config(interval, max),
            Some(tip(0)),
            MockLedgerStateProvider::at((1..).take((interval * max).try_into().unwrap())),
        )
    }

    fn expect_applied(out: &Outcome<MockIntent>) {
        assert!(matches!(
            out,
            Outcome::StatusChecked {
                status: IntentStatus::Applied,
                ..
            }
        ));
    }

    fn expect_not_applied(out: &Outcome<MockIntent>) {
        assert!(matches!(
            out,
            Outcome::StatusChecked {
                status: IntentStatus::NotApplied,
                ..
            }
        ));
    }

    fn config(interval: u64, max: u64) -> Config {
        Config {
            status_check_interval_in_tip_changes: interval.try_into().unwrap(),
            max_status_checks: max.try_into().unwrap(),
        }
    }

    fn tip(n: u8) -> HeaderId {
        [n; 32].into()
    }

    /// An intent whose status is set by the test, ignoring the ledger state.
    #[derive(Clone)]
    struct MockIntent(Arc<Mutex<Option<IntentStatus>>>);

    impl Intent for MockIntent {
        type Error = MockIntentStatusCheckFailed;

        fn status(&self, _: &LedgerState) -> Result<IntentStatus, Self::Error> {
            self.0
                .lock()
                .unwrap()
                .map_or_else(|| Err(MockIntentStatusCheckFailed), Ok)
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("no status")]
    struct MockIntentStatusCheckFailed;

    /// Provides a static ledger state only at the given tips.
    #[derive(Debug)]
    struct MockLedgerStateProvider(HashSet<HeaderId>);

    impl MockLedgerStateProvider {
        fn at(tips: impl IntoIterator<Item = u8>) -> Self {
            Self(tips.into_iter().map(tip).collect())
        }
    }

    #[async_trait]
    impl LedgerStateProvider for MockLedgerStateProvider {
        type Error = Infallible;

        async fn get(&self, block: HeaderId) -> Result<Option<LedgerState>, Self::Error> {
            if self.0.contains(&block) {
                Ok(Some(ledger_state()))
            } else {
                Ok(None)
            }
        }
    }

    fn disabled_reward_config() -> RewardPoWConfig {
        RewardPoWConfig {
            reward_pool_genesis: 1_000_000_000,
            epoch_reward_genesis: 1_000_000,
            initial_difficulty_seed: 1_000,
            ema_smoothing_factor: 9,
            ema_smoothing_precision: NonZeroU64::new(10).unwrap(),
            target_claims_per_block: 100,
            rate_num: 0,
            rate_den: NonZeroU64::MIN,
            target_claim_per_block: NonZeroU64::MIN,
            expected_blocks_per_epoch: NonZeroU64::MIN,
            slot_window: NonZeroU64::new(100).unwrap(),
        }
    }

    fn ledger_state() -> LedgerState {
        let consensus_config = lb_cryptarchia_engine::Config::new(
            NonZero::new(2).unwrap(),
            NonNegativeRatio::new(1, 10.try_into().unwrap()),
            1f64.try_into().unwrap(),
            NonZero::new(12).unwrap(),
        );
        let epoch_config = lb_cryptarchia_engine::EpochConfig {
            epoch_stake_distribution_stabilization: 1.try_into().unwrap(),
            epoch_period_nonce_buffer: 1.try_into().unwrap(),
            epoch_period_nonce_stabilization: 1.try_into().unwrap(),
        };
        let epoch_length = epoch_config.epoch_length(consensus_config.base_period_length());

        LedgerState::from_utxos(
            [],
            &lb_ledger::Config {
                epoch_config,
                consensus_config,
                sdp_config: lb_ledger::mantle::sdp::Config {
                    service_params: Arc::new(
                        [(
                            ServiceType::BlendNetwork,
                            ServiceParameters {
                                inactivity_period: 20.try_into().unwrap(),
                                epoch: 0.into(),
                            },
                        )]
                        .into(),
                    ),
                    service_rewards_params: ServiceRewardsParameters {
                        blend: rewards::blend::RewardsParameters {
                            rounds_per_epoch: epoch_length.try_into().unwrap(),
                            message_frequency_per_round: PositiveF64::try_from(1.0).unwrap(),
                            num_blend_layers: NonZeroU64::new(3).unwrap(),
                            minimum_network_size: NonZeroU64::new(1).unwrap(),
                            data_replication_factor: 0,
                            activity_threshold_sensitivity: 1,
                        },
                    },
                    min_stake: MinStake {
                        threshold: 1,
                        timestamp: 0,
                    },
                },
                faucet_pk: None,
                pow_config: PoWConfig {
                    reward: disabled_reward_config(),
                    blend: BlendPoWConfig {
                        base_difficulty: ModulusShift::new::<19>(),
                        damping_den_offset: 0,
                        damping_num: 1.try_into().unwrap(),
                        max_step: 1.try_into().unwrap(),
                        target_transactions_per_block: 1.try_into().unwrap(),
                    },
                },
            },
        )
    }
}
