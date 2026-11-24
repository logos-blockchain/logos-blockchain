use std::{
    collections::{HashMap, hash_map::Entry},
    convert::Infallible,
    net::IpAddr,
    num::NonZeroU32,
    task::{Context, Poll},
};

use libp2p::{
    Multiaddr, PeerId,
    core::{Endpoint, transport::PortUse},
    multiaddr::Protocol,
    swarm::{
        ConnectionClosed, ConnectionDenied, ConnectionId, FromSwarm, ListenFailure,
        NetworkBehaviour, THandler, THandlerInEvent, THandlerOutEvent, ToSwarm, dummy,
    },
};
use thiserror::Error;

/// Refuses an incoming connection from an IP address that already has
/// `max_per_ip` incoming connections, in their handshake or established.
pub struct Behaviour {
    max_per_ip: NonZeroU32,
    /// The address each counted connection comes from, until it closes or
    /// fails.
    connection_ips: HashMap<ConnectionId, IpAddr>,
    /// How many counted connections each address has.
    connections_per_ip: HashMap<IpAddr, u32>,
}

#[derive(Debug, Error)]
#[error("at most {0} incoming connections are allowed from one IP address")]
struct LimitReached(NonZeroU32);

impl Behaviour {
    #[must_use]
    pub fn new(max_per_ip: NonZeroU32) -> Self {
        Self {
            max_per_ip,
            connection_ips: HashMap::new(),
            connections_per_ip: HashMap::new(),
        }
    }

    /// Stops counting `connection_id`, if it was counted.
    fn release(&mut self, connection_id: ConnectionId) {
        let Some(ip) = self.connection_ips.remove(&connection_id) else {
            return;
        };
        if let Entry::Occupied(mut count) = self.connections_per_ip.entry(ip) {
            *count.get_mut() -= 1;
            if *count.get() == 0 {
                count.remove();
            }
        }
    }
}

/// The IP address in `remote_addr`, unless it is a loopback one.
fn limited_ip(remote_addr: &Multiaddr) -> Option<IpAddr> {
    let ip = remote_addr.iter().find_map(|protocol| match protocol {
        Protocol::Ip4(addr) => Some(IpAddr::V4(addr)),
        Protocol::Ip6(addr) => Some(IpAddr::V6(addr)),
        _ => None,
    })?;
    (!ip.is_loopback()).then_some(ip)
}

impl NetworkBehaviour for Behaviour {
    type ConnectionHandler = dummy::ConnectionHandler;
    type ToSwarm = Infallible;

    fn handle_pending_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        _local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<(), ConnectionDenied> {
        let Some(ip) = limited_ip(remote_addr) else {
            return Ok(());
        };
        let count = self.connections_per_ip.entry(ip).or_default();
        if *count >= self.max_per_ip.get() {
            return Err(ConnectionDenied::new(LimitReached(self.max_per_ip)));
        }
        *count += 1;
        self.connection_ips.insert(connection_id, ip);
        Ok(())
    }

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _peer: PeerId,
        _local_addr: &Multiaddr,
        _remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(dummy::ConnectionHandler)
    }

    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _peer: PeerId,
        _addr: &Multiaddr,
        _role_override: Endpoint,
        _port_use: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(dummy::ConnectionHandler)
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        // A connection the swarm refused or failed to establish ends in a
        // listen failure, and an established one in its closing.
        if let FromSwarm::ListenFailure(ListenFailure { connection_id, .. })
        | FromSwarm::ConnectionClosed(ConnectionClosed { connection_id, .. }) = event
        {
            self.release(connection_id);
        }
    }

    fn on_connection_handler_event(
        &mut self,
        _peer_id: PeerId,
        _connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {}
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use libp2p::{core::ConnectedPoint, swarm::ListenError};

    use super::*;

    const LOCAL_ADDR: &str = "/ip4/0.0.0.0/udp/3000/quic-v1";

    fn remote_addr(ip: &str) -> Multiaddr {
        format!("/ip4/{ip}/udp/4000/quic-v1").parse().unwrap()
    }

    fn accepts(behaviour: &mut Behaviour, connection_id: usize, ip: &str) -> bool {
        behaviour
            .handle_pending_inbound_connection(
                ConnectionId::new_unchecked(connection_id),
                &LOCAL_ADDR.parse().unwrap(),
                &remote_addr(ip),
            )
            .is_ok()
    }

    fn fail(behaviour: &mut Behaviour, connection_id: usize, ip: &str) {
        behaviour.on_swarm_event(FromSwarm::ListenFailure(ListenFailure {
            local_addr: &LOCAL_ADDR.parse().unwrap(),
            send_back_addr: &remote_addr(ip),
            error: &ListenError::Aborted,
            connection_id: ConnectionId::new_unchecked(connection_id),
            peer_id: None,
        }));
    }

    fn close(behaviour: &mut Behaviour, connection_id: usize, ip: &str) {
        behaviour.on_swarm_event(FromSwarm::ConnectionClosed(ConnectionClosed {
            peer_id: PeerId::random(),
            connection_id: ConnectionId::new_unchecked(connection_id),
            endpoint: &ConnectedPoint::Listener {
                local_addr: LOCAL_ADDR.parse().unwrap(),
                send_back_addr: remote_addr(ip),
            },
            cause: None,
            remaining_established: 0,
        }));
    }

    #[test]
    fn refuses_an_address_at_its_limit_only() {
        let mut behaviour = Behaviour::new(2.try_into().unwrap());

        assert!(accepts(&mut behaviour, 0, "1.2.3.4"));
        assert!(accepts(&mut behaviour, 1, "1.2.3.4"));
        assert!(!accepts(&mut behaviour, 2, "1.2.3.4"));
        assert!(accepts(&mut behaviour, 3, "5.6.7.8"));
    }

    #[test]
    fn frees_a_slot_when_a_counted_connection_ends() {
        let mut behaviour = Behaviour::new(1.try_into().unwrap());
        assert!(accepts(&mut behaviour, 0, "1.2.3.4"));
        assert!(!accepts(&mut behaviour, 1, "1.2.3.4"));

        // The refused connection was never counted, so its failure frees
        // nothing.
        fail(&mut behaviour, 1, "1.2.3.4");
        assert!(!accepts(&mut behaviour, 2, "1.2.3.4"));

        close(&mut behaviour, 0, "1.2.3.4");
        assert!(accepts(&mut behaviour, 3, "1.2.3.4"));

        fail(&mut behaviour, 3, "1.2.3.4");
        assert!(accepts(&mut behaviour, 4, "1.2.3.4"));
        assert_eq!(behaviour.connection_ips.len(), 1);
        assert_eq!(behaviour.connections_per_ip.len(), 1);

        close(&mut behaviour, 4, "1.2.3.4");
        assert!(behaviour.connection_ips.is_empty());
        assert!(behaviour.connections_per_ip.is_empty());
    }

    #[test]
    fn does_not_limit_loopback_addresses() {
        let mut behaviour = Behaviour::new(1.try_into().unwrap());

        assert!(accepts(&mut behaviour, 0, "127.0.0.1"));
        assert!(accepts(&mut behaviour, 1, "127.0.0.1"));
        assert!(accepts(&mut behaviour, 2, "127.0.0.2"));
    }
}
