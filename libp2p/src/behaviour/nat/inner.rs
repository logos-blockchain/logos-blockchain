use std::{
    task::{Context, Poll},
    time::Duration,
};

use either::Either;
use futures::{
    FutureExt as _,
    future::{BoxFuture, Fuse, OptionFuture},
};
use libp2p::{
    Multiaddr, PeerId, autonat,
    core::{Endpoint, transport::PortUse},
    swarm::{
        ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
        THandlerOutEvent, ToSwarm,
    },
};
use rand::RngCore;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{error, info};

use crate::{
    behaviour::nat::{
        address_mapper,
        address_mapper::{AddressMapperBehaviour, NatMapper, protocols::ProtocolManager},
        gateway_monitor::{
            GatewayDetector, GatewayMonitor, GatewayMonitorEvent, SystemGatewayDetector,
        },
        state_machine::{Command, StateMachine},
    },
    config::TraversalSettings,
};

type Task = BoxFuture<'static, Multiaddr>;

pub struct InnerNatBehaviour<Rng, Mapper, Detector>
where
    Rng: RngCore + 'static,
{
    /// `AutoNAT` client behaviour which is used to confirm if addresses of our
    /// node are indeed publicly reachable.
    autonat_client_behaviour: autonat::v2::client::Behaviour<Rng>,
    /// The address mapper behaviour is used to map the node's addresses at the
    /// default gateway using one of the protocols: `PCP`, `NAT-PMP`,
    /// `UPNP-IGD`.
    address_mapper_behaviour: AddressMapperBehaviour<Mapper>,
    /// Gateway monitor that periodically checks for gateway address changes
    /// and triggers re-mapping when the gateway changes.
    gateway_monitor: GatewayMonitor<Detector>,
    /// The state machine reacts to events from the swarm and from the
    /// sub-behaviours of the `InnerNatBehaviour` and issues commands to the
    /// `InnerNatBehaviour`.
    state_machine: StateMachine,
    /// Commands issued by the state machine are received through this end of
    /// the channel.
    command_rx: UnboundedReceiver<Command>,
    /// Used to schedule "re-tests" for already confirmed external addresses via
    /// the `autonat_client_behaviour`. Unused outside of the states that
    /// require periodic maintenance.
    next_autonat_client_tick: Fuse<OptionFuture<Task>>,
    /// Interval for the above ticker.
    autonat_client_tick_interval: Duration,
    /// Current local address that is being managed
    local_address: Option<Multiaddr>,
}

pub type NatBehaviour<Rng> = InnerNatBehaviour<Rng, ProtocolManager, SystemGatewayDetector>;

impl<Rng: RngCore + 'static> NatBehaviour<Rng> {
    pub fn new(rng: Rng, settings: &TraversalSettings) -> Self {
        let address_mapper_behaviour =
            AddressMapperBehaviour::<ProtocolManager>::new(settings.mapping);

        let gateway_monitor =
            GatewayMonitor::<SystemGatewayDetector>::new(settings.gateway_monitor);

        Self::create(rng, settings, address_mapper_behaviour, gateway_monitor)
    }
}

impl<Rng, Mapper, Detector> InnerNatBehaviour<Rng, Mapper, Detector>
where
    Rng: RngCore + 'static,
{
    fn create(
        rng: Rng,
        settings: &TraversalSettings,
        address_mapper_behaviour: AddressMapperBehaviour<Mapper>,
        gateway_monitor: GatewayMonitor<Detector>,
    ) -> Self {
        let autonat_config = settings.autonat.to_libp2p_config();
        let autonat_client_tick_interval = settings
            .autonat
            .retest_successful_external_addresses_interval;

        let autonat_client_behaviour = autonat::v2::client::Behaviour::new(rng, autonat_config);

        let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
        let state_machine = StateMachine::new(command_tx);

        Self {
            autonat_client_behaviour,
            address_mapper_behaviour,
            gateway_monitor,
            state_machine,
            command_rx,
            next_autonat_client_tick: OptionFuture::default().fuse(),
            autonat_client_tick_interval,
            local_address: None,
        }
    }
}

impl<Rng, Mapper, Detector> NetworkBehaviour for InnerNatBehaviour<Rng, Mapper, Detector>
where
    Rng: RngCore + 'static,
    Mapper: NatMapper + 'static,
    Detector: GatewayDetector + 'static,
{
    type ConnectionHandler =
        <autonat::v2::client::Behaviour as NetworkBehaviour>::ConnectionHandler;

    type ToSwarm = Either<
        <autonat::v2::client::Behaviour as NetworkBehaviour>::ToSwarm,
        address_mapper::Event,
    >;

    fn handle_pending_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<(), ConnectionDenied> {
        self.autonat_client_behaviour
            .handle_pending_inbound_connection(connection_id, local_addr, remote_addr)
    }

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.autonat_client_behaviour
            .handle_established_inbound_connection(connection_id, peer, local_addr, remote_addr)
    }

    fn handle_pending_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        maybe_peer: Option<PeerId>,
        addresses: &[Multiaddr],
        effective_role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        self.autonat_client_behaviour
            .handle_pending_outbound_connection(
                connection_id,
                maybe_peer,
                addresses,
                effective_role,
            )
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role_override: Endpoint,
        port_use: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.autonat_client_behaviour
            .handle_established_outbound_connection(
                connection_id,
                peer,
                addr,
                role_override,
                port_use,
            )
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        if let FromSwarm::NewListenAddr(addr_event) = &event {
            self.local_address = Some(addr_event.addr.clone());
        }

        self.state_machine.on_event(event);
        self.autonat_client_behaviour.on_swarm_event(event);
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        self.autonat_client_behaviour
            .on_connection_handler_event(peer_id, connection_id, event);
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        if let Poll::Ready(to_swarm) = self.autonat_client_behaviour.poll(cx) {
            if let ToSwarm::GenerateEvent(event) = &to_swarm
                && let Some(ref local_addr) = self.local_address
                && &event.tested_addr == local_addr
            {
                self.state_machine.on_event(event);
            }

            return Poll::Ready(to_swarm.map_out(Either::Left));
        }

        if let Poll::Ready(to_swarm) = self.address_mapper_behaviour.poll(cx) {
            if let ToSwarm::GenerateEvent(event) = &to_swarm {
                self.state_machine.on_event(event);
            }

            return Poll::Ready(to_swarm.map_out(Either::Right).map_in(Either::Right));
        }

        if let Poll::Ready(Some(_addr)) = self.next_autonat_client_tick.poll_unpin(cx) {
            // TODO: This is a placeholder for the missing API of the
            // autonat client
            // self.autonat_client_behaviour.retest_address(addr);
        }

        if let Poll::Ready(Some(event)) = self.gateway_monitor.poll(cx) {
            match event {
                GatewayMonitorEvent::GatewayChanged {
                    old_gateway,
                    new_gateway,
                } => {
                    info!(
                        "Gateway changed from {old_gateway:?} to {new_gateway:?}, triggering address re-mapping",
                    );

                    self.state_machine
                        .on_event(&address_mapper::Event::DefaultGatewayChanged {
                            old_gateway,
                            new_gateway,
                            local_address: self.local_address.clone(),
                        });
                }
            }
        }

        if let Poll::Ready(Some(command)) = self.command_rx.poll_recv(cx) {
            match command {
                Command::ScheduleAutonatClientTest(addr) => {
                    self.next_autonat_client_tick = OptionFuture::from(Some(
                        tokio::time::sleep(self.autonat_client_tick_interval)
                            .map(|()| addr)
                            .boxed(),
                    ))
                    .fuse();
                }
                Command::MapAddress(addr) => {
                    if let Err(e) = self.address_mapper_behaviour.try_map_address(addr) {
                        error!("Failed to start address mapping: {e}");
                    }
                }
                Command::NewExternalAddrCandidate(addr) => {
                    return Poll::Ready(ToSwarm::NewExternalAddrCandidate(addr));
                }
            }
        }

        Poll::Pending
    }
}
