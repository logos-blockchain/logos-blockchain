use std::{fmt::Debug, num::NonZeroU64};

use async_trait::async_trait;
use lb_core::header::HeaderId;
use lb_ledger::{IntentStatus, LedgerState};
use overwatch::DynError;
use serde::{Deserialize, Serialize};
use tracing::warn;

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
            ledger_state_provider,
        }
    }
}

impl<Intent, Provider> IntentTracker<Intent, Provider>
where
    Intent: lb_ledger::Intent<Error: Send + Sync + 'static> + Sync + Clone,
    Provider: LedgerStateProvider + Sync,
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
    /// If the intent appears in the ledger of `lib`, this function returns
    /// [`Outcome::Finalized`]. The caller can stop calling this function in
    /// this case. If the status check against the LIB ledger fails, the intent
    /// is considered not finalized yet, and the tracking continues as above.
    pub async fn handle_tip(
        &mut self,
        tip: HeaderId,
        lib: HeaderId,
    ) -> Result<Outcome<Intent>, Error> {
        // Check the intent state in LIB ledger first.
        match self.check_status(lib).await {
            Ok(IntentStatus::Applied) => return Ok(Outcome::Finalized),
            Ok(IntentStatus::NotApplied) => {}
            Err(err) => {
                warn!(%err, "failed to check the intent status in the LIB ledger: continuing tracking");
            }
        }

        if self.last_tip.replace(tip) == Some(tip) {
            return Ok(Outcome::WaitingforMoreTipChanges); // tip unchanged
        }

        self.tip_changes = self.tip_changes.saturating_add(1);
        if self.tip_changes < self.config.status_check_interval_in_tip_changes.get() {
            return Ok(Outcome::WaitingforMoreTipChanges); // not time to check the status yet
        }
        self.tip_changes = 0;

        let status = self.check_status(tip).await?;
        Ok(Outcome::StatusChecked {
            intent: self.intent.clone(),
            status,
        })
    }

    async fn check_status(&self, block: HeaderId) -> Result<IntentStatus, Error> {
        let ledger = self.get_ledger_state(block).await?;
        self.intent
            .status(&ledger)
            .map_err(|e| Error::StatusCheckFailed(Box::new(e)))
    }

    async fn get_ledger_state(&self, block: HeaderId) -> Result<LedgerState, Error> {
        self.ledger_state_provider
            .get(block)
            .await
            .map_err(|e| Error::LedgerStateProvider(Box::new(e)))?
            .ok_or(Error::LedgerStateNotFound(block))
    }
}

/// Return type of [`IntentTracker::handle_tip`].
pub enum Outcome<Intent> {
    /// Intent status checked against the tip ledger.
    StatusChecked {
        intent: Intent,
        status: IntentStatus,
    },
    /// Intent status check is triggered every
    /// [`Config::status_check_interval_in_tip_changes`] tip changes.
    WaitingforMoreTipChanges,
    /// Intent finalized on the LIB ledger.
    Finalized,
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
        convert::Infallible,
        num::NonZero,
        sync::{Arc, Mutex},
    };

    use lb_core::{
        mantle::{Note, Utxo},
        sdp::{MinStake, ServiceParameters, ServiceType},
    };
    use lb_key_management_system_keys::keys::ZkPublicKey;
    use lb_ledger::{
        Intent,
        config::{BlendPoWConfig, ModulusShift, PoWConfig, RewardPoWConfig},
        mantle::sdp::{ServiceRewardsParameters, rewards},
    };
    use lb_utils::math::{NonNegativeRatio, PositiveF64};

    use super::*;

    #[tokio::test]
    async fn unchanged_tip_never_trigger_status_check() {
        let mut tracker = tracker(IntentStatus::NotApplied, IntentStatus::NotApplied, 2);
        let tip = tracker.last_tip.unwrap();

        let out = tracker.handle_tip(tip, LIB.into()).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        assert_eq!(tracker.last_tip, Some(tip));
        assert_eq!(tracker.tip_changes, 0);
    }

    #[tokio::test]
    async fn status_check_is_triggered_after_interval() {
        let mut tracker = tracker(IntentStatus::Applied, IntentStatus::NotApplied, 2);

        let out = tracker.handle_tip(tip(1), LIB.into()).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        assert_eq!(tracker.tip_changes, 1);

        let out = tracker.handle_tip(tip(2), LIB.into()).await.unwrap();
        expect_applied(&out);
        assert_eq!(tracker.tip_changes, 0); // was reset

        let out = tracker.handle_tip(tip(3), LIB.into()).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        assert_eq!(tracker.tip_changes, 1);

        let out = tracker.handle_tip(tip(4), LIB.into()).await.unwrap();
        expect_applied(&out);
        assert_eq!(tracker.tip_changes, 0); // was reset
    }

    #[tokio::test]
    async fn intent_not_applied() {
        let mut tracker = tracker(IntentStatus::NotApplied, IntentStatus::NotApplied, 2);

        let out = tracker.handle_tip(tip(1), LIB.into()).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));

        let out = tracker.handle_tip(tip(2), LIB.into()).await.unwrap();
        expect_not_applied(&out);

        let out = tracker.handle_tip(tip(3), LIB.into()).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));

        let out = tracker.handle_tip(tip(4), LIB.into()).await.unwrap();
        expect_not_applied(&out);
    }

    #[tokio::test]
    async fn applied_intent_reverted() {
        let status_in_tip = Arc::new(Mutex::new(Some(IntentStatus::Applied)));
        let mut tracker = tracker_with(
            MockIntent {
                status_in_tip: Arc::clone(&status_in_tip),
                status_in_lib: Arc::new(Mutex::new(Some(IntentStatus::NotApplied))),
            },
            2,
        );

        let out = tracker.handle_tip(tip(1), LIB.into()).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        let out = tracker.handle_tip(tip(2), LIB.into()).await.unwrap();
        expect_applied(&out);

        // e.g., a reorg reverted the applied intent.
        *status_in_tip.lock().unwrap() = Some(IntentStatus::NotApplied);
        let out = tracker.handle_tip(tip(1), LIB.into()).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        let out = tracker.handle_tip(tip(2), LIB.into()).await.unwrap();
        expect_not_applied(&out);
    }

    #[tokio::test]
    async fn status_check_failed() {
        let status_in_tip = Arc::new(Mutex::new(None));
        let mut tracker = tracker_with(
            MockIntent {
                status_in_tip: Arc::clone(&status_in_tip),
                status_in_lib: Arc::new(Mutex::new(Some(IntentStatus::NotApplied))),
            },
            2,
        );

        let out = tracker.handle_tip(tip(1), LIB.into()).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        tracker.handle_tip(tip(2), LIB.into()).await.err().unwrap();

        let out = tracker.handle_tip(tip(3), LIB.into()).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        tracker.handle_tip(tip(4), LIB.into()).await.err().unwrap();
    }

    #[tokio::test]
    async fn status_check_becomes_available_again() {
        let status_in_tip = Arc::new(Mutex::new(None));
        let mut tracker = tracker_with(
            MockIntent {
                status_in_tip: Arc::clone(&status_in_tip),
                status_in_lib: Arc::new(Mutex::new(Some(IntentStatus::NotApplied))),
            },
            2,
        );

        let out = tracker.handle_tip(tip(1), LIB.into()).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        tracker.handle_tip(tip(2), LIB.into()).await.err().unwrap();

        // e.g., the intent became applicable after a chain reorg.
        *status_in_tip.lock().unwrap() = Some(IntentStatus::NotApplied);
        let out = tracker.handle_tip(tip(3), LIB.into()).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        let out = tracker.handle_tip(tip(4), LIB.into()).await.unwrap();
        expect_not_applied(&out);
    }

    #[tokio::test]
    async fn intent_finalized() {
        let status_in_lib = Arc::new(Mutex::new(Some(IntentStatus::NotApplied)));
        let mut tracker = tracker_with(
            MockIntent {
                status_in_tip: Arc::new(Mutex::new(Some(IntentStatus::NotApplied))),
                status_in_lib: Arc::clone(&status_in_lib),
            },
            2,
        );

        let out = tracker.handle_tip(tip(1), LIB.into()).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        let out = tracker.handle_tip(tip(2), LIB.into()).await.unwrap();
        expect_not_applied(&out);

        // the intent appears in the LIB ledger state, finally.
        *status_in_lib.lock().unwrap() = Some(IntentStatus::Applied);
        let out = tracker.handle_tip(tip(3), LIB.into()).await.unwrap();
        assert!(matches!(out, Outcome::Finalized));
    }

    #[tokio::test]
    async fn lib_status_check_failure_does_not_stop_tracking() {
        let status_in_lib = Arc::new(Mutex::new(None));
        let mut tracker = tracker_with(
            MockIntent {
                status_in_tip: Arc::new(Mutex::new(Some(IntentStatus::NotApplied))),
                status_in_lib: Arc::clone(&status_in_lib),
            },
            2,
        );

        // The tip status check continues while the LIB status check fails.
        let out = tracker.handle_tip(tip(1), LIB.into()).await.unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        let out = tracker.handle_tip(tip(2), LIB.into()).await.unwrap();
        expect_not_applied(&out);

        // LIB status check becomes available again
        *status_in_lib.lock().unwrap() = Some(IntentStatus::Applied);
        let out = tracker.handle_tip(tip(3), LIB.into()).await.unwrap();
        assert!(matches!(out, Outcome::Finalized));
    }

    #[tokio::test]
    async fn lib_ledger_state_not_found_does_not_stop_tracking() {
        let mut tracker = tracker(IntentStatus::NotApplied, IntentStatus::Applied, 2);

        // The tip status check continues while the LIB ledger state is missing.
        let out = tracker
            .handle_tip(tip(1), UNKNOWN_BLOCK.into())
            .await
            .unwrap();
        assert!(matches!(out, Outcome::WaitingforMoreTipChanges));
        let out = tracker
            .handle_tip(tip(2), UNKNOWN_BLOCK.into())
            .await
            .unwrap();
        expect_not_applied(&out);

        // LIB ledger state becomes available again
        let out = tracker.handle_tip(tip(3), LIB.into()).await.unwrap();
        assert!(matches!(out, Outcome::Finalized));
    }

    type MockTracker = IntentTracker<MockIntent, MockLedgerStateProvider>;

    fn tracker(
        status_in_tip: IntentStatus,
        status_in_lib: IntentStatus,
        interval: u64,
    ) -> MockTracker {
        tracker_with(MockIntent::new(status_in_tip, status_in_lib), interval)
    }

    fn tracker_with(intent: MockIntent, interval: u64) -> MockTracker {
        IntentTracker::new(
            intent,
            config(interval),
            Some(LIB.into()),
            MockLedgerStateProvider,
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

    fn config(interval: u64) -> Config {
        Config {
            status_check_interval_in_tip_changes: interval.try_into().unwrap(),
        }
    }

    fn tip(n: u8) -> HeaderId {
        [n; 32].into()
    }

    /// An intent whose status is set by the test.
    ///
    /// [`MockIntent::status`] returns the status set by the test without
    /// checking the ledger state.
    #[derive(Clone)]
    struct MockIntent {
        status_in_tip: Arc<Mutex<Option<IntentStatus>>>,
        status_in_lib: Arc<Mutex<Option<IntentStatus>>>,
    }

    impl MockIntent {
        fn new(status_in_tip: IntentStatus, status_in_lib: IntentStatus) -> Self {
            Self {
                status_in_tip: Arc::new(Mutex::new(Some(status_in_tip))),
                status_in_lib: Arc::new(Mutex::new(Some(status_in_lib))),
            }
        }
    }

    impl Intent for MockIntent {
        type Error = MockIntentStatusCheckFailed;

        /// Returns the status set by the test.
        ///
        /// If the ledger state is for LIB, it returns [`Self::status_in_lib`].
        /// Otherwise, it returns [`Self::status_in_tip`].
        fn status(&self, ledger: &LedgerState) -> Result<IntentStatus, Self::Error> {
            if ledger == &lib_ledger_state() {
                self.status_in_lib
                    .lock()
                    .unwrap()
                    .map_or_else(|| Err(MockIntentStatusCheckFailed), Ok)
            } else {
                self.status_in_tip
                    .lock()
                    .unwrap()
                    .map_or_else(|| Err(MockIntentStatusCheckFailed), Ok)
            }
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("no status")]
    struct MockIntentStatusCheckFailed;

    /// Provides a static ledger state.
    #[derive(Debug)]
    struct MockLedgerStateProvider;

    const LIB: [u8; 32] = [0; 32];
    const UNKNOWN_BLOCK: [u8; 32] = [u8::MAX; 32];

    #[async_trait]
    impl LedgerStateProvider for MockLedgerStateProvider {
        type Error = Infallible;

        /// Returns the static LIB ledger state if `block` is the static `LIB`.
        /// Otherwise, returns the static tip ledger state.
        async fn get(&self, block: HeaderId) -> Result<Option<LedgerState>, Self::Error> {
            if block == LIB.into() {
                Ok(Some(lib_ledger_state()))
            } else if block == UNKNOWN_BLOCK.into() {
                Ok(None)
            } else {
                Ok(Some(tip_ledger_state()))
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

    fn lib_ledger_state() -> LedgerState {
        ledger_state([Utxo::new([0; 32], 0, Note::new(1, ZkPublicKey::zero()))])
    }

    fn tip_ledger_state() -> LedgerState {
        ledger_state([])
    }

    fn ledger_state(utxos: impl IntoIterator<Item = Utxo>) -> LedgerState {
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
            utxos,
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
