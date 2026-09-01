use core::{num::NonZeroU64, time::Duration};
use std::{
    fmt::{Debug, Display},
    hash::Hash,
};

use lb_blend::scheduling::epoch::EpochEvent;
use overwatch::{
    overwatch::OverwatchHandle,
    services::{AsServiceId, ServiceData},
};
use tracing::info;

use crate::{
    membership::MembershipInfo,
    mode::Mode,
    orchestrator::{self, OnDemandServiceMode},
};

/// Which mode is running, and what is still winding down behind it.
///
/// All three modes are Overwatch services now, so the three variants that name
/// one are the same shape. The two `*AfterCore` variants are the exception, and
/// they earn it: a core node leaving core mode still owes a transition period's
/// worth of releases and an activity proof, so the old service stays alive
/// alongside the new one until the period expires. That overlap is the only
/// reason this is a five-state machine rather than a three-state one.
pub enum Instance<CoreService, EdgeService, BroadcastService, RuntimeServiceId>
where
    CoreService: ServiceData,
    EdgeService: ServiceData,
    BroadcastService: ServiceData,
{
    Core(OnDemandServiceMode<CoreService, RuntimeServiceId>),
    Edge(OnDemandServiceMode<EdgeService, RuntimeServiceId>),
    EdgeAfterCore {
        mode: OnDemandServiceMode<EdgeService, RuntimeServiceId>,
        /// Kept for the epoch transition period.
        prev: OnDemandServiceMode<CoreService, RuntimeServiceId>,
    },
    Broadcast(OnDemandServiceMode<BroadcastService, RuntimeServiceId>),
    BroadcastAfterCore {
        mode: OnDemandServiceMode<BroadcastService, RuntimeServiceId>,
        /// Kept for the epoch transition period.
        prev: OnDemandServiceMode<CoreService, RuntimeServiceId>,
    },
}

/// How long a core service is given to finish draining before it is killed. It
/// gets the longest grace because it is the only mode with anything to wind
/// down.
const CORE_SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
/// The other two hold nothing that outlives them.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

impl<CoreService, EdgeService, BroadcastService, RuntimeServiceId>
    Instance<CoreService, EdgeService, BroadcastService, RuntimeServiceId>
where
    CoreService: ServiceData<Message: Send + 'static> + 'static,
    EdgeService: ServiceData<Message = CoreService::Message> + 'static,
    BroadcastService: ServiceData<Message = CoreService::Message> + 'static,
    RuntimeServiceId: AsServiceId<CoreService>
        + AsServiceId<EdgeService>
        + AsServiceId<BroadcastService>
        + Debug
        + Display
        + Clone
        + Send
        + Sync
        + 'static,
{
    /// Starts the mode this membership calls for.
    pub async fn new(
        mode: Mode,
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Result<Self, orchestrator::Error> {
        Ok(match mode {
            Mode::Core => Self::Core(OnDemandServiceMode::new(overwatch_handle.clone()).await?),
            Mode::Edge => Self::Edge(OnDemandServiceMode::new(overwatch_handle.clone()).await?),
            Mode::Broadcast => {
                Self::Broadcast(OnDemandServiceMode::new(overwatch_handle.clone()).await?)
            }
        })
    }

    /// Hands an inbound message to whichever mode is running.
    ///
    /// A draining core never gets one: it is finishing what it already has.
    pub async fn handle_inbound_message(
        &self,
        message: CoreService::Message,
    ) -> Result<(), orchestrator::Error> {
        match self {
            Self::Core(mode) => mode.handle_inbound_message(message).await,
            Self::Edge(mode) | Self::EdgeAfterCore { mode, .. } => {
                mode.handle_inbound_message(message).await
            }
            Self::Broadcast(mode) | Self::BroadcastAfterCore { mode, .. } => {
                mode.handle_inbound_message(message).await
            }
        }
    }

    /// Reacts to an epoch event, possibly switching modes.
    pub async fn handle_epoch_event<NodeId>(
        self,
        event: EpochEvent<MembershipInfo<NodeId>>,
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
        minimum_network_size: NonZeroU64,
    ) -> Result<Self, orchestrator::Error>
    where
        NodeId: Eq + Hash,
    {
        match event {
            EpochEvent::NewEpoch(MembershipInfo { membership, .. }) => {
                self.transition(
                    Mode::choose(&membership, minimum_network_size),
                    overwatch_handle,
                )
                .await
            }
            EpochEvent::TransitionPeriodExpired => {
                Ok(self.handle_transition_period_expired().await)
            }
        }
    }

    /// Switches to `to_mode`, keeping a draining core alongside if there is
    /// one.
    ///
    /// Three rules, and they are the whole transition matrix:
    /// - already in `to_mode`: stay, including while a core drains behind it;
    /// - leaving core: start the new mode and keep the core draining;
    /// - anything else: stop what is running, then start the new mode.
    async fn transition(
        self,
        to_mode: Mode,
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Result<Self, orchestrator::Error> {
        let previous_mode = self.mode();
        if previous_mode == to_mode {
            // Already serving this mode. If a core is still draining behind
            // it, a fresh epoch calling for the same mode settles the overlap
            // just as the transition period expiring would — which is the
            // safety net for an expiry that never arrives.
            let settled = self.handle_transition_period_expired().await;
            log_mode_applied(previous_mode, to_mode, settled.mode());
            return Ok(settled);
        }
        let leaving_core = match self {
            Self::Core(core) => Some(core),
            other => {
                other.stop().await;
                None
            }
        };
        let next = match (to_mode, leaving_core) {
            (Mode::Core, _) => {
                Self::Core(OnDemandServiceMode::new(overwatch_handle.clone()).await?)
            }
            (Mode::Edge, None) => {
                Self::Edge(OnDemandServiceMode::new(overwatch_handle.clone()).await?)
            }
            (Mode::Edge, Some(prev)) => Self::EdgeAfterCore {
                mode: OnDemandServiceMode::new(overwatch_handle.clone()).await?,
                prev,
            },
            (Mode::Broadcast, None) => {
                Self::Broadcast(OnDemandServiceMode::new(overwatch_handle.clone()).await?)
            }
            (Mode::Broadcast, Some(prev)) => Self::BroadcastAfterCore {
                mode: OnDemandServiceMode::new(overwatch_handle.clone()).await?,
                prev,
            },
        };
        log_mode_applied(previous_mode, to_mode, next.mode());
        Ok(next)
    }

    /// Which mode is serving messages right now.
    const fn mode(&self) -> Mode {
        match self {
            Self::Core(_) => Mode::Core,
            Self::Edge(_) | Self::EdgeAfterCore { .. } => Mode::Edge,
            Self::Broadcast(_) | Self::BroadcastAfterCore { .. } => Mode::Broadcast,
        }
    }

    /// Lets the draining core go, now that its transition period is over.
    async fn handle_transition_period_expired(self) -> Self {
        match self {
            Self::EdgeAfterCore { mode, prev } => {
                prev.wait_until_stopped_or_kill(CORE_SHUTDOWN_GRACE).await;
                Self::Edge(mode)
            }
            Self::BroadcastAfterCore { mode, prev } => {
                prev.wait_until_stopped_or_kill(CORE_SHUTDOWN_GRACE).await;
                Self::Broadcast(mode)
            }
            already_settled => already_settled,
        }
    }

    /// Stops everything this instance holds, draining core included.
    async fn stop(self) {
        match self {
            Self::Core(mode) => {
                mode.wait_until_stopped_or_kill(CORE_SHUTDOWN_GRACE).await;
            }
            Self::Edge(mode) => {
                mode.wait_until_stopped_or_kill(SHUTDOWN_GRACE).await;
            }
            Self::Broadcast(mode) => {
                mode.wait_until_stopped_or_kill(SHUTDOWN_GRACE).await;
            }
            Self::EdgeAfterCore { mode, prev } => {
                mode.wait_until_stopped_or_kill(SHUTDOWN_GRACE).await;
                prev.wait_until_stopped_or_kill(CORE_SHUTDOWN_GRACE).await;
            }
            Self::BroadcastAfterCore { mode, prev } => {
                mode.wait_until_stopped_or_kill(SHUTDOWN_GRACE).await;
                prev.wait_until_stopped_or_kill(CORE_SHUTDOWN_GRACE).await;
            }
        }
    }
}

/// Reports which mode ended up running, and whether that was a change.
///
/// Carried over from the TSI-outage diagnostics: these two events are how an
/// operator sees a node's mode decisions in the log.
fn log_mode_applied(previous_mode: Mode, selected_mode: Mode, resulting_mode: Mode) {
    let mode_changed = previous_mode != resulting_mode;
    info!(
        target: crate::LOG_TARGET,
        diagnostic = "blend_tsi_outage",
        event = "blend_mode_applied",
        selected_mode = selected_mode.as_ref(),
        previous_mode = previous_mode.as_ref(),
        resulting_mode = resulting_mode.as_ref(),
        mode_changed,
        "Applied selected Blend mode"
    );
    if mode_changed {
        info!(
            target: crate::LOG_TARGET,
            diagnostic = "blend_tsi_outage",
            event = "blend_mode_changed",
            previous_mode = previous_mode.as_ref(),
            new_mode = resulting_mode.as_ref(),
            "Blend mode changed"
        );
    }
}

#[cfg(test)]
mod tests {

    use lb_blend::scheduling::membership::{Membership, Node};
    use lb_key_management_system_service::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
    use libp2p::Multiaddr;
    use overwatch::{
        DynError, OpaqueServiceResourcesHandle,
        overwatch::OverwatchRunner,
        services::{
            ServiceCore,
            state::{NoOperator, NoState},
        },
    };
    use tokio::time::sleep;

    use super::*;
    use crate::message::ServiceMessage;

    const LOCAL_NODE_ID: u8 = 99;

    /// Check if the instance is initialized successfully for each mode.
    #[test]
    fn test_new() {
        let app = OverwatchRunner::<Services>::run(settings(), None).unwrap();
        app.runtime().handle().block_on(async {
            let handle = app.handle();

            // Check if the Core instance is created successfully.
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            assert!(matches!(instance, Instance::Core(_)));
            instance.stop().await;

            // Check if the Edge instance is created successfully.
            let instance = TestInstance::new(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::Edge(_)));
            instance.stop().await;

            // Check if the Broadcast instance is created successfully.
            let instance = TestInstance::new(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::Broadcast(_)));
            instance.stop().await;
        });
    }

    /// Check if the instance transitions to Core mode correctly from all other
    /// modes.
    #[test]
    fn test_transition_to_core() {
        let app = OverwatchRunner::<Services>::run(settings(), None).unwrap();
        app.runtime().handle().block_on(async {
            let handle = app.handle();

            // Core -> Core
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Core, handle).await.unwrap();
            assert!(matches!(instance, Instance::Core(_)));
            instance.stop().await;

            // Edge -> Core
            let instance = TestInstance::new(Mode::Edge, handle).await.unwrap();
            let instance = instance.transition(Mode::Core, handle).await.unwrap();
            assert!(matches!(instance, Instance::Core(_)));
            instance.stop().await;

            // EdgeAfterCore -> Core
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::EdgeAfterCore { .. }));
            let instance = instance.transition(Mode::Core, handle).await.unwrap();
            assert!(matches!(instance, Instance::Core(_)));
            instance.stop().await;

            // Broadcast -> Core
            let instance = TestInstance::new(Mode::Broadcast, handle).await.unwrap();
            let instance = instance.transition(Mode::Core, handle).await.unwrap();
            assert!(matches!(instance, Instance::Core(_)));
            instance.stop().await;

            // BroadcastAfterCore -> Core
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::BroadcastAfterCore { .. }));
            let instance = instance.transition(Mode::Core, handle).await.unwrap();
            assert!(matches!(instance, Instance::Core(_)));
            instance.stop().await;
        });
    }

    /// Check if the instance transitions to Edge mode correctly from all other
    /// modes.
    #[test]
    fn test_transition_to_edge() {
        let app = OverwatchRunner::<Services>::run(settings(), None).unwrap();
        app.runtime().handle().block_on(async {
            let handle = app.handle();

            // Core -> EdgeAfterCore
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::EdgeAfterCore { .. }));
            instance.stop().await;

            // Edge -> Edge
            let instance = TestInstance::new(Mode::Edge, handle).await.unwrap();
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::Edge(_)));
            instance.stop().await;

            // EdgeAfterCore -> Edge
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::EdgeAfterCore { .. }));
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::Edge(_)));
            instance.stop().await;

            // Broadcast -> Edge
            let instance = TestInstance::new(Mode::Broadcast, handle).await.unwrap();
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::Edge(_)));
            instance.stop().await;

            // BroadcastAfterCore -> Edge
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::BroadcastAfterCore { .. }));
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::Edge(_)));
            instance.stop().await;
        });
    }

    /// Check if the instance transitions to Broadcast mode correctly from all
    /// other modes.
    #[test]
    fn test_transition_to_broadcast() {
        let app = OverwatchRunner::<Services>::run(settings(), None).unwrap();
        app.runtime().handle().block_on(async {
            let handle = app.handle();

            // Core -> BroadcastAfterCore
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::BroadcastAfterCore { .. }));
            instance.stop().await;

            // Edge -> Broadcast
            let instance = TestInstance::new(Mode::Edge, handle).await.unwrap();
            let instance = instance.transition(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::Broadcast(_)));
            instance.stop().await;

            // EdgeAfterCore -> Broadcast
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::EdgeAfterCore { .. }));
            let instance = instance.transition(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::Broadcast(_)));
            instance.stop().await;

            // Broadcast -> Broadcast
            let instance = TestInstance::new(Mode::Broadcast, handle).await.unwrap();
            let instance = instance.transition(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::Broadcast(_)));
            instance.stop().await;

            // BroadcastAfterCore -> Broadcast
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::BroadcastAfterCore { .. }));
            let instance = instance.transition(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::Broadcast(_)));
            instance.stop().await;
        });
    }

    /// Check if the instance handles epoch events correctly.
    #[test]
    fn test_handle_epoch_event() {
        let app = OverwatchRunner::<Services>::run(settings(), None).unwrap();
        app.runtime().handle().block_on(async {
            let handle = app.handle();

            // Start with the Core instance.
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();

            // Core -> BroadcastAfterCore
            let minimal_network_size = NonZeroU64::MIN;
            let instance = instance
                .handle_epoch_event(
                    // With an empty membership smaller than the minimal size.
                    EpochEvent::NewEpoch(membership(&[], LOCAL_NODE_ID).into()),
                    handle,
                    minimal_network_size,
                )
                .await
                .unwrap();
            assert!(matches!(instance, Instance::BroadcastAfterCore { .. }));

            // BroadcastAfterCore -> Broadcast, after the transition period expires.
            let instance = instance
                .handle_epoch_event::<u8>(
                    EpochEvent::TransitionPeriodExpired,
                    handle,
                    minimal_network_size,
                )
                .await
                .unwrap();
            assert!(matches!(instance, Instance::Broadcast(_)));

            // Broadcast -> Edge
            let instance = instance
                .handle_epoch_event(
                    EpochEvent::NewEpoch(membership(&[1], LOCAL_NODE_ID).into()),
                    handle,
                    minimal_network_size,
                )
                .await
                .unwrap();
            assert!(matches!(instance, Instance::Edge(_)));

            // Edge -> Edge (stay)
            let instance = instance
                .handle_epoch_event(
                    EpochEvent::NewEpoch(membership(&[1], LOCAL_NODE_ID).into()),
                    handle,
                    minimal_network_size,
                )
                .await
                .unwrap();
            assert!(matches!(instance, Instance::Edge(_)));

            // Edge -> Core
            let instance = instance
                .handle_epoch_event(
                    EpochEvent::NewEpoch(membership(&[1], 1).into()),
                    handle,
                    minimal_network_size,
                )
                .await
                .unwrap();
            assert!(matches!(instance, Instance::Core(_)));
        });
    }

    type TestInstance = Instance<CoreService, EdgeService, BroadcastService, RuntimeServiceId>;

    #[overwatch::derive_services]
    struct Services {
        core: CoreService,
        edge: EdgeService,
        broadcast: BroadcastService,
    }

    struct CoreService {
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    }

    impl ServiceData for CoreService {
        type Settings = ();
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = ServiceMessage<u8>;
    }

    #[async_trait::async_trait]
    impl ServiceCore<RuntimeServiceId> for CoreService {
        fn init(
            service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
            _: Self::State,
        ) -> Result<Self, DynError> {
            Ok(Self {
                service_resources_handle,
            })
        }

        async fn run(self) -> Result<(), DynError> {
            let Self {
                service_resources_handle:
                    OpaqueServiceResourcesHandle::<Self, RuntimeServiceId> {
                        ref status_updater, ..
                    },
                ..
            } = self;
            status_updater.notify_ready();

            loop {
                sleep(Duration::from_secs(1)).await;
            }
        }
    }

    struct EdgeService {
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    }

    impl ServiceData for EdgeService {
        type Settings = ();
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = ServiceMessage<u8>;
    }

    #[async_trait::async_trait]
    impl ServiceCore<RuntimeServiceId> for EdgeService {
        fn init(
            service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
            _: Self::State,
        ) -> Result<Self, DynError> {
            Ok(Self {
                service_resources_handle,
            })
        }

        async fn run(self) -> Result<(), DynError> {
            let Self {
                service_resources_handle:
                    OpaqueServiceResourcesHandle::<Self, RuntimeServiceId> {
                        ref status_updater, ..
                    },
                ..
            } = self;
            status_updater.notify_ready();

            loop {
                sleep(Duration::from_secs(1)).await;
            }
        }
    }

    struct BroadcastService {
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    }

    impl ServiceData for BroadcastService {
        type Settings = ();
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = ServiceMessage<u8>;
    }

    #[async_trait::async_trait]
    impl ServiceCore<RuntimeServiceId> for BroadcastService {
        fn init(
            service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
            _: Self::State,
        ) -> Result<Self, DynError> {
            Ok(Self {
                service_resources_handle,
            })
        }

        async fn run(self) -> Result<(), DynError> {
            let Self {
                service_resources_handle:
                    OpaqueServiceResourcesHandle::<Self, RuntimeServiceId> {
                        ref status_updater, ..
                    },
                ..
            } = self;
            status_updater.notify_ready();

            loop {
                sleep(Duration::from_secs(1)).await;
            }
        }
    }

    fn settings() -> ServicesServiceSettings {
        ServicesServiceSettings {
            core: (),
            edge: (),
            broadcast: (),
        }
    }

    type NodeId = u8;

    fn membership(ids: &[NodeId], local_id: NodeId) -> Membership<NodeId> {
        let nodes = ids
            .iter()
            .copied()
            .map(|id| Node {
                id,
                address: Multiaddr::empty(),
                public_key: key(id).1,
            })
            .collect::<Vec<_>>();
        let local_public_key = key(local_id).1;
        Membership::new(&nodes, &local_public_key)
    }

    fn key(id: u8) -> (UnsecuredEd25519Key, Ed25519PublicKey) {
        let private_key = UnsecuredEd25519Key::from_bytes(&[id; 32]);
        let public_key = private_key.public_key();
        (private_key, public_key)
    }
}
