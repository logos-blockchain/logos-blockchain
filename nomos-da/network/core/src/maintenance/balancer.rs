use std::{
    collections::VecDeque,
    convert::Infallible,
    fmt::Debug,
    pin::Pin,
    task::{Context, Poll, Waker},
};

use libp2p::{
    Multiaddr, PeerId,
    core::{
        ConnectedPoint::{Dialer, Listener},
        Endpoint,
        transport::PortUse,
    },
    swarm::{
        ConnectionClosed, ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour,
        THandlerInEvent, THandlerOutEvent, ToSwarm, dial_opts::DialOpts, dummy,
    },
};
use tokio::sync::{
    mpsc::{self, UnboundedReceiver, UnboundedSender},
    oneshot,
};

use crate::addressbook::AddressBookHandler;

pub enum ConnectionEvent {
    OpenInbound(PeerId),
    OpenOutbound(PeerId),
    CloseInbound(PeerId),
    CloseOutbound(PeerId),
}

pub trait ConnectionBalancer {
    type Stats;

    fn record_event(&mut self, event: ConnectionEvent);
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<VecDeque<PeerId>>;
    fn stats(&self) -> Self::Stats;
    fn refresh_subnet_attributions(&mut self);
}

#[derive(Debug)]
pub enum ConnectionBalancerCommand<Stats> {
    Stats(oneshot::Sender<Stats>),
}

pub struct ConnectionBalancerBehaviour<Balancer, Addressbook>
where
    Balancer: ConnectionBalancer,
    Addressbook: AddressBookHandler,
{
    addressbook: Addressbook,
    balancer: Balancer,
    peers_to_dial: VecDeque<PeerId>,
    command_sender:
        UnboundedSender<ConnectionBalancerCommand<<Balancer as ConnectionBalancer>::Stats>>,
    command_receiver:
        UnboundedReceiver<ConnectionBalancerCommand<<Balancer as ConnectionBalancer>::Stats>>,
    stats_sender: Option<UnboundedSender<<Balancer as ConnectionBalancer>::Stats>>,
    subnets_refresh_signal: Pin<Box<dyn futures::Stream<Item = ()> + Send>>,
    waker: Option<Waker>,
}

impl<Balancer, Addressbook> ConnectionBalancerBehaviour<Balancer, Addressbook>
where
    Balancer: ConnectionBalancer,
    Addressbook: AddressBookHandler,
{
    pub fn new(
        addressbook: Addressbook,
        balancer: Balancer,
        stats_sender: Option<UnboundedSender<<Balancer as ConnectionBalancer>::Stats>>,
        subnets_refresh_signal: impl futures::Stream<Item = ()> + Send + 'static,
    ) -> Self {
        let (command_sender, command_receiver) = mpsc::unbounded_channel();

        Self {
            addressbook,
            balancer,
            peers_to_dial: VecDeque::new(),
            command_sender,
            command_receiver,
            stats_sender,
            waker: None,
            subnets_refresh_signal: Box::pin(subnets_refresh_signal),
        }
    }

    fn record_event(&mut self, event: ConnectionEvent) {
        self.balancer.record_event(event);
        if let Some(stats_sender) = &self.stats_sender
            && let Err(err) = stats_sender.send(self.balancer.stats())
        {
            tracing::error!("Error while sending response to a channel: {err:?}");
        }
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }

    pub fn command_channel(
        &self,
    ) -> UnboundedSender<ConnectionBalancerCommand<<Balancer as ConnectionBalancer>::Stats>> {
        self.command_sender.clone()
    }
}

impl<Balancer, Addressbook> NetworkBehaviour for ConnectionBalancerBehaviour<Balancer, Addressbook>
where
    Balancer: ConnectionBalancer + 'static,
    Balancer::Stats: Debug,
    Addressbook: AddressBookHandler<Id = PeerId> + 'static,
{
    type ConnectionHandler = dummy::ConnectionHandler;
    type ToSwarm = Infallible;

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        peer: PeerId,
        _: &Multiaddr,
        _: &Multiaddr,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        self.record_event(ConnectionEvent::OpenInbound(peer));
        Ok(dummy::ConnectionHandler)
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        peer: PeerId,
        _: &Multiaddr,
        _: Endpoint,
        _: PortUse,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        self.record_event(ConnectionEvent::OpenOutbound(peer));
        Ok(dummy::ConnectionHandler)
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        if let FromSwarm::ConnectionClosed(ConnectionClosed {
            peer_id, endpoint, ..
        }) = event
        {
            match endpoint {
                Dialer { .. } => {
                    self.record_event(ConnectionEvent::CloseInbound(peer_id));
                }
                Listener { .. } => {
                    self.record_event(ConnectionEvent::CloseOutbound(peer_id));
                }
            }
        }
    }

    fn on_connection_handler_event(
        &mut self,
        _: PeerId,
        _: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        libp2p::core::util::unreachable(event)
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: refactor after tests are green"
    )]
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        self.waker = Some(cx.waker().clone());

        if let Poll::Ready(Some(cmd)) = self.command_receiver.poll_recv(cx) {
            match cmd {
                ConnectionBalancerCommand::Stats(response) => {
                    let stats = self.balancer.stats();
                    if let Err(err) = response.send(stats) {
                        tracing::error!("Error while sending response to a channel: {err:?}");
                    }
                }
            }

            cx.waker().wake_by_ref();
        }

        if self.subnets_refresh_signal.as_mut().poll_next(cx) == Poll::Ready(Some(())) {
            tracing::info!("BALANCER: Received subnet refresh signal, re-attributing connections");
            self.balancer.refresh_subnet_attributions();

            if let Some(stats_sender) = &self.stats_sender
                && let Err(err) = stats_sender.send(self.balancer.stats())
            {
                tracing::error!("Error while sending refreshed stats: {err:?}");
            }

            cx.waker().wake_by_ref();
        }

        if self.peers_to_dial.is_empty()
            && let Poll::Ready(peers) = self.balancer.poll(cx)
        {
            self.peers_to_dial = peers;
        }

        if let Some(peer) = self.peers_to_dial.pop_front()
            && let Some(addr) = self.addressbook.get_address(&peer)
        {
            let opts = DialOpts::peer_id(peer)
                .addresses(vec![addr])
                .extend_addresses_through_behaviour()
                .build();
            return Poll::Ready(ToSwarm::Dial { opts });
        }

        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, time::Duration};

    use libp2p::swarm::{Swarm, SwarmEvent};
    use libp2p_swarm_test::SwarmExt as _;
    use tokio::time::timeout;

    use super::*;
    use crate::test_utils::AllNeighbours;

    #[derive(Default)]
    struct MockBalancer {
        peers_to_connect: VecDeque<PeerId>,
        connected_peers: HashSet<PeerId>,
    }

    impl MockBalancer {
        fn peer_to_connect(&mut self, peer: PeerId) {
            self.peers_to_connect.push_back(peer);
        }
    }

    impl ConnectionBalancer for MockBalancer {
        type Stats = ();

        fn record_event(&mut self, event: ConnectionEvent) {
            match event {
                ConnectionEvent::OpenInbound(peer) | ConnectionEvent::OpenOutbound(peer) => {
                    self.connected_peers.insert(peer);
                }
                ConnectionEvent::CloseInbound(peer) | ConnectionEvent::CloseOutbound(peer) => {
                    self.connected_peers.remove(&peer);
                }
            }
        }

        fn poll(&mut self, _: &mut Context<'_>) -> Poll<VecDeque<PeerId>> {
            if self.peers_to_connect.is_empty() {
                Poll::Pending
            } else {
                Poll::Ready(self.peers_to_connect.drain(..).collect())
            }
        }

        fn refresh_subnet_attributions(&mut self) {}

        fn stats(&self) {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn test_balancer_dials_provided_peers() {
        let membership_dialer = AllNeighbours::default();

        let mut dialer = Swarm::new_ephemeral_tokio(|_| {
            ConnectionBalancerBehaviour::new(
                membership_dialer.clone(),
                MockBalancer::default(),
                None,
                futures::stream::pending(),
            )
        });

        let mut listener = Swarm::new_ephemeral_tokio(|_| {
            ConnectionBalancerBehaviour::new(
                AllNeighbours::default(),
                MockBalancer::default(),
                None,
                futures::stream::pending(),
            )
        });

        let dialer_peer = *dialer.local_peer_id();
        let listener_peer = *listener.local_peer_id();
        listener.listen().with_memory_addr_external().await;

        let address_book = listener
            .external_addresses()
            .cloned()
            .map(|addr| (listener_peer, addr))
            .collect();

        membership_dialer.update_addresses(address_book);
        // Using balancer `peer_to_connect` we are populating the peers list that will
        // be returned when the balancer is polled by the dialer.
        dialer
            .behaviour_mut()
            .balancer
            .peer_to_connect(listener_peer);

        let listener_task = tokio::spawn(timeout(Duration::from_millis(500), async move {
            listener
                .wait(|e| match e {
                    SwarmEvent::ConnectionEstablished { .. } => Some(()),
                    _ => None,
                })
                .await;

            listener.behaviour().balancer.connected_peers.clone()
        }));

        let dialer_task = tokio::spawn(timeout(Duration::from_millis(500), async move {
            dialer
                .wait(|e| match e {
                    SwarmEvent::ConnectionEstablished { .. } => Some(()),
                    _ => None,
                })
                .await;

            dialer.behaviour().balancer.connected_peers.clone()
        }));

        let (listener_result, dialer_result) = tokio::join!(listener_task, dialer_task);
        let listener_addresses = listener_result
            .expect("Listener timeout")
            .expect("Listener error");
        let dialer_addresses = dialer_result
            .expect("Dialer timeout")
            .expect("Dialer error");

        assert!(listener_addresses.contains(&dialer_peer));
        assert!(dialer_addresses.contains(&listener_peer));
    }

    #[tokio::test]
    async fn test_balancer_dials_all_peers_from_poll() {
        let membership_dialer = AllNeighbours::default();

        let mut dialer = Swarm::new_ephemeral_tokio(|_| {
            ConnectionBalancerBehaviour::new(
                membership_dialer.clone(),
                MockBalancer::default(),
                None,
                futures::stream::pending(),
            )
        });

        let peer1 = PeerId::random();
        let peer2 = PeerId::random();
        let peer3 = PeerId::random();

        let addresses = vec![
            (peer1, "/ip4/127.0.0.1/tcp/4001".parse().unwrap()),
            (peer2, "/ip4/127.0.0.1/tcp/4002".parse().unwrap()),
            (peer3, "/ip4/127.0.0.1/tcp/4003".parse().unwrap()),
        ];

        membership_dialer.update_addresses(addresses);

        dialer.behaviour_mut().balancer.peer_to_connect(peer1);
        dialer.behaviour_mut().balancer.peer_to_connect(peer2);
        dialer.behaviour_mut().balancer.peer_to_connect(peer3);

        let mut dial_requests = Vec::new();

        let mut cx = Context::from_waker(futures::task::noop_waker_ref());

        // Call poll repeatedly and collect all dial requests
        while let Poll::Ready(to_swarm) = dialer.behaviour_mut().poll(&mut cx) {
            if let ToSwarm::Dial { opts } = to_swarm {
                dial_requests.push(opts);
            }
        }

        let dialed_peers: HashSet<_> = dial_requests
            .iter()
            .map(|opts| opts.get_peer_id().unwrap())
            .collect();

        assert_eq!(dialed_peers.len(), 3, "Expected 3 ToSwarm::Dial requests");
        assert!(
            dialed_peers.contains(&peer1),
            "Expected dial request for peer1"
        );
        assert!(
            dialed_peers.contains(&peer2),
            "Expected dial request for peer2"
        );
        assert!(
            dialed_peers.contains(&peer3),
            "Expected dial request for peer3"
        );

        // Ensure that the balancer queue is now empty
        assert!(dialer.behaviour_mut().balancer.peers_to_connect.is_empty());
    }
}
