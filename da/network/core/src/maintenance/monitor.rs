use std::{
    collections::{HashMap, HashSet, VecDeque},
    convert::Infallible,
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};

use libp2p::{
    Multiaddr, PeerId,
    core::{Endpoint, transport::PortUse},
    swarm::{
        CloseConnection, ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler,
        THandlerInEvent, THandlerOutEvent, ToSwarm, dummy,
    },
};
use thiserror::Error;
use tokio::sync::{
    mpsc::{self, UnboundedReceiver, UnboundedSender},
    oneshot,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PeerStatus {
    Malicious,
    Unhealthy,
    Healthy,
}

pub struct ConnectionMonitorOutput {
    pub peer_id: PeerId,
    pub peer_status: PeerStatus,
}

pub trait ConnectionMonitor {
    type Event;
    type Stats;

    fn record_event(&mut self, event: Self::Event) -> Option<ConnectionMonitorOutput>;
    fn reset_peer(&mut self, peer_id: &PeerId);
    fn stats(&self) -> Self::Stats;
}

#[derive(Debug)]
pub enum ConnectionMonitorCommand<Stats> {
    Block(PeerId, oneshot::Sender<bool>),
    Unblock(PeerId, oneshot::Sender<bool>),
    BlacklistedPeers(oneshot::Sender<Vec<PeerId>>),
    Stats(oneshot::Sender<Stats>),
}

impl<Stats> ConnectionMonitorCommand<Stats> {
    #[must_use]
    pub const fn discriminant(&self) -> &str {
        match self {
            Self::Block(_, _) => "Block",
            Self::Unblock(_, _) => "Unblock",
            Self::BlacklistedPeers(_) => "BlacklistedPeers",
            Self::Stats(_) => "Stats",
        }
    }

    #[must_use]
    pub const fn peer_id(&self) -> Option<&PeerId> {
        match self {
            Self::Block(peer, _) | Self::Unblock(peer, _) => Some(peer),
            Self::BlacklistedPeers(_) | Self::Stats(_) => None,
        }
    }
}

/// A `NetworkBehaviour` that block connections to malicious peers or
/// temporarily disconnects from unhealthy peers.
#[derive(Debug)]
pub struct ConnectionMonitorBehaviour<Monitor>
where
    Monitor: ConnectionMonitor,
{
    monitor: Monitor,
    malicious_peers: HashSet<PeerId>,
    unhealthy_peers: HashMap<PeerId, Instant>,
    close_connections: VecDeque<PeerId>,
    redial_cooldown: Duration,
    command_sender:
        UnboundedSender<ConnectionMonitorCommand<<Monitor as ConnectionMonitor>::Stats>>,
    command_receiver:
        UnboundedReceiver<ConnectionMonitorCommand<<Monitor as ConnectionMonitor>::Stats>>,
    waker: Option<Waker>,
}

impl<Monitor> ConnectionMonitorBehaviour<Monitor>
where
    Monitor: ConnectionMonitor,
{
    pub fn new(monitor: Monitor, redial_cooldown: Duration) -> Self {
        let (command_sender, command_receiver) = mpsc::unbounded_channel();

        Self {
            monitor,
            malicious_peers: HashSet::new(),
            unhealthy_peers: HashMap::new(),
            close_connections: VecDeque::new(),
            redial_cooldown,
            command_sender,
            command_receiver,
            waker: None,
        }
    }

    fn try_wake(&mut self) {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }

    /// Block connections to a given peer.
    ///
    /// All active connections to this peer will be closed immediately.
    ///
    /// Returns whether the peer was newly inserted. Does nothing if the peer
    /// was already present in the set.
    pub fn block_peer(&mut self, peer: PeerId) -> bool {
        let inserted = self.malicious_peers.insert(peer);
        if inserted {
            self.close_connections.push_back(peer);
        }
        inserted
    }

    /// **Temporarily blocks a peer** due to unhealthy behavior.
    ///
    /// The peer is added to `unhealthy_peers`, and after `redial_cooldown`, it
    /// will be allowed to reconnect. The existing connections to this peer
    /// are **closed immediately**.
    pub fn temporarily_block_peer(&mut self, peer: PeerId) {
        let until = Instant::now() + self.redial_cooldown;
        self.unhealthy_peers.insert(peer, until);
        // Close existing connections
        self.close_connections.push_back(peer);
    }

    /// Unblock connections to a given peer.
    ///
    /// Returns whether the peer was present in the set. Does nothing if the
    /// peer was not present in the set.
    pub fn unblock_peer(&mut self, peer: PeerId) -> bool {
        self.malicious_peers.remove(&peer) || self.unhealthy_peers.remove(&peer).is_some()
    }

    pub fn record_event(&mut self, event: Monitor::Event) {
        if let Some(output) = self.monitor.record_event(event) {
            match output.peer_status {
                PeerStatus::Malicious => {
                    self.block_peer(output.peer_id);
                    self.try_wake();
                }
                PeerStatus::Unhealthy => {
                    self.temporarily_block_peer(output.peer_id);
                    self.try_wake();
                }
                PeerStatus::Healthy => {}
            }
        }
    }

    pub fn command_channel(
        &self,
    ) -> UnboundedSender<ConnectionMonitorCommand<<Monitor as ConnectionMonitor>::Stats>> {
        self.command_sender.clone()
    }

    /// Enforce connection rules (deny blocked peers).
    fn enforce(&mut self, peer: &PeerId) -> Result<(), ConnectionDenied> {
        let now = Instant::now();
        self.unhealthy_peers
            .retain(|_peer, &mut until| now <= until);

        if self.malicious_peers.contains(peer) {
            return Err(ConnectionDenied::new(Blocked { peer: *peer }));
        }
        if self.unhealthy_peers.contains_key(peer) {
            return Err(ConnectionDenied::new(TemporarilyBlocked { peer: *peer }));
        }
        Ok(())
    }
}

/// A connection to this peer was explicitly blocked or malicious.
#[derive(Debug, Error)]
#[error("peer {peer} is in the block list")]
pub struct Blocked {
    peer: PeerId,
}

/// A connection to this peer is temporarily blocked due to being unhealthy.
#[derive(Debug, Error)]
#[error("peer {peer} is temporarily blocked due to being unhealthy")]
pub struct TemporarilyBlocked {
    peer: PeerId,
}

impl<Monitor> NetworkBehaviour for ConnectionMonitorBehaviour<Monitor>
where
    Monitor: ConnectionMonitor + 'static,
{
    type ConnectionHandler = dummy::ConnectionHandler;
    type ToSwarm = Infallible;

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        peer: PeerId,
        _: &Multiaddr,
        _: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.enforce(&peer)?;

        Ok(dummy::ConnectionHandler)
    }

    fn handle_pending_outbound_connection(
        &mut self,
        _: ConnectionId,
        peer: Option<PeerId>,
        _: &[Multiaddr],
        _: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        if let Some(peer) = peer {
            self.enforce(&peer)?;
        }

        Ok(vec![])
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        peer: PeerId,
        _: &Multiaddr,
        _: Endpoint,
        _: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.enforce(&peer)?;

        Ok(dummy::ConnectionHandler)
    }

    fn on_swarm_event(&mut self, _event: FromSwarm) {}

    fn on_connection_handler_event(
        &mut self,
        _id: PeerId,
        _: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        libp2p::core::util::unreachable(event)
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        self.waker = Some(cx.waker().clone());

        if let Poll::Ready(Some(cmd)) = self.command_receiver.poll_recv(cx) {
            match cmd {
                ConnectionMonitorCommand::Block(peer, response) => {
                    let result = self.block_peer(peer);
                    let _ = response.send(result);
                }
                ConnectionMonitorCommand::Unblock(peer, response) => {
                    let result = self.unblock_peer(peer);
                    let _ = response.send(result);
                }
                ConnectionMonitorCommand::BlacklistedPeers(response) => {
                    let blacklisted_peers = self.malicious_peers.iter().copied().collect();
                    drop(response.send(blacklisted_peers));
                }
                ConnectionMonitorCommand::Stats(response) => {
                    let stats = self.monitor.stats();
                    drop(response.send(stats));
                }
            }

            cx.waker().wake_by_ref();
        }

        if let Some(peer) = self.close_connections.pop_front() {
            return Poll::Ready(ToSwarm::CloseConnection {
                peer_id: peer,
                connection: CloseConnection::All,
            });
        }

        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, task::Context, time::Duration};

    use libp2p::{
        PeerId, Swarm,
        swarm::{DialError, ListenError, NetworkBehaviour as _, SwarmEvent, dial_opts::DialOpts},
    };
    use libp2p_swarm_test::SwarmExt as _;
    use tokio::sync::oneshot;

    use crate::maintenance::monitor::{
        Blocked, ConnectionMonitor, ConnectionMonitorBehaviour, ConnectionMonitorCommand,
        ConnectionMonitorOutput, PeerStatus, TemporarilyBlocked,
    };

    #[derive(Default)]
    struct MockMonitor {
        stats: HashMap<PeerId, PeerStatus>,
    }

    impl ConnectionMonitor for MockMonitor {
        type Event = (PeerId, PeerStatus);
        type Stats = ();

        fn record_event(
            &mut self,
            (peer_id, peer_status): Self::Event,
        ) -> Option<ConnectionMonitorOutput> {
            self.stats.insert(peer_id, peer_status);
            Some(ConnectionMonitorOutput {
                peer_id,
                peer_status,
            })
        }

        fn reset_peer(&mut self, peer_id: &PeerId) {
            self.stats.remove(peer_id);
        }

        fn stats(&self) {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn test_cannot_dial_unhealthy_peer() {
        let mut dialer = Swarm::new_ephemeral_tokio(|_| {
            ConnectionMonitorBehaviour::new(MockMonitor::default(), Duration::from_secs(1))
        });
        let mut listener = Swarm::new_ephemeral_tokio(|_| {
            ConnectionMonitorBehaviour::new(MockMonitor::default(), Duration::from_secs(1))
        });
        listener.listen().with_memory_addr_external().await;

        let listener_peer = *listener.local_peer_id();
        dialer
            .behaviour_mut()
            .record_event((listener_peer, PeerStatus::Unhealthy));

        let DialError::Denied { cause } = dial(&mut dialer, &listener).unwrap_err() else {
            panic!("unexpected dial error")
        };
        assert!(cause.downcast::<TemporarilyBlocked>().is_ok());
    }

    #[tokio::test]
    async fn test_can_dial_unhealthy_peer_after_cooldown() {
        let mut dialer = Swarm::new_ephemeral_tokio(|_| {
            ConnectionMonitorBehaviour::new(MockMonitor::default(), Duration::from_millis(100))
        });
        let mut listener = Swarm::new_ephemeral_tokio(|_| {
            ConnectionMonitorBehaviour::new(MockMonitor::default(), Duration::from_millis(100))
        });
        listener.listen().with_memory_addr_external().await;

        let listener_peer = *listener.local_peer_id();

        dialer
            .behaviour_mut()
            .record_event((listener_peer, PeerStatus::Unhealthy));

        let DialError::Denied { cause } = dial(&mut dialer, &listener).unwrap_err() else {
            panic!("unexpected dial error")
        };
        assert!(cause.downcast::<TemporarilyBlocked>().is_ok());

        tokio::time::sleep(Duration::from_millis(150)).await;

        // Attempt to dial again (should succeed)
        assert!(dial(&mut dialer, &listener).is_ok());
    }

    #[tokio::test]
    async fn test_cannot_accept_malicious_peer() {
        let mut dialer = Swarm::new_ephemeral_tokio(|_| {
            ConnectionMonitorBehaviour::new(MockMonitor::default(), Duration::ZERO)
        });
        let mut listener = Swarm::new_ephemeral_tokio(|_| {
            ConnectionMonitorBehaviour::new(MockMonitor::default(), Duration::ZERO)
        });
        listener.listen().with_memory_addr_external().await;

        let dialer_peer = *dialer.local_peer_id();
        listener
            .behaviour_mut()
            .record_event((dialer_peer, PeerStatus::Malicious));

        dial(&mut dialer, &listener).unwrap();
        tokio::spawn(dialer.loop_on_next());

        let cause = listener
            .wait(|e| match e {
                SwarmEvent::IncomingConnectionError {
                    error: ListenError::Denied { cause },
                    ..
                } => Some(cause),
                _ => None,
            })
            .await;
        assert!(cause.downcast::<Blocked>().is_ok());
    }

    #[tokio::test]
    async fn test_block_unblock_peer() {
        use std::sync::{Arc, Mutex};

        let dialer = Arc::new(Mutex::new(Swarm::new_ephemeral_tokio(|_| {
            ConnectionMonitorBehaviour::new(MockMonitor::default(), Duration::from_millis(100))
        })));
        let mut listener = Swarm::new_ephemeral_tokio(|_| {
            ConnectionMonitorBehaviour::new(MockMonitor::default(), Duration::from_millis(100))
        });
        listener.listen().with_memory_addr_external().await;

        let listener_peer = *listener.local_peer_id();

        let sender_channel = dialer.lock().unwrap().behaviour_mut().command_channel();

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

        let dialer_clone = Arc::clone(&dialer);
        let poll_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_millis(10)) => {
                        let waker = futures::task::noop_waker();
                        let mut cx = Context::from_waker(&waker);
                        drop(dialer_clone.lock().unwrap().behaviour_mut().poll(&mut cx));
                    }
                    _ = &mut shutdown_rx => {
                        break;
                    }
                }
            }
        });

        tokio::time::sleep(Duration::from_millis(20)).await;

        let response_channel = oneshot::channel();

        sender_channel
            .send(ConnectionMonitorCommand::Block(
                listener_peer,
                response_channel.0,
            ))
            .unwrap();

        let result = tokio::time::timeout(Duration::from_millis(100), response_channel.1)
            .await
            .expect("Test timed out")
            .expect("Response channel closed");
        assert!(result);

        // blacklisted peers
        let response_channel = oneshot::channel();
        sender_channel
            .send(ConnectionMonitorCommand::BlacklistedPeers(
                response_channel.0,
            ))
            .unwrap();

        let blacklisted_peers =
            tokio::time::timeout(Duration::from_millis(100), response_channel.1)
                .await
                .expect("Test timed out")
                .expect("Response channel closed");

        assert_eq!(blacklisted_peers.len(), 1);
        assert_eq!(blacklisted_peers[0], listener_peer);

        // unblock
        let response_channel = oneshot::channel();
        sender_channel
            .send(ConnectionMonitorCommand::Unblock(
                listener_peer,
                response_channel.0,
            ))
            .unwrap();

        let result = tokio::time::timeout(Duration::from_millis(100), response_channel.1)
            .await
            .expect("Test timed out")
            .expect("Response channel closed");
        assert!(result);

        // blacklisted peers
        let response_channel = oneshot::channel();
        sender_channel
            .send(ConnectionMonitorCommand::BlacklistedPeers(
                response_channel.0,
            ))
            .unwrap();

        let blacklisted_peers =
            tokio::time::timeout(Duration::from_millis(100), response_channel.1)
                .await
                .expect("Test timed out")
                .expect("Response channel closed");

        assert!(blacklisted_peers.is_empty());

        shutdown_tx.send(()).unwrap();
        poll_handle.await.unwrap();
    }

    fn dial(
        dialer: &mut Swarm<ConnectionMonitorBehaviour<MockMonitor>>,
        listener: &Swarm<ConnectionMonitorBehaviour<MockMonitor>>,
    ) -> Result<(), DialError> {
        dialer.dial(
            DialOpts::peer_id(*listener.local_peer_id())
                .addresses(listener.external_addresses().cloned().collect())
                .build(),
        )
    }
}
