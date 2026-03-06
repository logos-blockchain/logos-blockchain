mod serde {
    use core::hash::Hash;
    use std::collections::HashSet;

    use lb_blend::message::{
        encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
        reward::{OldSessionBlendingTokenCollector, SessionBlendingTokenCollector},
    };
    use serde::{Deserialize, Serialize};

    use crate::{
        core::state::{error, recovery_state::RecoveryServiceState, service::ServiceState},
        message::ProcessedMessage,
    };

    #[derive(Clone, Serialize, Deserialize)]
    /// Recovery state that is serialized and deserialized to file.
    ///
    /// For details about its fields, check [`ServiceState`].
    pub struct SerializableServiceState<BroadcastSettings, NodeId>
    where
        NodeId: Eq + Hash,
    {
        last_seen_session: u64,
        spent_core_quota: u64,
        #[serde(bound(
            deserialize = "BroadcastSettings: Deserialize<'de> + Eq + core::hash::Hash"
        ))]
        unsent_processed_messages: HashSet<ProcessedMessage<BroadcastSettings>>,
        unsent_data_messages: HashSet<EncapsulatedMessageWithVerifiedPublicHeader>,
        current_session_token_collector: SessionBlendingTokenCollector,
        old_session_token_collector: Option<OldSessionBlendingTokenCollector>,
        banned_peers: HashSet<NodeId>,
    }

    impl<BroadcastSettings, NodeId> SerializableServiceState<BroadcastSettings, NodeId>
    where
        NodeId: Eq + Hash,
    {
        /// Consume the serializable state to create an actual state object, by
        /// passing it an Overwatch
        /// [`overwatch::services::state::StateUpdater`].
        pub fn try_into_state_with_state_updater<BackendSettings>(
            self,
            state_updater: overwatch::services::state::StateUpdater<
                Option<RecoveryServiceState<BackendSettings, BroadcastSettings, NodeId>>,
            >,
        ) -> Result<ServiceState<BackendSettings, BroadcastSettings, NodeId>, error::SessionMismatch>
        where
            BackendSettings: Clone,
            BroadcastSettings: Clone,
            NodeId: Clone,
        {
            ServiceState::new(
                self.last_seen_session,
                self.spent_core_quota,
                self.unsent_processed_messages,
                self.unsent_data_messages,
                self.current_session_token_collector,
                self.old_session_token_collector,
                self.banned_peers,
                state_updater,
            )
        }
    }

    impl<BackendSettings, BroadcastSettings, NodeId>
        From<ServiceState<BackendSettings, BroadcastSettings, NodeId>>
        for SerializableServiceState<BroadcastSettings, NodeId>
    where
        NodeId: Eq + Hash,
    {
        fn from(value: ServiceState<BackendSettings, BroadcastSettings, NodeId>) -> Self {
            let (
                last_seen_session,
                spent_core_quota,
                unsent_processed_messages,
                unsent_data_messages,
                current_session_token_collector,
                old_session_token_collector,
                banned_peers,
                _,
            ) = value.into_components();
            Self {
                last_seen_session,
                spent_core_quota,
                unsent_processed_messages,
                unsent_data_messages,
                current_session_token_collector,
                old_session_token_collector,
                banned_peers,
            }
        }
    }
}

pub use self::service::ServiceState;
mod service {
    use core::{
        fmt::{self, Debug, Formatter},
        hash::Hash,
    };
    use std::collections::HashSet;

    use lb_blend::message::{
        encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
        reward::{BlendingToken, OldSessionBlendingTokenCollector, SessionBlendingTokenCollector},
    };

    use crate::{
        core::state::{error, recovery_state::RecoveryServiceState, state_updater::StateUpdater},
        message::ProcessedMessage,
    };

    #[derive(Clone)]
    /// Recovery state for Blend core service.
    pub struct ServiceState<BackendSettings, BroadcastSettings, NodeId>
    where
        NodeId: Eq + Hash,
    {
        /// The last session that was saved.
        last_seen_session: u64,
        /// The last value for the core quota allowance for the session that is
        /// tracked.
        spent_core_quota: u64,
        unsent_processed_messages: HashSet<ProcessedMessage<BroadcastSettings>>,
        unsent_data_messages: HashSet<EncapsulatedMessageWithVerifiedPublicHeader>,
        current_session_token_collector: SessionBlendingTokenCollector,
        old_session_token_collector: Option<OldSessionBlendingTokenCollector>,
        banned_peers: HashSet<NodeId>,
        state_updater: overwatch::services::state::StateUpdater<
            Option<RecoveryServiceState<BackendSettings, BroadcastSettings, NodeId>>,
        >,
    }

    impl<BackendSettings, BroadcastSettings, NodeId> Debug
        for ServiceState<BackendSettings, BroadcastSettings, NodeId>
    where
        BroadcastSettings: Debug,
        NodeId: Eq + Hash + Debug,
    {
        fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
            f.debug_struct("ServiceState")
                .field("last_seen_session", &self.last_seen_session)
                .field("spent_core_quota", &self.spent_core_quota)
                .field("unsent_processed_messages", &self.unsent_processed_messages)
                .field("unsent_data_messages", &self.unsent_data_messages)
                .field(
                    "current_session_token_collector",
                    &self.current_session_token_collector,
                )
                .field(
                    "old_session_token_collector",
                    &self.old_session_token_collector,
                )
                .field("banned_peers", &self.banned_peers)
                .finish_non_exhaustive()
        }
    }

    impl<BackendSettings, BroadcastSettings, NodeId>
        ServiceState<BackendSettings, BroadcastSettings, NodeId>
    where
        BackendSettings: Clone,
        BroadcastSettings: Clone,
        NodeId: Eq + Hash + Clone,
    {
        // Creates a new instance with the provided fields, and saves it using
        // `state_updater`.
        #[expect(clippy::too_many_arguments, reason = "TODO: Change into a struct.")]
        pub(super) fn new(
            last_seen_session: u64,
            spent_core_quota: u64,
            unsent_processed_messages: HashSet<ProcessedMessage<BroadcastSettings>>,
            unsent_data_messages: HashSet<EncapsulatedMessageWithVerifiedPublicHeader>,
            current_session_token_collector: SessionBlendingTokenCollector,
            old_session_token_collector: Option<OldSessionBlendingTokenCollector>,
            banned_peers: HashSet<NodeId>,
            state_updater: overwatch::services::state::StateUpdater<
                Option<RecoveryServiceState<BackendSettings, BroadcastSettings, NodeId>>,
            >,
        ) -> Result<Self, error::SessionMismatch> {
            // Check if `current_session_token_collector` has the correct session number.
            let provided_current_session = current_session_token_collector.session_number();
            if provided_current_session != last_seen_session {
                return Err(error::SessionMismatch {
                    last_seen: last_seen_session,
                    provided: provided_current_session,
                });
            }

            // Check if `old_session_token_collector` has the correct session number.
            if let Some(old_session_token_collector) = &old_session_token_collector {
                let provided_current_session = old_session_token_collector
                    .session_number()
                    .saturating_add(1);
                if provided_current_session != last_seen_session {
                    return Err(error::SessionMismatch {
                        last_seen: last_seen_session,
                        provided: provided_current_session,
                    });
                }
            }

            let this = Self {
                last_seen_session,
                spent_core_quota,
                unsent_processed_messages,
                unsent_data_messages,
                current_session_token_collector,
                old_session_token_collector,
                banned_peers,
                state_updater,
            };
            this.save();
            Ok(this)
        }

        /// Create a new instance with the provided session, and empty state for
        /// the rest.
        ///
        /// The new instance is saved immediately using `state_updater`.
        ///
        /// This is typically used on session rotations or when no previous
        /// state was recovered.
        pub fn with_session(
            session: u64,
            current_session_token_collector: SessionBlendingTokenCollector,
            old_session_token_collector: Option<OldSessionBlendingTokenCollector>,
            state_updater: overwatch::services::state::StateUpdater<
                Option<RecoveryServiceState<BackendSettings, BroadcastSettings, NodeId>>,
            >,
        ) -> Result<Self, error::SessionMismatch> {
            Self::new(
                session,
                0,
                HashSet::new(),
                HashSet::new(),
                current_session_token_collector,
                old_session_token_collector,
                HashSet::new(),
                state_updater,
            )
        }

        pub(super) fn save(&self) {
            self.state_updater.update(Some(self.clone().into()));
        }
    }

    impl<BackendSettings, BroadcastSettings, NodeId>
        ServiceState<BackendSettings, BroadcastSettings, NodeId>
    where
        NodeId: Eq + Hash,
    {
        /// Consume `self` to return a [`StateUpdater`], which can be used to
        /// batch changes before they are stored using the underlying
        /// [`overwatch::services::state::StateUpdater`].
        pub const fn start_updating(
            self,
        ) -> StateUpdater<BackendSettings, BroadcastSettings, NodeId> {
            StateUpdater::new(self)
        }

        pub const fn last_seen_session(&self) -> u64 {
            self.last_seen_session
        }

        pub(super) const fn spend_quota(&mut self, quota: u64) {
            self.spent_core_quota = self
                .spent_core_quota
                .checked_add(quota)
                .expect("Spent core quota addition overflow.");
        }

        pub const fn spent_quota(&self) -> u64 {
            self.spent_core_quota
        }

        pub(super) fn collect_current_session_tokens(
            &mut self,
            tokens: impl Iterator<Item = BlendingToken>,
        ) {
            for token in tokens {
                self.current_session_token_collector.collect(token);
            }
        }

        pub(super) fn collect_old_session_tokens(
            &mut self,
            tokens: impl Iterator<Item = BlendingToken>,
        ) -> Result<(), error::OldSessionTokenCollectorNotExist> {
            self.old_session_token_collector.as_mut().map_or(
                Err(error::OldSessionTokenCollectorNotExist),
                |collector| {
                    for token in tokens {
                        collector.collect(token);
                    }
                    Ok(())
                },
            )
        }

        pub(super) const fn clear_old_session_token_collector(
            &mut self,
        ) -> Option<OldSessionBlendingTokenCollector> {
            self.old_session_token_collector.take()
        }

        #[cfg(test)]
        pub(crate) const fn current_session_token_collector(
            &self,
        ) -> &SessionBlendingTokenCollector {
            &self.current_session_token_collector
        }

        #[expect(
            clippy::type_complexity,
            reason = "Just a tuple over the struct's fields."
        )]
        pub fn into_components(
            self,
        ) -> (
            u64,
            u64,
            HashSet<ProcessedMessage<BroadcastSettings>>,
            HashSet<EncapsulatedMessageWithVerifiedPublicHeader>,
            SessionBlendingTokenCollector,
            Option<OldSessionBlendingTokenCollector>,
            HashSet<NodeId>,
            overwatch::services::state::StateUpdater<
                Option<RecoveryServiceState<BackendSettings, BroadcastSettings, NodeId>>,
            >,
        ) {
            (
                self.last_seen_session,
                self.spent_core_quota,
                self.unsent_processed_messages,
                self.unsent_data_messages,
                self.current_session_token_collector,
                self.old_session_token_collector,
                self.banned_peers,
                self.state_updater,
            )
        }
    }

    impl<BackendSettings, BroadcastSettings, NodeId>
        ServiceState<BackendSettings, BroadcastSettings, NodeId>
    where
        BroadcastSettings: Eq + Hash,
        NodeId: Eq + Hash,
    {
        pub(super) fn add_unsent_processed_message(
            &mut self,
            message: ProcessedMessage<BroadcastSettings>,
        ) -> Result<(), ()> {
            if self.unsent_processed_messages.insert(message) {
                Ok(())
            } else {
                Err(())
            }
        }

        pub(super) fn remove_sent_processed_message(
            &mut self,
            message: &ProcessedMessage<BroadcastSettings>,
        ) -> Result<(), ()> {
            if self.unsent_processed_messages.remove(message) {
                Ok(())
            } else {
                Err(())
            }
        }

        /// Reference to the messages currently marked as unsent.
        pub const fn unsent_processed_messages(
            &self,
        ) -> &HashSet<ProcessedMessage<BroadcastSettings>> {
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
    }
}

pub use self::state_updater::StateUpdater;
mod state_updater {
    use core::hash::Hash;

    use lb_blend::message::{
        encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
        reward::{BlendingToken, OldSessionBlendingTokenCollector},
    };

    use crate::{
        core::state::{error, service::ServiceState},
        message::ProcessedMessage,
    };

    /// A state updater which gathers changes to the underlying [`ServiceState`]
    /// before committing them via the underlying
    /// [`overwatch::services::state::StateUpdater`].
    pub struct StateUpdater<BackendSettings, BroadcastSettings, NodeId>
    where
        NodeId: Eq + Hash,
    {
        inner: ServiceState<BackendSettings, BroadcastSettings, NodeId>,
        /// Flag indicating whether ANY changes happened since this object
        /// creation.
        changed: bool,
    }

    impl<BackendSettings, BroadcastSettings, NodeId>
        StateUpdater<BackendSettings, BroadcastSettings, NodeId>
    where
        NodeId: Eq + Hash,
    {
        pub(super) const fn new(
            inner: ServiceState<BackendSettings, BroadcastSettings, NodeId>,
        ) -> Self {
            Self {
                inner,
                changed: false,
            }
        }

        pub fn into_inner(self) -> ServiceState<BackendSettings, BroadcastSettings, NodeId> {
            self.inner
        }

        pub const fn consume_core_quota(&mut self, amount: u64) {
            self.changed = true;
            self.inner.spend_quota(amount);
        }

        /// Consumes `self` and return the state with any changes applied to it,
        /// without storing those changes via the underlying
        /// `overwatch::services::state::StateUpdater`.
        ///
        /// It is important to note that it is not equivalent to calling
        /// rollback, since any changes applied before calling this function
        /// will still be applied to the returned object.
        /// In case the original state is needed, it needs to be `.clone()`d
        /// before consuming it to produce this state updater instance.
        pub fn consume_without_committing(
            self,
        ) -> ServiceState<BackendSettings, BroadcastSettings, NodeId> {
            self.inner
        }

        pub fn collect_current_session_tokens(
            &mut self,
            tokens: impl Iterator<Item = BlendingToken>,
        ) {
            self.changed = true;
            self.inner.collect_current_session_tokens(tokens);
        }

        pub fn collect_old_session_tokens(
            &mut self,
            tokens: impl Iterator<Item = BlendingToken>,
        ) -> Result<(), error::OldSessionTokenCollectorNotExist> {
            self.changed = true;
            self.inner.collect_old_session_tokens(tokens)
        }

        pub const fn clear_old_session_token_collector(
            &mut self,
        ) -> Option<OldSessionBlendingTokenCollector> {
            self.changed = true;
            self.inner.clear_old_session_token_collector()
        }
    }

    impl<BackendSettings, BroadcastSettings, NodeId>
        StateUpdater<BackendSettings, BroadcastSettings, NodeId>
    where
        BackendSettings: Clone,
        BroadcastSettings: Clone,
        NodeId: Eq + Hash + Clone,
    {
        /// Consumes `self` and stores the latest state via the underlying
        /// `overwatch::services::state::StateUpdater`, returning the updated
        /// [`ServiceState`].
        pub fn commit_changes(self) -> ServiceState<BackendSettings, BroadcastSettings, NodeId> {
            if self.changed {
                self.inner.save();
            }
            self.inner
        }
    }

    impl<BackendSettings, BroadcastSettings, NodeId>
        StateUpdater<BackendSettings, BroadcastSettings, NodeId>
    where
        BroadcastSettings: Eq + Hash,
        NodeId: Eq + Hash,
    {
        /// Mark a new [`ProcessedMessage`] as unsent, meaning that it has been
        /// decapsulated and scheduled for release but not yet released.
        ///
        /// It returns `Ok` if the message was not already present, `Err`
        /// otherwise.
        pub fn add_unsent_processed_message(
            &mut self,
            message: ProcessedMessage<BroadcastSettings>,
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
            message: &ProcessedMessage<BroadcastSettings>,
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
    }
}

pub use self::recovery_state::RecoveryServiceState;
mod recovery_state {
    use core::{convert::Infallible, hash::Hash, marker::PhantomData};

    use serde::{Deserialize, Serialize};

    use crate::core::{
        settings::StartingBlendConfig as BlendConfig,
        state::{ServiceState, serde::SerializableServiceState},
    };

    #[derive(Clone, Serialize, Deserialize)]
    /// Recovery state type as expected by the file-based recovery operator.
    ///
    /// This type is required since Overwatch does not allow for recovered state
    /// to be `None`, hence we need to wrap the actual state into this type to
    /// make it an `Option`.
    ///
    /// If Overwatch will start supporting optional states, this type will most
    /// likely go.
    pub struct RecoveryServiceState<BackendSettings, BroadcastSettings, NodeId>
    where
        NodeId: Eq + Hash,
    {
        #[serde(bound(
            deserialize = "BroadcastSettings: Deserialize<'de> + Eq + core::hash::Hash, NodeId: Deserialize<'de>"
        ))]
        pub service_state: Option<SerializableServiceState<BroadcastSettings, NodeId>>,
        _phantom: PhantomData<BackendSettings>,
    }

    impl<BackendSettings, BroadcastSettings, NodeId>
        From<ServiceState<BackendSettings, BroadcastSettings, NodeId>>
        for RecoveryServiceState<BackendSettings, BroadcastSettings, NodeId>
    where
        NodeId: Eq + Hash,
    {
        fn from(value: ServiceState<BackendSettings, BroadcastSettings, NodeId>) -> Self {
            Self {
                _phantom: PhantomData,
                service_state: Some(value.into()),
            }
        }
    }

    impl<BackendSettings, BroadcastSettings, NodeId> overwatch::services::state::ServiceState
        for RecoveryServiceState<BackendSettings, BroadcastSettings, NodeId>
    where
        NodeId: Eq + Hash,
    {
        type Error = Infallible;
        type Settings = BlendConfig<BackendSettings>;

        fn from_settings(_: &Self::Settings) -> Result<Self, Self::Error> {
            Ok(Self {
                _phantom: PhantomData,
                service_state: None,
            })
        }
    }
}

pub mod error {
    use lb_core::sdp::SessionNumber;

    #[derive(Debug)]
    pub struct SessionMismatch {
        pub last_seen: SessionNumber,
        pub provided: SessionNumber,
    }

    #[derive(Debug)]
    pub struct OldSessionTokenCollectorNotExist;
}
