use core::{
    fmt::{self, Debug, Formatter},
    marker::PhantomData,
    num::NonZeroU64,
    ops::Deref,
};

use async_trait::async_trait;
use futures::Stream;
use lb_blend::proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs;
use lb_chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use lb_core::{crypto::ZkHash, proofs::leader_proof::LeaderPublic};
use lb_cryptarchia_engine::{Epoch, Slot};
use lb_groth16::Fr;
use lb_ledger::EpochState;
use lb_time_service::SlotTick;
use overwatch::overwatch::OverwatchHandle;

/// Secret `PoL` info associated to an epoch, as returned by the `PoL` info
/// provider.
#[derive(Clone)]
pub struct PolEpochInfo {
    pub epoch: Epoch,
    pub poq_public_inputs: LeaderPublic,
    /// The `PoL` secret inputs that are found to be winning at least one slot
    /// in the current epoch.
    pub poq_private_inputs: ProofOfLeadershipQuotaInputs,
}

impl Debug for PolEpochInfo {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("PolEpochInfo")
            .field("epoch", &self.epoch)
            .field("poq_public_inputs", &self.poq_public_inputs)
            .field("poq_private_inputs", &"<redacted>")
            .finish()
    }
}

#[async_trait]
pub trait PolInfoProvider<RuntimeServiceId> {
    type Stream: Stream<Item = PolEpochInfo>;

    async fn subscribe(
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Option<Self::Stream>;
}

const LOG_TARGET: &str = "blend::service::epoch";

/// A trait that provides the needed functionalities for the epoch stream to
/// fetch the epoch state for a given slot.
#[async_trait]
pub trait ChainApi<RuntimeServiceId> {
    async fn get_epoch_state_for_slot(&self, slot: Slot) -> Option<EpochState>;
}

#[async_trait]
impl<Cryptarchia, RuntimeServiceId> ChainApi<RuntimeServiceId>
    for CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>
where
    Cryptarchia: CryptarchiaServiceData<Tx: Send + Sync>,
    RuntimeServiceId: Send + Sync,
{
    async fn get_epoch_state_for_slot(&self, slot: Slot) -> Option<EpochState> {
        match self.get_epoch_state(slot).await {
            Ok(Ok(val)) => Some(val),
            Ok(Err(e)) => {
                tracing::error!(target: LOG_TARGET, "Get epoch state error: {e:?}.");
                None
            }
            Err(e) => {
                tracing::error!(target: LOG_TARGET, "Get epoch state API error: {e:?}.");
                None
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Default))]
pub struct LeaderInputsMinusQuota {
    pub pol_ledger_aged: ZkHash,
    pub pol_epoch_nonce: ZkHash,
    pub lottery_0: Fr,
    pub lottery_1: Fr,
}

impl From<EpochState> for LeaderInputsMinusQuota {
    fn from(
        EpochState {
            nonce,
            utxos,
            lottery_0,
            lottery_1,
            ..
        }: EpochState,
    ) -> Self {
        Self {
            pol_epoch_nonce: nonce,
            pol_ledger_aged: utxos.root(),
            lottery_0,
            lottery_1,
        }
    }
}

/// Event related to a given epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochEvent {
    /// A new epoch is available, which is either ongoing (if the handler is
    /// started mid-epoch) or has just started.
    NewEpoch((LeaderInputsMinusQuota, Epoch)),
    /// The information about the previous epoch the handler was tracking can
    /// now be discarded since its transition period has elapsed.
    OldEpochTransitionPeriodExpired,
    /// A new epoch comes just as the previous epoch transition is over. This
    /// can happen in one of two cases:
    /// * The epoch transition period is set to 1s, which means that an epoch
    ///   can be discarded as soon as it is over, or
    /// * The epoch transition period has the same duration as an epoch, which
    ///   means that when a new epoch starts, the epoch before the one that just
    ///   elapsed can be discarded, with the rotate one that will be remembered
    ///   until a new epoch starts.
    ///
    /// Consumers of this event will need to consider that any calls to
    /// `terminate_epoch_transition` should precede any calls to
    /// `rotate_epoch`, where `terminate_epoch_transition` would invalidate
    /// the epoch that can now be discarded, while `rotate_epoch`
    /// would move from the previous epoch to the new one that is notified about
    /// in this event.
    NewEpochAndOldEpochTransitionExpired((LeaderInputsMinusQuota, Epoch)),
}

/// A slot tick whose values against the previous tick have been validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ValidatedSlotTick(SlotTick);

impl ValidatedSlotTick {
    fn transition_to(self, slot_tick: SlotTick) -> Result<Self, ()> {
        if self.epoch > slot_tick.epoch || self.slot >= slot_tick.slot {
            return Err(());
        }
        Ok(slot_tick.into())
    }
}

impl From<SlotTick> for ValidatedSlotTick {
    fn from(value: SlotTick) -> Self {
        Self(value)
    }
}

impl Deref for ValidatedSlotTick {
    type Target = SlotTick;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, PartialEq, Eq)]
/// Internal tracking state for epoch information.
enum EpochTrackingState {
    /// The handler (which was most likely just started) has received the first
    /// epoch information from the chain API.
    FirstEpoch { current_epoch: Epoch },
    /// The handler has witnessed an epoch change, and it keeps minimal info
    /// about the previous epoch, to notify consumer when the configured
    /// epoch transition period has elapsed.
    SecondOrLaterEpoch {
        /// The last slot of the previous epoch. `None` when the consumers have
        /// been notified of the previous epoch, and `Some` for the time between
        /// a new epoch and the old epoch transition period.
        previous_epoch_last_slot: Option<Slot>,
        /// The current epoch number.
        current_epoch: Epoch,
    },
}

impl EpochTrackingState {
    const fn new(ValidatedSlotTick(SlotTick { epoch, .. }): ValidatedSlotTick) -> Self {
        Self::FirstEpoch {
            current_epoch: epoch,
        }
    }

    /// Use the slot before the provided one to mark the currently tracked epoch
    /// as passed, and takes the new provided epoch as the new ongoing one.
    #[expect(clippy::unused_self, reason = "We want to consume the input.")]
    fn transition_to_new_epoch(
        self,
        ValidatedSlotTick(SlotTick { epoch, slot }): ValidatedSlotTick,
    ) -> Self {
        Self::SecondOrLaterEpoch {
            previous_epoch_last_slot: Some(slot.into_inner().saturating_sub(1).into()),
            current_epoch: epoch,
        }
    }

    /// Return the currently tracked epoch.
    const fn last_processed_epoch(&self) -> Epoch {
        match self {
            Self::FirstEpoch { current_epoch } | Self::SecondOrLaterEpoch { current_epoch, .. } => {
                *current_epoch
            }
        }
    }
}

/// A stream that listens to slot ticks, and on the first slot tick received as
/// well as the first slot tick of each new epoch, fetches the epoch state from
/// the provided chain service adapter.
///
/// Then, when the configured transition period elapses for the past epoch, it
/// notifies consumers about it, once for each epoch.
pub struct EpochHandler<ChainService, RuntimeServiceId> {
    /// A tracked of the last processed tick, to perform input tick sanitation,
    /// i.e., ensuring slots are always increasing and epochs are not
    /// decreasing.
    last_processed_tick: Option<ValidatedSlotTick>,
    /// Information about the current epoch, and whether the past epoch epoch
    /// transition has already elapsed.
    epoch_tracking_state: Option<EpochTrackingState>,
    /// The chain service API providing epoch state for a given slot.
    chain_service: ChainService,
    /// Configured epoch transition period in slots.
    epoch_transition_period_in_slots: NonZeroU64,
    _phantom: PhantomData<RuntimeServiceId>,
}

impl<ChainService, RuntimeServiceId> EpochHandler<ChainService, RuntimeServiceId> {
    pub const fn new(
        chain_service: ChainService,
        epoch_transition_period_in_slots: NonZeroU64,
    ) -> Self {
        Self {
            last_processed_tick: None,
            epoch_tracking_state: None,
            chain_service,
            epoch_transition_period_in_slots,
            _phantom: PhantomData,
        }
    }
}

impl<ChainService, RuntimeServiceId> EpochHandler<ChainService, RuntimeServiceId>
where
    ChainService: ChainApi<RuntimeServiceId> + Sync,
    RuntimeServiceId: Sync,
{
    pub async fn tick(&mut self, new_tick: SlotTick) -> Option<EpochEvent> {
        // We try to validate the new tick, else we restore the last valid one that we
        // had.
        let validated_slot_tick = if let Some(last_slot_tick) = self.last_processed_tick.take() {
            last_slot_tick.transition_to(new_tick).inspect_err(|()| {
                tracing::error!(target: LOG_TARGET, "Slot ticks are assumed to be always increasing, and epoch ticks are assumed to never be decreasing.");
                self.last_processed_tick = Some(last_slot_tick);
            }).ok()?
        } else {
            new_tick.into()
        };
        self.last_processed_tick = Some(validated_slot_tick);

        if let Some(last_processed_epoch) = self
            .epoch_tracking_state
            .as_ref()
            .map(EpochTrackingState::last_processed_epoch)
            && last_processed_epoch == new_tick.epoch
        {
            tracing::trace!(target: LOG_TARGET, "New slot for same epoch. Skipping...");
            // We know we're in the same epoch, so we only check if the previous epoch is
            // expired, and notify consumers about it.
            if self.check_and_consume_past_epoch_transition_period(validated_slot_tick) {
                return Some(EpochEvent::OldEpochTransitionPeriodExpired);
            }
            return None;
        }

        tracing::debug!(target: LOG_TARGET, "Found epoch unseen before. Retrieving its state...");
        let Some(epoch_state) = self
            .chain_service
            .get_epoch_state_for_slot(new_tick.slot)
            .await
        else {
            tracing::warn!(target: LOG_TARGET, "Failed to get epoch state for slot.  Skipping...");
            return None;
        };
        tracing::debug!(target: LOG_TARGET, "Retrieved epoch state for unseen epoch: {:?}.", epoch_state);

        // This is true if epochs are shorter than transition periods. It's not likely
        // to happen in production, but we must still account for this
        // possibility. In this case, we consider an old epoch expired if another one
        // after it was replaced with a new, current one.
        let should_notify_about_two_epochs_back =
            if let Some(EpochTrackingState::SecondOrLaterEpoch {
                previous_epoch_last_slot,
                ..
            }) = self.epoch_tracking_state
                && previous_epoch_last_slot.is_some()
            {
                true
            } else {
                false
            };

        // If we have already previously processed an epoch, keep track of its last
        // slot, so we can signal consumers when the transition period is over.
        if let Some(current_epoch_state) = self.epoch_tracking_state.take() {
            self.epoch_tracking_state =
                Some(current_epoch_state.transition_to_new_epoch(validated_slot_tick));
        } else {
            self.epoch_tracking_state = Some(EpochTrackingState::new(validated_slot_tick));
        }

        // We need to notify about epoch transitioncheck if the previous epoch is
        // expired only if two epochs in the past are not, since the two
        // conditions cannot exist at the same time.
        let epoch_event = if should_notify_about_two_epochs_back
            || self.check_and_consume_past_epoch_transition_period(validated_slot_tick)
        {
            EpochEvent::NewEpochAndOldEpochTransitionExpired((epoch_state.into(), new_tick.epoch))
        } else {
            EpochEvent::NewEpoch((epoch_state.into(), new_tick.epoch))
        };

        Some(epoch_event)
    }

    // If we have witnessed a new epoch being transitioned and we have passed its
    // transition period, notify the consumers.
    const fn check_and_consume_past_epoch_transition_period(
        &mut self,
        ValidatedSlotTick(SlotTick { slot, .. }): ValidatedSlotTick,
    ) -> bool {
        if let Some(EpochTrackingState::SecondOrLaterEpoch {
            previous_epoch_last_slot,
            ..
        }) = &mut self.epoch_tracking_state
            && let Some(some_previous_epoch_last_slot) = previous_epoch_last_slot
            && slot.into_inner() - some_previous_epoch_last_slot.into_inner()
                >= self.epoch_transition_period_in_slots.get()
        {
            *previous_epoch_last_slot = None;
            return true;
        }

        false
    }
}

#[cfg(test)]
mod tests {

    use lb_time_service::SlotTick;
    use test_log::test;

    use crate::{
        epoch_info::{EpochEvent, EpochHandler, EpochTrackingState, LeaderInputsMinusQuota},
        test_utils::epoch::{TestChainService, default_epoch_state},
    };

    type TestEpochHandler = EpochHandler<TestChainService, ()>;

    #[test(tokio::test)]
    async fn epoch_ticks() {
        let ticks = vec![
            SlotTick {
                epoch: 1.into(),
                slot: 1.into(),
            },
            // New slot same epoch
            SlotTick {
                epoch: 1.into(),
                slot: 2.into(),
            },
            // New slot new epoch
            SlotTick {
                epoch: 2.into(),
                slot: 3.into(),
            },
        ];
        let mut ticks_iter = ticks.into_iter();
        let mut stream = TestEpochHandler::new(TestChainService, u64::MAX.try_into().unwrap());

        // First poll of the stream will set the epoch info and return the retrieved
        // state.
        let next_tick = stream.tick(ticks_iter.next().unwrap()).await;
        assert_eq!(
            stream.last_processed_tick,
            Some(
                SlotTick {
                    epoch: 1.into(),
                    slot: 1.into()
                }
                .into()
            )
        );
        assert_eq!(
            next_tick,
            Some(EpochEvent::NewEpoch((
                LeaderInputsMinusQuota::from(default_epoch_state()),
                1.into()
            )))
        );
        assert_eq!(
            stream.epoch_tracking_state,
            Some(EpochTrackingState::FirstEpoch {
                current_epoch: 1.into()
            })
        );

        // Second poll of the stream will not return anything since it's in the same
        // epoch.
        let next_tick = stream.tick(ticks_iter.next().unwrap()).await;
        assert_eq!(
            stream.last_processed_tick,
            Some(
                SlotTick {
                    epoch: 1.into(),
                    slot: 2.into()
                }
                .into()
            )
        );
        assert!(next_tick.is_none());
        assert_eq!(
            stream.epoch_tracking_state,
            Some(EpochTrackingState::FirstEpoch {
                current_epoch: 1.into()
            })
        );

        // Third poll of the stream will yield a new element since we're in a new epoch.
        let next_tick = stream.tick(ticks_iter.next().unwrap()).await;
        assert_eq!(
            stream.last_processed_tick,
            Some(
                SlotTick {
                    epoch: 2.into(),
                    slot: 3.into()
                }
                .into()
            )
        );
        assert_eq!(
            next_tick,
            Some(EpochEvent::NewEpoch((
                LeaderInputsMinusQuota::from(default_epoch_state()),
                2.into()
            )))
        );
        assert_eq!(
            stream.epoch_tracking_state,
            Some(EpochTrackingState::SecondOrLaterEpoch {
                current_epoch: 2.into(),
                previous_epoch_last_slot: Some(2.into())
            })
        );
    }

    #[test(tokio::test)]
    async fn epoch_transition_1_slot() {
        let ticks = vec![
            // Ongoing epoch
            SlotTick {
                epoch: 1.into(),
                slot: 1.into(),
            },
            // New slot same epoch
            SlotTick {
                epoch: 1.into(),
                slot: 2.into(),
            },
            // New slot new epoch (will trigger epoch transition and new epoch event because of 1s
            // transition period)
            SlotTick {
                epoch: 2.into(),
                slot: 3.into(),
            },
            // New slot new epoch (will trigger epoch transition and new epoch event because of 1s
            // transition period)
            SlotTick {
                epoch: 3.into(),
                slot: 4.into(),
            },
        ];
        let mut ticks_iter = ticks.into_iter();
        let mut stream = TestEpochHandler::new(TestChainService, 1.try_into().unwrap());

        // First tick: we yield the retrieved data and mark the epoch as the first seen.
        let next_tick = stream.tick(ticks_iter.next().unwrap()).await;
        assert_eq!(
            next_tick,
            Some(EpochEvent::NewEpoch((
                LeaderInputsMinusQuota::from(default_epoch_state()),
                1.into()
            )))
        );
        assert_eq!(
            stream.epoch_tracking_state,
            Some(EpochTrackingState::FirstEpoch {
                current_epoch: 1.into()
            })
        );

        // Second tick: same epoch new slot, nothing interesting happens.
        let next_tick = stream.tick(ticks_iter.next().unwrap()).await;
        assert!(next_tick.is_none());
        assert_eq!(
            stream.epoch_tracking_state,
            Some(EpochTrackingState::FirstEpoch {
                current_epoch: 1.into()
            })
        );

        // Third tick: new epoch new slot, old epoch transition expired
        let next_tick = stream.tick(ticks_iter.next().unwrap()).await;
        assert_eq!(
            next_tick,
            Some(EpochEvent::NewEpochAndOldEpochTransitionExpired((
                LeaderInputsMinusQuota::from(default_epoch_state()),
                2.into()
            )))
        );
        assert_eq!(
            stream.epoch_tracking_state,
            Some(EpochTrackingState::SecondOrLaterEpoch {
                current_epoch: 2.into(),
                previous_epoch_last_slot: None
            })
        );

        // Fourth tick: new epoch same slot, old epoch transition expired
        let next_tick = stream.tick(ticks_iter.next().unwrap()).await;
        assert_eq!(
            next_tick,
            Some(EpochEvent::NewEpochAndOldEpochTransitionExpired((
                LeaderInputsMinusQuota::from(default_epoch_state()),
                3.into()
            )))
        );
        assert_eq!(
            stream.epoch_tracking_state,
            Some(EpochTrackingState::SecondOrLaterEpoch {
                current_epoch: 3.into(),
                // `None` since we are notifying in this tick about its expiration.
                previous_epoch_last_slot: None
            })
        );
    }

    #[test(tokio::test)]
    async fn epoch_transition_more_than_1_slot() {
        let ticks = vec![
            // Ongoing epoch
            SlotTick {
                epoch: 1.into(),
                slot: 1.into(),
            },
            // New slot same epoch
            SlotTick {
                epoch: 1.into(),
                slot: 2.into(),
            },
            // New slot new epoch (will trigger a new epoch event only)
            SlotTick {
                epoch: 2.into(),
                slot: 3.into(),
            },
            // New slot same epoch (will trigger an epoch transition only)
            SlotTick {
                epoch: 2.into(),
                slot: 4.into(),
            },
            // New slot new epoch (will trigger a new epoch event only)
            SlotTick {
                epoch: 3.into(),
                slot: 5.into(),
            },
            // New slot new epoch (will trigger a new epoch event and a epoch transition
            // together)
            SlotTick {
                epoch: 4.into(),
                slot: 6.into(),
            },
            // New slot same epoch ((will trigger an epoch transition only)
            SlotTick {
                epoch: 4.into(),
                slot: 7.into(),
            },
        ];
        let mut ticks_iter = ticks.into_iter();
        let mut stream = TestEpochHandler::new(TestChainService, 2.try_into().unwrap());

        // We yield the retrieved data and mark the epoch as the first seen.
        let next_tick = stream.tick(ticks_iter.next().unwrap()).await;
        assert_eq!(
            next_tick,
            Some(EpochEvent::NewEpoch((
                LeaderInputsMinusQuota::from(default_epoch_state()),
                1.into()
            )))
        );
        assert_eq!(
            stream.epoch_tracking_state,
            Some(EpochTrackingState::FirstEpoch {
                current_epoch: 1.into()
            })
        );

        // Second tick: same epoch new slot, nothing interesting happens.
        let next_tick = stream.tick(ticks_iter.next().unwrap()).await;
        assert!(next_tick.is_none());
        assert_eq!(
            stream.epoch_tracking_state,
            Some(EpochTrackingState::FirstEpoch {
                current_epoch: 1.into()
            })
        );

        // Third tick: new epoch new slot, new epoch event generated
        let next_tick = stream.tick(ticks_iter.next().unwrap()).await;
        assert_eq!(
            next_tick,
            Some(EpochEvent::NewEpoch((
                LeaderInputsMinusQuota::from(default_epoch_state()),
                2.into()
            )))
        );
        assert_eq!(
            stream.epoch_tracking_state,
            Some(EpochTrackingState::SecondOrLaterEpoch {
                current_epoch: 2.into(),
                previous_epoch_last_slot: Some(2.into())
            })
        );

        // Fourth tick: new slot same epoch, previous epoch now expires
        let next_tick = stream.tick(ticks_iter.next().unwrap()).await;
        assert_eq!(next_tick, Some(EpochEvent::OldEpochTransitionPeriodExpired));
        assert_eq!(
            stream.epoch_tracking_state,
            Some(EpochTrackingState::SecondOrLaterEpoch {
                current_epoch: 2.into(),
                // `None` since we are notifying in this tick about its expiration.
                previous_epoch_last_slot: None
            })
        );

        // Fifth tick: new slot new epoch, new epoch event generated
        let next_tick = stream.tick(ticks_iter.next().unwrap()).await;
        assert_eq!(
            next_tick,
            Some(EpochEvent::NewEpoch((
                LeaderInputsMinusQuota::from(default_epoch_state()),
                3.into()
            )))
        );
        assert_eq!(
            stream.epoch_tracking_state,
            Some(EpochTrackingState::SecondOrLaterEpoch {
                current_epoch: 3.into(),
                previous_epoch_last_slot: Some(4.into())
            })
        );

        // Sixth tick: new slot new epoch, new epoch and old epoch expiration are
        // generated
        let next_tick = stream.tick(ticks_iter.next().unwrap()).await;
        assert_eq!(
            next_tick,
            Some(EpochEvent::NewEpochAndOldEpochTransitionExpired((
                LeaderInputsMinusQuota::from(default_epoch_state()),
                4.into()
            )))
        );
        assert_eq!(
            stream.epoch_tracking_state,
            Some(EpochTrackingState::SecondOrLaterEpoch {
                current_epoch: 4.into(),
                // Not `None` because we notified consumers about the epoch before the one that was
                // just rotated, so this one will be notified next.
                previous_epoch_last_slot: Some(5.into())
            })
        );

        // Seventh tick: new slot same epoch, the past epoch can now be discarded.
        let next_tick = stream.tick(ticks_iter.next().unwrap()).await;
        assert_eq!(next_tick, Some(EpochEvent::OldEpochTransitionPeriodExpired));
        assert_eq!(
            stream.epoch_tracking_state,
            Some(EpochTrackingState::SecondOrLaterEpoch {
                current_epoch: 4.into(),
                previous_epoch_last_slot: None
            })
        );
    }

    #[test(tokio::test)]
    async fn slot_not_increasing() {
        let mut stream = TestEpochHandler::new(TestChainService, u64::MAX.try_into().unwrap());
        stream
            .tick(SlotTick {
                epoch: 2.into(),
                slot: 2.into(),
            })
            .await;
        assert_eq!(
            stream.last_processed_tick,
            Some(
                SlotTick {
                    epoch: 2.into(),
                    slot: 2.into()
                }
                .into()
            )
        );
        assert!(
            stream
                .tick(SlotTick {
                    epoch: 3.into(),
                    slot: 2.into(),
                })
                .await
                .is_none()
        );
        assert_eq!(
            stream.last_processed_tick,
            Some(
                SlotTick {
                    epoch: 2.into(),
                    slot: 2.into()
                }
                .into()
            )
        );
        assert_eq!(
            stream.last_processed_tick,
            Some(
                SlotTick {
                    epoch: 2.into(),
                    slot: 2.into(),
                }
                .into()
            )
        );
        assert_eq!(
            stream.last_processed_tick,
            Some(
                SlotTick {
                    epoch: 2.into(),
                    slot: 2.into()
                }
                .into()
            )
        );
        assert!(
            stream
                .tick(SlotTick {
                    epoch: 3.into(),
                    slot: 1.into(),
                })
                .await
                .is_none()
        );
        assert_eq!(
            stream.last_processed_tick,
            Some(
                SlotTick {
                    epoch: 2.into(),
                    slot: 2.into()
                }
                .into()
            )
        );
        assert_eq!(
            stream.last_processed_tick,
            Some(
                SlotTick {
                    epoch: 2.into(),
                    slot: 2.into(),
                }
                .into()
            )
        );
        assert_eq!(
            stream.last_processed_tick,
            Some(
                SlotTick {
                    epoch: 2.into(),
                    slot: 2.into()
                }
                .into()
            )
        );
    }

    #[test(tokio::test)]
    async fn epoch_not_increasing() {
        let mut stream = TestEpochHandler::new(TestChainService, u64::MAX.try_into().unwrap());
        stream
            .tick(SlotTick {
                epoch: 2.into(),
                slot: 2.into(),
            })
            .await;
        assert_eq!(
            stream.last_processed_tick,
            Some(
                SlotTick {
                    epoch: 2.into(),
                    slot: 2.into()
                }
                .into()
            )
        );
        assert!(
            stream
                .tick(SlotTick {
                    epoch: 2.into(),
                    slot: 3.into(),
                })
                .await
                .is_none()
        );
        assert_eq!(
            stream.last_processed_tick,
            Some(
                SlotTick {
                    epoch: 2.into(),
                    slot: 3.into()
                }
                .into()
            )
        );
        assert!(
            stream
                .tick(SlotTick {
                    epoch: 1.into(),
                    slot: 3.into(),
                })
                .await
                .is_none()
        );
        assert_eq!(
            stream.last_processed_tick,
            Some(
                SlotTick {
                    epoch: 2.into(),
                    slot: 3.into()
                }
                .into()
            )
        );
    }
}
