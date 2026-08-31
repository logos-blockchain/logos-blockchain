mod serde {
    use std::collections::{HashSet, VecDeque};

    use lb_blend::message::{
        encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
        reward::{EpochBlendingTokenCollector, OldEpochBlendingTokenCollector},
    };
    use lb_chain_service::Epoch;
    use lb_poq::Quota;
    use serde::{Deserialize, Serialize};

    use crate::{
        core::state::{error, recovery_state::RecoveryServiceState, service::ServiceState},
        message::ProcessedMessage,
    };

    #[derive(Clone, Serialize, Deserialize)]
    /// Recovery state that is serialized and deserialized to file.
    ///
    /// For details about its fields, check [`ServiceState`].
    pub struct SerializableServiceState {
        last_seen_epoch: Epoch,
        spent_core_quota: Quota,
        unsent_processed_messages: HashSet<ProcessedMessage>,
        unsent_data_messages: HashSet<EncapsulatedMessageWithVerifiedPublicHeader>,
        pending_transactions: VecDeque<Vec<u8>>,
        current_epoch_token_collector: EpochBlendingTokenCollector,
        old_epoch_token_collector: Option<OldEpochBlendingTokenCollector>,
    }

    impl SerializableServiceState {
        /// Consume the serializable state to create an actual state object, by
        /// passing it an Overwatch
        /// [`overwatch::services::state::StateUpdater`].
        pub fn try_into_state_with_state_updater<ServiceSettings>(
            self,
            state_updater: overwatch::services::state::StateUpdater<
                Option<RecoveryServiceState<ServiceSettings>>,
            >,
        ) -> Result<ServiceState<ServiceSettings>, error::EpochMismatch>
        where
            ServiceSettings: Clone,
        {
            ServiceState::new(
                self.last_seen_epoch,
                self.spent_core_quota,
                self.unsent_processed_messages,
                self.unsent_data_messages,
                self.pending_transactions,
                self.current_epoch_token_collector,
                self.old_epoch_token_collector,
                state_updater,
            )
        }
    }

    impl<ServiceSettings> From<ServiceState<ServiceSettings>> for SerializableServiceState {
        fn from(value: ServiceState<ServiceSettings>) -> Self {
            let (
                last_seen_epoch,
                spent_core_quota,
                unsent_processed_messages,
                unsent_data_messages,
                pending_transactions,
                current_epoch_token_collector,
                old_epoch_token_collector,
                _,
            ) = value.into_components();
            Self {
                last_seen_epoch,
                spent_core_quota,
                unsent_processed_messages,
                unsent_data_messages,
                pending_transactions,
                current_epoch_token_collector,
                old_epoch_token_collector,
            }
        }
    }
}

pub use self::service::ServiceState;
mod service {
    use core::fmt::{self, Debug, Formatter};
    use std::collections::{HashSet, VecDeque};

    use lb_blend::message::{
        encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
        reward::{BlendingToken, EpochBlendingTokenCollector, OldEpochBlendingTokenCollector},
    };
    use lb_chain_service::Epoch;
    use lb_poq::Quota;

    use crate::{
        core::state::{error, recovery_state::RecoveryServiceState, state_updater::StateUpdater},
        message::ProcessedMessage,
    };

    /// Recovery state for Blend core service.
    pub struct ServiceState<ServiceSettings> {
        /// The last epoch that was saved.
        last_seen_epoch: Epoch,
        /// The last value for the core quota allowance for the epoch that is
        /// tracked.
        spent_core_quota: Quota,
        unsent_processed_messages: HashSet<ProcessedMessage>,
        unsent_data_messages: HashSet<EncapsulatedMessageWithVerifiedPublicHeader>,
        /// Transactions handed over for blending that are still waiting for a
        /// `PoW` solution to back their layer proofs.
        pending_transactions: VecDeque<Vec<u8>>,
        current_epoch_token_collector: EpochBlendingTokenCollector,
        old_epoch_token_collector: Option<OldEpochBlendingTokenCollector>,
        state_updater:
            overwatch::services::state::StateUpdater<Option<RecoveryServiceState<ServiceSettings>>>,
    }

    impl<ServiceSettings> Clone for ServiceState<ServiceSettings> {
        fn clone(&self) -> Self {
            Self {
                last_seen_epoch: self.last_seen_epoch,
                spent_core_quota: self.spent_core_quota,
                unsent_processed_messages: self.unsent_processed_messages.clone(),
                unsent_data_messages: self.unsent_data_messages.clone(),
                pending_transactions: self.pending_transactions.clone(),
                current_epoch_token_collector: self.current_epoch_token_collector.clone(),
                old_epoch_token_collector: self.old_epoch_token_collector.clone(),
                state_updater: self.state_updater.clone(),
            }
        }
    }

    impl<ServiceSettings> Debug for ServiceState<ServiceSettings> {
        fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
            f.debug_struct("ServiceState")
                .field("last_seen_epoch", &self.last_seen_epoch)
                .field("spent_core_quota", &self.spent_core_quota)
                .field("unsent_processed_messages", &self.unsent_processed_messages)
                .field("unsent_data_messages", &self.unsent_data_messages)
                .field("pending_transactions", &self.pending_transactions.len())
                .field(
                    "current_epoch_token_collector",
                    &self.current_epoch_token_collector,
                )
                .field("old_epoch_token_collector", &self.old_epoch_token_collector)
                .finish_non_exhaustive()
        }
    }

    impl<ServiceSettings> ServiceState<ServiceSettings>
    where
        ServiceSettings: Clone,
    {
        // Creates a new instance with the provided fields, and saves it using
        // `state_updater`.
        #[expect(
            clippy::too_many_arguments,
            reason = "One argument per persisted field."
        )]
        pub(super) fn new(
            last_seen_epoch: Epoch,
            spent_core_quota: Quota,
            unsent_processed_messages: HashSet<ProcessedMessage>,
            unsent_data_messages: HashSet<EncapsulatedMessageWithVerifiedPublicHeader>,
            pending_transactions: VecDeque<Vec<u8>>,
            current_epoch_token_collector: EpochBlendingTokenCollector,
            old_epoch_token_collector: Option<OldEpochBlendingTokenCollector>,
            state_updater: overwatch::services::state::StateUpdater<
                Option<RecoveryServiceState<ServiceSettings>>,
            >,
        ) -> Result<Self, error::EpochMismatch> {
            // Check if `current_epoch_token_collector` has the correct epoch number.
            let provided_current_epoch = current_epoch_token_collector.epoch();
            if provided_current_epoch != last_seen_epoch {
                return Err(error::EpochMismatch {
                    last_seen: last_seen_epoch,
                    provided: provided_current_epoch,
                });
            }

            // Check if `old_epoch_token_collector` has the correct epoch number.
            if let Some(old_epoch_token_collector) = &old_epoch_token_collector {
                let provided_current_epoch = old_epoch_token_collector.epoch().strict_add(1.into());
                if provided_current_epoch != last_seen_epoch {
                    return Err(error::EpochMismatch {
                        last_seen: last_seen_epoch,
                        provided: provided_current_epoch,
                    });
                }
            }

            let this = Self {
                last_seen_epoch,
                spent_core_quota,
                unsent_processed_messages,
                unsent_data_messages,
                pending_transactions,
                current_epoch_token_collector,
                old_epoch_token_collector,
                state_updater,
            };
            this.save();
            Ok(this)
        }

        /// Create a new instance with the provided epoch, and empty state for
        /// the rest.
        ///
        /// The new instance is saved immediately using `state_updater`.
        ///
        /// This is typically used on epoch rotations or when no previous
        /// state was recovered. `pending_transactions` is carried in
        /// rather than emptied: a transaction that has not been encapsulated
        /// yet is tied to no epoch, so an epoch rotation is no reason to lose
        /// it.
        pub fn with_epoch(
            epoch: Epoch,
            pending_transactions: VecDeque<Vec<u8>>,
            current_epoch_token_collector: EpochBlendingTokenCollector,
            old_epoch_token_collector: Option<OldEpochBlendingTokenCollector>,
            state_updater: overwatch::services::state::StateUpdater<
                Option<RecoveryServiceState<ServiceSettings>>,
            >,
        ) -> Result<Self, error::EpochMismatch> {
            Self::new(
                epoch,
                Quota::ZERO,
                HashSet::new(),
                HashSet::new(),
                pending_transactions,
                current_epoch_token_collector,
                old_epoch_token_collector,
                state_updater,
            )
        }

        pub(super) fn save(&self) {
            self.state_updater.update(Some(self.clone().into()));
        }
    }

    impl<ServiceSettings> ServiceState<ServiceSettings> {
        /// Consume `self` to return a [`StateUpdater`], which can be used to
        /// batch changes before they are stored using the underlying
        /// [`overwatch::services::state::StateUpdater`].
        pub const fn start_updating(self) -> StateUpdater<ServiceSettings> {
            StateUpdater::new(self)
        }

        pub const fn last_seen_epoch(&self) -> Epoch {
            self.last_seen_epoch
        }

        pub(super) const fn spend_quota(&mut self, quota: Quota) {
            self.spent_core_quota = match self.spent_core_quota.checked_add(quota) {
                Some(spent) => spent,
                None => panic!("Spent core quota addition overflow."),
            };
        }

        pub const fn spent_quota(&self) -> Quota {
            self.spent_core_quota
        }

        pub(super) fn collect_current_epoch_tokens(
            &mut self,
            tokens: impl Iterator<Item = BlendingToken>,
        ) {
            for token in tokens {
                self.current_epoch_token_collector.collect(token);
            }
        }

        pub(super) fn collect_old_epoch_tokens(
            &mut self,
            tokens: impl Iterator<Item = BlendingToken>,
        ) -> Result<(), error::OldEpochTokenCollectorNotExist> {
            self.old_epoch_token_collector.as_mut().map_or(
                Err(error::OldEpochTokenCollectorNotExist),
                |collector| {
                    for token in tokens {
                        collector.collect(token);
                    }
                    Ok(())
                },
            )
        }

        pub(super) const fn clear_old_epoch_token_collector(
            &mut self,
        ) -> Option<OldEpochBlendingTokenCollector> {
            self.old_epoch_token_collector.take()
        }

        #[cfg(test)]
        pub(crate) const fn current_epoch_token_collector(&self) -> &EpochBlendingTokenCollector {
            &self.current_epoch_token_collector
        }

        #[expect(
            clippy::type_complexity,
            reason = "Just a tuple over the struct's fields."
        )]
        pub fn into_components(
            self,
        ) -> (
            Epoch,
            Quota,
            HashSet<ProcessedMessage>,
            HashSet<EncapsulatedMessageWithVerifiedPublicHeader>,
            VecDeque<Vec<u8>>,
            EpochBlendingTokenCollector,
            Option<OldEpochBlendingTokenCollector>,
            overwatch::services::state::StateUpdater<Option<RecoveryServiceState<ServiceSettings>>>,
        ) {
            (
                self.last_seen_epoch,
                self.spent_core_quota,
                self.unsent_processed_messages,
                self.unsent_data_messages,
                self.pending_transactions,
                self.current_epoch_token_collector,
                self.old_epoch_token_collector,
                self.state_updater,
            )
        }

        pub(super) fn add_unsent_processed_message(
            &mut self,
            message: ProcessedMessage,
        ) -> Result<(), ()> {
            if self.unsent_processed_messages.insert(message) {
                Ok(())
            } else {
                Err(())
            }
        }

        pub(super) fn remove_sent_processed_message(
            &mut self,
            message: &ProcessedMessage,
        ) -> Result<(), ()> {
            if self.unsent_processed_messages.remove(message) {
                Ok(())
            } else {
                Err(())
            }
        }

        /// Reference to the messages currently marked as unsent.
        pub const fn unsent_processed_messages(&self) -> &HashSet<ProcessedMessage> {
            &self.unsent_processed_messages
        }

        pub(super) fn add_unsent_data_message(
            &mut self,
            message: EncapsulatedMessageWithVerifiedPublicHeader,
        ) -> Result<(), ()> {
            if self.unsent_data_messages.insert(message) {
                Ok(())
            } else {
                Err(())
            }
        }

        pub(super) fn remove_sent_data_message(
            &mut self,
            message: &EncapsulatedMessageWithVerifiedPublicHeader,
        ) -> Result<(), ()> {
            if self.unsent_data_messages.remove(message) {
                Ok(())
            } else {
                Err(())
            }
        }

        pub const fn unsent_data_messages(
            &self,
        ) -> &HashSet<EncapsulatedMessageWithVerifiedPublicHeader> {
            &self.unsent_data_messages
        }

        pub(super) fn queue_pending_transaction(&mut self, transaction: Vec<u8>) {
            self.pending_transactions.push_back(transaction);
        }

        pub(super) fn dequeue_transaction(&mut self, expected_transaction: &[u8]) {
            assert_eq!(
                self.pending_transactions.pop_front().as_deref(),
                Some(expected_transaction),
                "Expected transaction to be dequeued does not match the oldest pending transaction."
            );
        }

        /// The transactions still waiting for a `PoW` solution, oldest first.
        ///
        /// This is what a restarting service reads to refill the queue it works
        /// from.
        pub const fn pending_transactions(&self) -> &VecDeque<Vec<u8>> {
            &self.pending_transactions
        }
    }
}

pub use self::state_updater::StateUpdater;
mod state_updater {

    use lb_blend::message::{
        encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
        reward::{BlendingToken, OldEpochBlendingTokenCollector},
    };
    use lb_poq::Quota;

    use crate::{
        core::state::{error, service::ServiceState},
        message::ProcessedMessage,
    };

    /// A state updater which gathers changes to the underlying [`ServiceState`]
    /// before committing them via the underlying
    /// [`overwatch::services::state::StateUpdater`].
    pub struct StateUpdater<ServiceSettings> {
        inner: ServiceState<ServiceSettings>,
        /// Flag indicating whether ANY changes happened since this object
        /// creation.
        changed: bool,
    }

    impl<ServiceSettings> StateUpdater<ServiceSettings> {
        pub(super) const fn new(inner: ServiceState<ServiceSettings>) -> Self {
            Self {
                inner,
                changed: false,
            }
        }

        pub fn into_inner(self) -> ServiceState<ServiceSettings> {
            self.inner
        }

        pub const fn consume_core_quota(&mut self, amount: Quota) {
            self.changed = true;
            self.inner.spend_quota(amount);
        }

        pub fn collect_current_epoch_tokens(
            &mut self,
            tokens: impl Iterator<Item = BlendingToken>,
        ) {
            self.changed = true;
            self.inner.collect_current_epoch_tokens(tokens);
        }

        pub fn collect_old_epoch_tokens(
            &mut self,
            tokens: impl Iterator<Item = BlendingToken>,
        ) -> Result<(), error::OldEpochTokenCollectorNotExist> {
            self.changed = true;
            self.inner.collect_old_epoch_tokens(tokens)
        }

        pub const fn clear_old_epoch_token_collector(
            &mut self,
        ) -> Option<OldEpochBlendingTokenCollector> {
            self.changed = true;
            self.inner.clear_old_epoch_token_collector()
        }

        /// Mark a new [`ProcessedMessage`] as unsent, meaning that it has been
        /// decapsulated and scheduled for release but not yet released.
        ///
        /// It returns `Ok` if the message was not already present, `Err`
        /// otherwise.
        pub fn add_unsent_processed_message(
            &mut self,
            message: ProcessedMessage,
        ) -> Result<(), ()> {
            self.changed = true;
            self.inner.add_unsent_processed_message(message)
        }

        /// Mark a new [`ProcessedMessage`] as sent, meaning that it has been
        /// released by the Blend release module.
        ///
        /// It returns `Ok` if the message was correctly removed (i.e. it was
        /// found), `Err` otherwise.
        pub fn remove_sent_processed_message(
            &mut self,
            message: &ProcessedMessage,
        ) -> Result<(), ()> {
            self.changed = true;
            self.inner.remove_sent_processed_message(message)
        }

        /// Mark a new [`EncapsulatedMessageWithVerifiedPublicHeader`] as
        /// unsent, meaning that it has been scheduled for release but
        /// not yet released.
        ///
        /// It returns `Ok` if the message was not already present, `Err`
        /// otherwise.
        pub fn add_unsent_data_message(
            &mut self,
            message: EncapsulatedMessageWithVerifiedPublicHeader,
        ) -> Result<(), ()> {
            self.changed = true;
            self.inner.add_unsent_data_message(message)
        }

        /// Mark a new [`EncapsulatedMessageWithVerifiedPublicHeader`] as sent,
        /// meaning that it has been released by the Blend release
        /// module.
        ///
        /// It returns `Ok` if the message was correctly removed (i.e. it was
        /// found), `Err` otherwise.
        pub fn remove_sent_data_message(
            &mut self,
            message: &EncapsulatedMessageWithVerifiedPublicHeader,
        ) -> Result<(), ()> {
            self.changed = true;
            self.inner.remove_sent_data_message(message)
        }

        /// Record a transaction as waiting for a `PoW` solution to back its
        /// layer proofs.
        pub fn queue_unencapsulated_transaction(&mut self, transaction: Vec<u8>) {
            self.changed = true;
            self.inner.queue_pending_transaction(transaction);
        }

        /// Take the longest-waiting transaction off the queue, whether it went
        /// on to be encapsulated or could not be.
        ///
        /// `expected_transaction` is what the caller believes is at the head,
        /// so that a drift between this queue and the one the event
        /// loop works from is caught here rather than silently dropping
        /// the wrong transaction.
        pub fn dequeue_unencapsulated_transaction(&mut self, expected_transaction: &[u8]) {
            self.changed = true;
            self.inner.dequeue_transaction(expected_transaction);
        }
    }

    impl<ServiceSettings> StateUpdater<ServiceSettings>
    where
        ServiceSettings: Clone,
    {
        pub fn commit_changes(self) -> ServiceState<ServiceSettings> {
            if self.changed {
                self.inner.save();
            }
            self.inner
        }
    }
}

pub use self::recovery_state::RecoveryServiceState;
mod recovery_state {
    use core::{convert::Infallible, marker::PhantomData};

    use serde::{Deserialize, Serialize};

    use crate::core::state::{ServiceState, serde::SerializableServiceState};

    #[derive(Clone, Serialize, Deserialize)]
    /// Recovery state type as expected by the file-based recovery operator.
    ///
    /// This type is required since Overwatch does not allow for recovered state
    /// to be `None`, hence we need to wrap the actual state into this type to
    /// make it an `Option`.
    ///
    /// If Overwatch will start supporting optional states, this type will most
    /// likely go.
    pub struct RecoveryServiceState<ServiceSettings> {
        pub service_state: Option<SerializableServiceState>,
        /// Type-level tie to the service's settings, which contribute no
        /// persisted data. Overwatch requires `ServiceState::Settings` to equal
        /// `ServiceData::Settings`, so this *is* that settings type rather than
        /// the pieces it is built from — one service covering three modes takes
        /// one settings struct, and it is not the core mode's.
        _phantom: PhantomData<fn() -> ServiceSettings>,
    }

    impl<ServiceSettings> From<ServiceState<ServiceSettings>>
        for RecoveryServiceState<ServiceSettings>
    {
        fn from(value: ServiceState<ServiceSettings>) -> Self {
            Self {
                _phantom: PhantomData,
                service_state: Some(value.into()),
            }
        }
    }

    impl<ServiceSettings> overwatch::services::state::ServiceState
        for RecoveryServiceState<ServiceSettings>
    {
        type Error = Infallible;
        type Settings = ServiceSettings;

        fn from_settings(_: &Self::Settings) -> Result<Self, Self::Error> {
            Ok(Self {
                _phantom: PhantomData,
                service_state: None,
            })
        }
    }
}

pub mod error {
    use lb_chain_service::Epoch;

    #[derive(Debug)]
    pub struct EpochMismatch {
        pub last_seen: Epoch,
        pub provided: Epoch,
    }

    #[derive(Debug)]
    pub struct OldEpochTokenCollectorNotExist;
}
