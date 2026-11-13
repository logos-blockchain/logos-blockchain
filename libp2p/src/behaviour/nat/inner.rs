use std::{
    net::Ipv4Addr,
    task::{Context, Poll},
    time::Duration,
};

use either::Either;
use futures::{
    FutureExt as _,
    future::{BoxFuture, Fuse, OptionFuture},
};
use lb_log_targets::libp2p as lb_log_targets_libp2p;
use libp2p::{
    Multiaddr, PeerId, autonat,
    core::{Endpoint, transport::PortUse},
    multiaddr::Protocol,
    swarm::{
        ConnectionDenied, ConnectionId, ExpiredListenAddr, FromSwarm, NetworkBehaviour,
        NewListenAddr, THandler, THandlerInEvent, THandlerOutEvent, ToSwarm,
    },
};
use rand::RngCore;
use tokio::sync::mpsc::UnboundedReceiver;

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

const LOG_TARGET: &str = lb_log_targets_libp2p::behaviour::nat::INNER;

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
    /// The swarm's listen addresses, in the order the swarm reported them.
    listen_addresses: Vec<Multiaddr>,
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
            listen_addresses: Vec::new(),
        }
    }
}

impl<Rng, Mapper, Detector> InnerNatBehaviour<Rng, Mapper, Detector>
where
    Rng: RngCore + 'static,
    Detector: GatewayDetector,
{
    /// The first listen address on the network interface that holds the
    /// default route, the only kind of address that the default gateway can
    /// map. With an unspecified listen IP, the swarm reports one listen address
    /// per interface, usually loopback first.
    fn gateway_facing_listen_address(&self) -> Option<&Multiaddr> {
        let interface_addresses = Detector::default_interface_addresses()
            .inspect_err(|e| {
                tracing::warn!(target: LOG_TARGET, "Failed to find the default interface: {e}");
            })
            .ok()?;
        first_address_on(&self.listen_addresses, &interface_addresses)
    }
}

/// The first of `listen_addresses` whose IP is one of `interface_addresses`.
fn first_address_on<'listen>(
    listen_addresses: &'listen [Multiaddr],
    interface_addresses: &[Ipv4Addr],
) -> Option<&'listen Multiaddr> {
    listen_addresses.iter().find(|address| {
        matches!(address.iter().next(), Some(Protocol::Ip4(ip)) if interface_addresses.contains(&ip))
    })
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
        match event {
            FromSwarm::NewListenAddr(NewListenAddr { addr, .. }) => {
                self.listen_addresses.push(addr.clone());
                // The state machine only tracks the address it can map.
                if self.gateway_facing_listen_address() == Some(addr) {
                    self.state_machine.on_event(event);
                }
            }
            FromSwarm::ExpiredListenAddr(ExpiredListenAddr { addr, .. }) => {
                self.listen_addresses
                    .retain(|listen_address| listen_address != addr);
            }
            _ => self.state_machine.on_event(event),
        }

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
            // Forward all autonat results to the state machine unconditionally.
            // The state machine decides which tested addresses matter in its
            // current state.
            if let ToSwarm::GenerateEvent(event) = &to_swarm {
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
                    tracing::info!(
                        target: LOG_TARGET,
                        "Gateway changed from {old_gateway:?} to {new_gateway:?}, triggering address re-mapping",
                    );

                    let local_address = self.gateway_facing_listen_address().cloned();
                    self.state_machine
                        .on_event(&address_mapper::Event::DefaultGatewayChanged {
                            old_gateway,
                            new_gateway,
                            local_address,
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
                        tracing::error!(target: LOG_TARGET, "Failed to start address mapping: {e}");
                    }
                }
                Command::NewExternalAddrCandidate(addr) => {
                    return Poll::Ready(ToSwarm::NewExternalAddrCandidate(addr));
                }
                Command::ExternalAddrExpired(addr) => {
                    return Poll::Ready(ToSwarm::ExternalAddrExpired(addr));
                }
            }
        }

        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_listen_address_on_the_default_interface_is_selected() {
        // What the swarm reports for an unspecified listen IP: loopback first,
        // then a bridge, then the interface that holds the default route.
        let listen_addresses: Vec<Multiaddr> = [
            "/ip4/127.0.0.1/udp/3000/quic-v1",
            "/ip4/172.17.0.1/udp/3000/quic-v1",
            "/ip4/192.168.1.10/udp/3000/quic-v1",
        ]
        .map(|address| address.parse().unwrap())
        .into();

        assert_eq!(
            first_address_on(&listen_addresses, &[Ipv4Addr::new(192, 168, 1, 10)]),
            Some(&listen_addresses[2])
        );
        assert_eq!(first_address_on(&listen_addresses, &[]), None);
    }
}
