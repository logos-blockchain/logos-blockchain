use core::{num::NonZeroU64, time::Duration};
use std::{
    fmt::{Debug, Display},
    hash::Hash,
};

use lb_blend::scheduling::epoch::EpochEvent;
use lb_log_targets::diagnostic::BLEND_REACHABILITY;
use lb_utils::tokio::task::{CancellableHandle, spawn};
use overwatch::{
    overwatch::OverwatchHandle,
    services::{AsServiceId, ServiceData},
};
use tracing::{debug, error, info};

use crate::{
    membership::MembershipInfo,
    mode::{Mode, ModeMembership},
    orchestrator::{self, LOG_TARGET, OnDemandServiceMode},
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
        prev: PreviousCore<CoreService, RuntimeServiceId>,
    },
    Broadcast(OnDemandServiceMode<BroadcastService, RuntimeServiceId>),
    BroadcastAfterCore {
        mode: OnDemandServiceMode<BroadcastService, RuntimeServiceId>,
        /// Kept for the epoch transition period.
        prev: PreviousCore<CoreService, RuntimeServiceId>,
    },
}

/// A core service on its way out, behind the mode that replaced it.
///
/// It stays known until it's gone: Overwatch takes the start of a service that
/// is still running for done, and the kill closing its grace period would then
/// hit the new core.
pub enum PreviousCore<CoreService, RuntimeServiceId>
where
    CoreService: ServiceData,
{
    /// In its epoch transition period.
    Draining(OnDemandServiceMode<CoreService, RuntimeServiceId>),
    /// Past its transition period, and shutting down on a task of its own so
    /// that the messages for the running mode don't wait for it.
    ShuttingDown(CancellableHandle<bool>),
}

/// How long a core service is given to finish draining before it is killed. It
/// gets the longest grace because it is the only mode with anything to wind
/// down.
const CORE_SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
/// The other two hold nothing that outlives them.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

impl<CoreService, RuntimeServiceId> PreviousCore<CoreService, RuntimeServiceId>
where
    CoreService: ServiceData<Message: Send + 'static> + 'static,
    RuntimeServiceId: AsServiceId<CoreService> + Debug + Display + Send + Sync + 'static,
{
    fn shutdown(self) -> Self {
        match self {
            Self::Draining(core) => Self::ShuttingDown(
                spawn(
                    "logos/blend/previous-core-shutdown",
                    core.wait_until_stopped_or_kill(CORE_SHUTDOWN_GRACE),
                )
                .into(),
            ),
            shutting_down @ Self::ShuttingDown(_) => shutting_down,
        }
    }

    async fn await_shutdown(self) {
        match self {
            Self::Draining(core) => {
                core.wait_until_stopped_or_kill(CORE_SHUTDOWN_GRACE).await;
            }
            Self::ShuttingDown(shutdown) => {
                if let Err(error) = shutdown.await {
                    error!(target: LOG_TARGET, "The previous core's shutdown failed: {error}");
                }
            }
        }
    }
}

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
    ) -> Result<Self, orchestrator::Error<CoreService::Message>> {
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
    ) -> Result<(), orchestrator::Error<CoreService::Message>> {
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
    ) -> Result<Self, orchestrator::Error<CoreService::Message>>
    where
        NodeId: Eq + Hash,
    {
        match event {
            EpochEvent::NewEpoch(membership_info) => {
                self.transition(
                    ModeMembership::resolve(membership_info, minimum_network_size).mode(),
                    overwatch_handle,
                )
                .await
            }
            EpochEvent::TransitionPeriodExpired => Ok(self.handle_transition_period_expired()),
        }
    }

    /// Switches to `to_mode`, keeping a draining core alongside if there is
    /// one.
    ///
    /// Three rules, and they are the whole transition matrix:
    /// - already in `to_mode`: stay, and let a core draining behind it go;
    /// - leaving core: start the new mode and keep the core draining;
    /// - anything else: stop what is running, then start the new mode.
    async fn transition(
        self,
        to_mode: Mode,
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Result<Self, orchestrator::Error<CoreService::Message>> {
        let previous_mode = self.mode();
        if previous_mode == to_mode {
            // Already serving this mode. A core still draining behind it is let
            // go of, just as the transition period expiring would, which is
            // the safety net for an expiry that never arrives. Its shutdown
            // runs on its own, so payloads don't wait for it.
            let next = self.handle_transition_period_expired();
            log_mode_applied(previous_mode, to_mode, next.mode());
            return Ok(next);
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
                prev: PreviousCore::Draining(prev),
            },
            (Mode::Broadcast, None) => {
                Self::Broadcast(OnDemandServiceMode::new(overwatch_handle.clone()).await?)
            }
            (Mode::Broadcast, Some(prev)) => Self::BroadcastAfterCore {
                mode: OnDemandServiceMode::new(overwatch_handle.clone()).await?,
                prev: PreviousCore::Draining(prev),
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
    fn handle_transition_period_expired(self) -> Self {
        match self {
            Self::EdgeAfterCore { mode, prev } => Self::EdgeAfterCore {
                mode,
                prev: prev.shutdown(),
            },
            Self::BroadcastAfterCore { mode, prev } => Self::BroadcastAfterCore {
                mode,
                prev: prev.shutdown(),
            },
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
                prev.await_shutdown().await;
            }
            Self::BroadcastAfterCore { mode, prev } => {
                mode.wait_until_stopped_or_kill(SHUTDOWN_GRACE).await;
                prev.await_shutdown().await;
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
    debug!(
        target: crate::LOG_TARGET,
        diagnostic = BLEND_REACHABILITY,
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
            diagnostic = BLEND_REACHABILITY,
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
        overwatch::{OverwatchRunner, Shutdown},
        services::{
            ServiceCore,
            resources::ServiceResourcesHandle,
            state::{NoOperator, NoState},
            status::ServiceStatus,
        },
    };
    use tokio::time::{Instant, sleep};

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

            // EdgeAfterCore -> EdgeAfterCore, letting the core go
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::EdgeAfterCore { .. }));
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            assert!(matches!(
                instance,
                Instance::EdgeAfterCore {
                    prev: PreviousCore::ShuttingDown(_),
                    ..
                }
            ));
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

            // BroadcastAfterCore -> BroadcastAfterCore, letting the core go
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::BroadcastAfterCore { .. }));
            let instance = instance.transition(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(
                instance,
                Instance::BroadcastAfterCore {
                    prev: PreviousCore::ShuttingDown(_),
                    ..
                }
            ));
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

            // The transition period expires: the core is let go of.
            let instance = instance
                .handle_epoch_event::<u8>(
                    EpochEvent::TransitionPeriodExpired,
                    handle,
                    minimal_network_size,
                )
                .await
                .unwrap();
            assert!(matches!(
                instance,
                Instance::BroadcastAfterCore {
                    prev: PreviousCore::ShuttingDown(_),
                    ..
                }
            ));

            // BroadcastAfterCore -> Edge
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

    /// A new epoch in the mode already running lets the core draining behind it
    /// go without waiting for it, as the end of its transition period would:
    /// the test core never stops by itself, so waiting would take all of its
    /// grace.
    #[test]
    fn a_new_epoch_in_the_same_mode_leaves_the_running_mode_free() {
        let app = OverwatchRunner::<Services>::run(settings(), None).unwrap();
        app.runtime().handle().block_on(async {
            let handle = app.handle();
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();

            let new_epoch = Instant::now();
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();

            assert!(new_epoch.elapsed() < CORE_SHUTDOWN_GRACE);
            instance.stop().await;
        });
    }

    /// The core takes all of its grace to go, as the test one never stops by
    /// itself, and the expiry of its transition period doesn't wait for that.
    #[test]
    fn the_end_of_a_transition_period_leaves_the_running_mode_free() {
        let app = OverwatchRunner::<Services>::run(settings(), None).unwrap();
        app.runtime().handle().block_on(async {
            let handle = app.handle();
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();

            let expiry = Instant::now();
            let instance = instance
                .handle_epoch_event::<u8>(
                    EpochEvent::TransitionPeriodExpired,
                    handle,
                    NonZeroU64::MIN,
                )
                .await
                .unwrap();

            assert!(expiry.elapsed() < CORE_SHUTDOWN_GRACE);
            instance.stop().await;
        });
    }

    /// A core started right after the previous one was let go of waits for it
    /// to be gone, and so survives the kill that closes its grace period.
    #[test]
    fn a_new_core_outlives_the_shutdown_of_the_previous_one() {
        let app = OverwatchRunner::<Services>::run(settings(), None).unwrap();
        app.runtime().handle().block_on(async {
            let handle = app.handle();
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            let instance = instance
                .handle_epoch_event::<u8>(
                    EpochEvent::TransitionPeriodExpired,
                    handle,
                    NonZeroU64::MIN,
                )
                .await
                .unwrap();

            let instance = instance.transition(Mode::Core, handle).await.unwrap();
            assert!(matches!(instance, Instance::Core(_)));

            sleep(CORE_SHUTDOWN_GRACE + Duration::from_millis(500)).await;
            let core = handle.status_watcher::<CoreService>().await.unwrap();
            assert_eq!(core.current(), ServiceStatus::Ready);
        });
    }

    type TestInstance = Instance<CoreService, EdgeService, BroadcastService, RuntimeServiceId>;

    #[overwatch::derive_services(panic_policy = Shutdown)]
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
                    ServiceResourcesHandle {
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
                    ServiceResourcesHandle {
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
                    ServiceResourcesHandle {
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
