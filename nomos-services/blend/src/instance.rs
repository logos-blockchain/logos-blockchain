use std::{
    fmt::{Debug, Display},
    hash::Hash,
};

use nomos_blend::scheduling::{membership::Membership, session::SessionEvent};
use nomos_network::NetworkService;
use overwatch::{
    overwatch::OverwatchHandle,
    services::{AsServiceId, ServiceData},
};

use crate::{
    BroadcastSettings,
    core::{
        network::NetworkAdapter as NetworkAdapterTrait,
        service_components::{
            MessageComponents, NetworkBackendOfService, ServiceComponents as CoreServiceComponents,
        },
    },
    membership::MembershipInfo,
    modes::{self, BroadcastMode, CoreMode, EdgeMode},
};

/// An instance that can operate in Core, Edge, or Broadcast mode,
/// and can transition between them.
pub enum Instance<CoreService, EdgeService, RuntimeServiceId>
where
    CoreService: ServiceData + CoreServiceComponents<RuntimeServiceId>,
    EdgeService: ServiceData,
{
    Core(CoreMode<CoreService, RuntimeServiceId>),
    Edge(EdgeMode<EdgeService, RuntimeServiceId>),
    EdgeAfterCore {
        mode: EdgeMode<EdgeService, RuntimeServiceId>,
        // Keep the previous core mode for the session transition period.
        prev: CoreMode<CoreService, RuntimeServiceId>,
    },
    Broadcast(BroadcastMode<CoreService::NetworkAdapter, RuntimeServiceId>),
    BroadcastAfterCore {
        mode: BroadcastMode<CoreService::NetworkAdapter, RuntimeServiceId>,
        // Keep the previous core mode for the session transition period.
        prev: CoreMode<CoreService, RuntimeServiceId>,
    },
}

impl<CoreService, EdgeService, RuntimeServiceId>
    Instance<CoreService, EdgeService, RuntimeServiceId>
where
    CoreService: ServiceData<
            Message: MessageComponents<
                Payload: Into<Vec<u8>>,
                BroadcastSettings: Into<BroadcastSettings<CoreService>>,
            > + Send
                         + Sync
                         + 'static,
        > + CoreServiceComponents<
            RuntimeServiceId,
            NetworkAdapter: NetworkAdapterTrait<
                RuntimeServiceId,
                BroadcastSettings = BroadcastSettings<CoreService>,
            > + Send
                                + Sync
                                + 'static,
            NodeId: Eq + Hash,
        > + 'static,
    EdgeService: ServiceData<Message = CoreService::Message> + 'static,
    RuntimeServiceId: AsServiceId<CoreService>
        + AsServiceId<EdgeService>
        + AsServiceId<
            NetworkService<
                NetworkBackendOfService<CoreService, RuntimeServiceId>,
                RuntimeServiceId,
            >,
        > + Debug
        + Display
        + Clone
        + Send
        + Sync
        + 'static,
{
    /// Initializes a new instance in the specified mode.
    pub async fn new(
        mode: Mode,
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Result<Self, modes::Error> {
        match mode {
            Mode::Core => Ok(Self::Core(Self::new_core_mode(overwatch_handle).await?)),
            Mode::Edge => Ok(Self::Edge(Self::new_edge_mode(overwatch_handle).await?)),
            Mode::Broadcast => Ok(Self::Broadcast(
                Self::new_broadcast_mode(overwatch_handle).await?,
            )),
        }
    }

    async fn new_core_mode(
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Result<CoreMode<CoreService, RuntimeServiceId>, modes::Error> {
        CoreMode::new(overwatch_handle.clone()).await
    }

    async fn new_edge_mode(
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Result<EdgeMode<EdgeService, RuntimeServiceId>, modes::Error> {
        EdgeMode::new(overwatch_handle.clone()).await
    }

    async fn new_broadcast_mode(
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Result<BroadcastMode<CoreService::NetworkAdapter, RuntimeServiceId>, modes::Error> {
        BroadcastMode::new::<
            NetworkService<
                NetworkBackendOfService<CoreService, RuntimeServiceId>,
                RuntimeServiceId,
            >,
        >(overwatch_handle)
        .await
    }

    /// Handles an inbound message by delegating it to the current mode.
    pub async fn handle_inbound_message(
        &self,
        message: CoreService::Message,
    ) -> Result<(), modes::Error> {
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

    /// Handles a session event, potentially causing a mode transition.
    pub async fn handle_session_event(
        self,
        event: SessionEvent<MembershipInfo<CoreService::NodeId>>,
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
        minimal_network_size: usize,
    ) -> Result<Self, modes::Error> {
        match event {
            SessionEvent::NewSession(MembershipInfo { membership, .. }) => {
                self.transition(
                    Mode::choose(&membership, minimal_network_size),
                    overwatch_handle,
                )
                .await
            }
            SessionEvent::TransitionPeriodExpired => {
                Ok(self.handle_transition_period_expired().await)
            }
        }
    }

    /// Transitions the instance to the specified mode.
    async fn transition(
        self,
        to_mode: Mode,
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Result<Self, modes::Error> {
        match to_mode {
            Mode::Core => self.transition_to_core(overwatch_handle).await,
            Mode::Edge => self.transition_to_edge(overwatch_handle).await,
            Mode::Broadcast => self.transition_to_broadcast(overwatch_handle).await,
        }
    }

    /// Transitions to Core mode.
    ///
    /// If the current mode is Core, it stays in Core mode.
    /// Otherwise, it shuts down the current mode and starts a new Core mode.
    async fn transition_to_core(
        self,
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Result<Self, modes::Error> {
        match self {
            Self::Core(mode) => Ok(Self::Core(mode)),
            mode => {
                mode.wait_until_stopped_or_kill().await;
                Ok(Self::Core(Self::new_core_mode(overwatch_handle).await?))
            }
        }
    }

    /// Transitions to Edge mode.
    ///
    /// If the current mode is Edge, it stays in Edge mode.
    /// Otherwise, it shuts down the current mode and starts a new Edge mode.
    async fn transition_to_edge(
        self,
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Result<Self, modes::Error> {
        match self {
            Self::Core(mode) => Ok(Self::EdgeAfterCore {
                mode: Self::new_edge_mode(overwatch_handle).await?,
                prev: mode,
            }),
            Self::Edge(mode) => Ok(Self::Edge(mode)),
            Self::EdgeAfterCore { mode, prev } => {
                prev.wait_until_stopped_or_kill().await;
                Ok(Self::Edge(mode))
            }
            mode => {
                mode.wait_until_stopped_or_kill().await;
                Ok(Self::Edge(Self::new_edge_mode(overwatch_handle).await?))
            }
        }
    }

    /// Transitions to Broadcast mode.
    ///
    /// If the current mode is Broadcast, it stays in Broadcast mode.
    /// Otherwise, it shuts down the current mode and starts a new Broadcast
    /// mode.
    async fn transition_to_broadcast(
        self,
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Result<Self, modes::Error> {
        match self {
            Self::Core(mode) => Ok(Self::BroadcastAfterCore {
                mode: Self::new_broadcast_mode(overwatch_handle).await?,
                prev: mode,
            }),
            Self::Broadcast(mode) => Ok(Self::Broadcast(mode)),
            Self::BroadcastAfterCore { mode, prev } => {
                prev.wait_until_stopped_or_kill().await;
                Ok(Self::Broadcast(mode))
            }
            mode => {
                mode.wait_until_stopped_or_kill().await;
                Ok(Self::Broadcast(
                    Self::new_broadcast_mode(overwatch_handle).await?,
                ))
            }
        }
    }

    /// Handles the expiration of the transition period,
    ///
    /// If the instance is transitioning from Core to other modes,
    /// the previous Core mode must be stopped by itself after completing the
    /// transition. If not, it is forcefully killed.
    async fn handle_transition_period_expired(self) -> Self {
        match self {
            Self::EdgeAfterCore { mode, prev } => {
                prev.wait_until_stopped_or_kill().await;
                Self::Edge(mode)
            }
            Self::BroadcastAfterCore { mode, prev } => {
                prev.wait_until_stopped_or_kill().await;
                Self::Broadcast(mode)
            }
            _ => self,
        }
    }

    /// Wait until the underlying mode is stopped itself.
    /// If it does not stop in time specified by each mode, forcefully kill it.
    async fn wait_until_stopped_or_kill(self) {
        match self {
            Self::Core(mode) => mode.wait_until_stopped_or_kill().await,
            Self::Edge(mode) => mode.wait_until_stopped_or_kill().await,
            Self::EdgeAfterCore { mode, prev } => {
                mode.wait_until_stopped_or_kill().await;
                prev.wait_until_stopped_or_kill().await;
            }
            Self::Broadcast(_) => {
                // No-op
            }
            Self::BroadcastAfterCore { prev, .. } => {
                prev.wait_until_stopped_or_kill().await;
            }
        }
    }
}

#[derive(Debug)]
pub enum Mode {
    Core,
    Edge,
    Broadcast,
}

impl Mode {
    pub fn choose<NodeId>(membership: &Membership<NodeId>, minimal_network_size: usize) -> Self
    where
        NodeId: Eq + Hash,
    {
        if membership.size() < minimal_network_size {
            Self::Broadcast
        } else if membership.contains_local() {
            Self::Core
        } else {
            Self::Edge
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use key_management_system_service::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
    use libp2p::Multiaddr;
    use nomos_blend::scheduling::membership::Node;
    use nomos_network::config::NetworkConfig;
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
    use crate::modes::broadcast_tests::{TestMessage, TestNetworkAdapter, TestNetworkBackend};

    /// Check if the instance is initialized successfully for each mode.
    #[test]
    fn test_new() {
        let app = OverwatchRunner::<Services>::run(settings(), None).unwrap();
        app.runtime().handle().block_on(async {
            let handle = app.handle();

            // Start only the network service first.
            // Other services will be started when the instance is created.
            start_network_service(handle).await;

            // Check if the Core instance is created successfully.
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            assert!(matches!(instance, Instance::Core(_)));
            instance.wait_until_stopped_or_kill().await;

            // Check if the Edge instance is created successfully.
            let instance = TestInstance::new(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::Edge(_)));
            instance.wait_until_stopped_or_kill().await;

            // Check if the Broadcast instance is created successfully.
            let instance = TestInstance::new(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::Broadcast(_)));
            instance.wait_until_stopped_or_kill().await;
        });
    }

    /// Check if the instance transitions to Core mode correctly from all other
    /// modes.
    #[test]
    fn test_transition_to_core() {
        let app = OverwatchRunner::<Services>::run(settings(), None).unwrap();
        app.runtime().handle().block_on(async {
            let handle = app.handle();

            // Start only the network service first.
            // Other services will be started when the instance is created.
            start_network_service(handle).await;

            // Core -> Core
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Core, handle).await.unwrap();
            assert!(matches!(instance, Instance::Core(_)));
            instance.wait_until_stopped_or_kill().await;

            // Edge -> Core
            let instance = TestInstance::new(Mode::Edge, handle).await.unwrap();
            let instance = instance.transition(Mode::Core, handle).await.unwrap();
            assert!(matches!(instance, Instance::Core(_)));
            instance.wait_until_stopped_or_kill().await;

            // EdgeAfterCore -> Core
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::EdgeAfterCore { .. }));
            let instance = instance.transition(Mode::Core, handle).await.unwrap();
            assert!(matches!(instance, Instance::Core(_)));
            instance.wait_until_stopped_or_kill().await;

            // Broadcast -> Core
            let instance = TestInstance::new(Mode::Broadcast, handle).await.unwrap();
            let instance = instance.transition(Mode::Core, handle).await.unwrap();
            assert!(matches!(instance, Instance::Core(_)));
            instance.wait_until_stopped_or_kill().await;

            // BroadcastAfterCore -> Core
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::BroadcastAfterCore { .. }));
            let instance = instance.transition(Mode::Core, handle).await.unwrap();
            assert!(matches!(instance, Instance::Core(_)));
            instance.wait_until_stopped_or_kill().await;
        });
    }

    /// Check if the instance transitions to Edge mode correctly from all other
    /// modes.
    #[test]
    fn test_transition_to_edge() {
        let app = OverwatchRunner::<Services>::run(settings(), None).unwrap();
        app.runtime().handle().block_on(async {
            let handle = app.handle();

            // Start only the network service first.
            // Other services will be started when the instance is created.
            start_network_service(handle).await;

            // Core -> EdgeAfterCore
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::EdgeAfterCore { .. }));
            instance.wait_until_stopped_or_kill().await;

            // Edge -> Edge
            let instance = TestInstance::new(Mode::Edge, handle).await.unwrap();
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::Edge(_)));
            instance.wait_until_stopped_or_kill().await;

            // EdgeAfterCore -> Edge
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::EdgeAfterCore { .. }));
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::Edge(_)));
            instance.wait_until_stopped_or_kill().await;

            // Broadcast -> Edge
            let instance = TestInstance::new(Mode::Broadcast, handle).await.unwrap();
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::Edge(_)));
            instance.wait_until_stopped_or_kill().await;

            // BroadcastAfterCore -> Edge
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::BroadcastAfterCore { .. }));
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::Edge(_)));
            instance.wait_until_stopped_or_kill().await;
        });
    }

    /// Check if the instance transitions to Broadcast mode correctly from all
    /// other modes.
    #[test]
    fn test_transition_to_broadcast() {
        let app = OverwatchRunner::<Services>::run(settings(), None).unwrap();
        app.runtime().handle().block_on(async {
            let handle = app.handle();

            // Start only the network service first.
            // Other services will be started when the instance is created.
            start_network_service(handle).await;

            // Core -> BroadcastAfterCore
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::BroadcastAfterCore { .. }));
            instance.wait_until_stopped_or_kill().await;

            // Edge -> Broadcast
            let instance = TestInstance::new(Mode::Edge, handle).await.unwrap();
            let instance = instance.transition(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::Broadcast(_)));
            instance.wait_until_stopped_or_kill().await;

            // EdgeAfterCore -> Broadcast
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Edge, handle).await.unwrap();
            assert!(matches!(instance, Instance::EdgeAfterCore { .. }));
            let instance = instance.transition(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::Broadcast(_)));
            instance.wait_until_stopped_or_kill().await;

            // Broadcast -> Broadcast
            let instance = TestInstance::new(Mode::Broadcast, handle).await.unwrap();
            let instance = instance.transition(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::Broadcast(_)));
            instance.wait_until_stopped_or_kill().await;

            // BroadcastAfterCore -> Broadcast
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();
            let instance = instance.transition(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::BroadcastAfterCore { .. }));
            let instance = instance.transition(Mode::Broadcast, handle).await.unwrap();
            assert!(matches!(instance, Instance::Broadcast(_)));
            instance.wait_until_stopped_or_kill().await;
        });
    }

    /// Check if the instance handles session events correctly.
    #[test]
    fn test_handle_session_event() {
        let app = OverwatchRunner::<Services>::run(settings(), None).unwrap();
        app.runtime().handle().block_on(async {
            let handle = app.handle();

            // Start only the network service first.
            // Other services will be started when the instance is created.
            start_network_service(handle).await;

            // Start with the Core instance.
            let instance = TestInstance::new(Mode::Core, handle).await.unwrap();

            // Core -> BroadcastAfterCore
            let local_node = 99;
            let minimal_network_size = 1;
            let instance = instance
                .handle_session_event(
                    // With an empty membership smaller than the minimal size.
                    SessionEvent::NewSession(MembershipInfo::from_membership_and_session_number(
                        membership(&[], local_node),
                        1,
                    )),
                    handle,
                    minimal_network_size,
                )
                .await
                .unwrap();
            assert!(matches!(instance, Instance::BroadcastAfterCore { .. }));

            // BroadcastAfterCore -> Broadcast, after the transition period expires.
            let instance = instance
                .handle_session_event(
                    SessionEvent::TransitionPeriodExpired,
                    handle,
                    minimal_network_size,
                )
                .await
                .unwrap();
            assert!(matches!(instance, Instance::Broadcast(_)));

            // Broadcast -> Edge
            let instance = instance
                .handle_session_event(
                    SessionEvent::NewSession(MembershipInfo::from_membership_and_session_number(
                        membership(&[1], local_node),
                        1,
                    )),
                    handle,
                    minimal_network_size,
                )
                .await
                .unwrap();
            assert!(matches!(instance, Instance::Edge(_)));

            // Edge -> Edge (stay)
            let instance = instance
                .handle_session_event(
                    SessionEvent::NewSession(MembershipInfo::from_membership_and_session_number(
                        membership(&[1], local_node),
                        1,
                    )),
                    handle,
                    minimal_network_size,
                )
                .await
                .unwrap();
            assert!(matches!(instance, Instance::Edge(_)));

            // Edge -> Core
            let instance = instance
                .handle_session_event(
                    SessionEvent::NewSession(MembershipInfo::from_membership_and_session_number(
                        membership(&[1], 1),
                        1,
                    )),
                    handle,
                    minimal_network_size,
                )
                .await
                .unwrap();
            assert!(matches!(instance, Instance::Core(_)));
        });
    }

    type TestInstance = Instance<CoreService, EdgeService, RuntimeServiceId>;

    #[overwatch::derive_services]
    struct Services {
        core: CoreService,
        edge: EdgeService,
        network: NetworkService<TestNetworkBackend, RuntimeServiceId>,
    }

    async fn start_network_service(handle: &OverwatchHandle<RuntimeServiceId>) {
        handle
            .start_service::<NetworkService<TestNetworkBackend, RuntimeServiceId>>()
            .await
            .unwrap();
    }

    struct CoreService {
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    }

    impl ServiceData for CoreService {
        type Settings = ();
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = TestMessage;
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

    impl CoreServiceComponents<RuntimeServiceId> for CoreService {
        type NetworkAdapter = TestNetworkAdapter;
        type BackendSettings = ();
        type NodeId = u8;
        type Rng = ();
        type ProofsGenerator = ();
    }

    struct EdgeService {
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    }

    impl ServiceData for EdgeService {
        type Settings = ();
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = TestMessage;
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

    fn settings() -> ServicesServiceSettings {
        ServicesServiceSettings {
            core: (),
            edge: (),
            network: NetworkConfig { backend: () },
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
