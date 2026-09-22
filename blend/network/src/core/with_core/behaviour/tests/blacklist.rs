use core::{
    num::{NonZeroU64, NonZeroU128},
    time::Duration,
};

use either::Either;
use futures::{AsyncWriteExt as _, StreamExt as _};
use lb_libp2p::SwarmEvent;
use libp2p::{
    Multiaddr, PeerId, Stream,
    swarm::{ConnectionId, NetworkBehaviour as _, ToSwarm},
};
use libp2p_stream::{Behaviour as StreamBehaviour, IncomingStreams};
use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::{select, time::timeout};

use crate::core::{
    tests::utils::{
        PROTOCOL_NAME, TestEncapsulatedMessage, TestSwarm, drive_for, undecodable_message_bytes,
    },
    with_core::behaviour::{
        ConnectionDirection, ConnectionUpgradeFailureReason, Event, PendingUpgrade,
        RemotePeerConnectionDetails,
        blacklist::BlacklistReason,
        handler::ToBehaviour,
        tests::utils::{
            BehaviourBuilder, SwarmExt as _, TestBehaviour, new_nodes_with_empty_address,
        },
    },
};

const ROUND: NonZeroU64 = NonZeroU64::new(1).unwrap();
/// Short enough that a test can outlive an entry without sleeping for long.
/// The blacklist forgets an offence after the same window liveness uses.
const WINDOW_IN_ROUNDS: NonZeroU128 = NonZeroU128::new(3).unwrap();
/// Long enough that nothing expires while a test is looking at it.
const LONG_WINDOW_IN_ROUNDS: NonZeroU128 = NonZeroU128::new(1_000).unwrap();

/// Drives both swarms until the listener blacklists the offender, and returns
/// the reason it gave.
async fn provoke_blacklisting(
    offender: &mut TestSwarm<TestBehaviour>,
    listener: &mut TestSwarm<TestBehaviour>,
) -> BlacklistReason {
    offender
        .behaviour_mut()
        .force_send_serialized_message_to_current_epoch_peer(
            &undecodable_message_bytes(),
            *listener.local_peer_id(),
        )
        .unwrap();

    loop {
        select! {
            _ = offender.select_next_some() => {}
            event = listener.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::PeerBlacklisted { peer, reason }) = event {
                    assert_eq!(peer, *offender.local_peer_id());
                    return reason;
                }
            }
        }
    }
}

/// Drives both swarms until the listener upgrades an inbound connection with
/// the dialer, giving up after `patience`.
async fn wait_for_upgrade(
    dialer: &mut TestSwarm<TestBehaviour>,
    listener: &mut TestSwarm<TestBehaviour>,
    patience: Duration,
) -> bool {
    timeout(patience, async {
        loop {
            select! {
                _ = dialer.select_next_some() => {}
                event = listener.select_next_some() => {
                    if let SwarmEvent::Behaviour(Event::InboundConnectionUpgradeSucceeded(peer)) = event
                        && peer == *dialer.local_peer_id()
                    {
                        return;
                    }
                }
            }
        }
    })
    .await
    .is_ok()
}

fn pair(window_in_rounds: NonZeroU128) -> (TestSwarm<TestBehaviour>, TestSwarm<TestBehaviour>) {
    pair_with_share(window_in_rounds, NonZeroU64::new(1_000).unwrap())
}

/// A pair whose connections carry at most `share_per_round` messages a round,
/// so that a backlog can be made to outlast whatever the test is waiting for.
fn pair_with_share(
    window_in_rounds: NonZeroU128,
    share_per_round: NonZeroU64,
) -> (TestSwarm<TestBehaviour>, TestSwarm<TestBehaviour>) {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let build = |id: &_| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_liveness(ROUND, window_in_rounds)
            .with_connection_share_per_round(share_per_round)
            .build()
    };
    let offender = TestSwarm::new(&identities.next().unwrap(), build);
    let listener = TestSwarm::new(&identities.next().unwrap(), build);
    (offender, listener)
}

async fn listen(listener: &mut TestSwarm<TestBehaviour>) -> Multiaddr {
    let (address, _) = listener.listen().with_memory_addr_external().await;
    address
}

#[test(tokio::test)]
async fn a_blacklisted_peer_cannot_dial_its_way_back_in() {
    let (mut offender, mut listener) = pair(LONG_WINDOW_IN_ROUNDS);
    let listening_address = listen(&mut listener).await;
    offender.connect_and_wait_for_upgrade(&mut listener).await;

    let reason = provoke_blacklisting(&mut offender, &mut listener).await;
    assert_eq!(reason, BlacklistReason::UndeserializableMessage);

    assert!(
        listener
            .behaviour()
            .blacklisted_peers()
            .any(|peer| peer == offender.local_peer_id()),
        "the offender must be excluded from the peers this node will deal with"
    );

    // The offence cost the offender the connection it made it on; dialing
    // again must not buy it another one.
    offender.dial(listening_address).unwrap();

    assert!(
        !wait_for_upgrade(&mut offender, &mut listener, Duration::from_secs(3)).await,
        "a blacklisted peer must not be able to dial its way back in"
    );
}

#[test(tokio::test)]
async fn a_blacklisting_is_forgotten_once_its_window_has_passed() {
    let (mut offender, mut listener) = pair(WINDOW_IN_ROUNDS);
    let listening_address = listen(&mut listener).await;
    offender.connect_and_wait_for_upgrade(&mut listener).await;

    provoke_blacklisting(&mut offender, &mut listener).await;

    // Let the entry outlive its window. A dial placed before this point is
    // refused once and never retried, so the wait has to come first.
    let window = Duration::from_secs(u64::try_from(WINDOW_IN_ROUNDS.get()).unwrap() * ROUND.get());
    drive_for(
        &mut offender,
        &mut listener,
        window + Duration::from_secs(1),
    )
    .await;

    assert!(
        listener.behaviour().blacklisted_peers().next().is_none(),
        "the entry must have been forgotten by now"
    );

    // And a fresh dial is upgraded like any other. A permanent block list —
    // which is what this replaced — would never get here.
    offender.dial(listening_address).unwrap();

    assert!(
        wait_for_upgrade(&mut offender, &mut listener, Duration::from_secs(5)).await,
        "a blacklisting must expire, so a node cannot be argued out of the network one peer at a time"
    );
}

/// Drives both swarms until the listener blacklists the peer, or `patience`
/// runs out, and reports the reason it gave.
async fn wait_for_blacklisting(
    offender: &mut TestSwarm<StreamBehaviour>,
    listener: &mut TestSwarm<TestBehaviour>,
    patience: Duration,
) -> Option<BlacklistReason> {
    timeout(patience, async {
        loop {
            select! {
                _ = offender.select_next_some() => {}
                event = listener.select_next_some() => {
                    if let SwarmEvent::Behaviour(Event::PeerBlacklisted { peer, reason }) = event {
                        assert_eq!(peer, *offender.local_peer_id());
                        return reason;
                    }
                }
            }
        }
    })
    .await
    .ok()
}

/// Connects a raw-stream peer to a core node and hands back the stream it
/// opened, so a test can put bytes on the wire that no correct node would.
///
/// The returned [`IncomingStreams`] must be held for as long as the test runs:
/// the core node opens a substream of its own, and with
/// `idle_connection_timeout` at zero a connection whose substreams have all
/// failed to negotiate is dropped before a byte can be written.
async fn open_raw_core_stream(
    offender: &mut TestSwarm<StreamBehaviour>,
    listener: &mut TestSwarm<TestBehaviour>,
) -> (Stream, IncomingStreams) {
    let mut control = offender.behaviour_mut().new_control();
    let incoming = control.accept(PROTOCOL_NAME).unwrap();
    offender.connect(listener).await;
    let listener_peer_id = *listener.local_peer_id();
    let mut open = Box::pin(control.open_stream(listener_peer_id, PROTOCOL_NAME));
    let stream = loop {
        select! {
            opened = &mut open => break opened.unwrap(),
            _ = offender.select_next_some() => {}
            _ = listener.select_next_some() => {}
        }
    };
    (stream, incoming)
}

fn core_and_raw_peer() -> (TestSwarm<StreamBehaviour>, TestSwarm<TestBehaviour>) {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let offender = TestSwarm::new(&identities.next().unwrap(), |_| StreamBehaviour::new());
    let listener = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_liveness(ROUND, LONG_WINDOW_IN_ROUNDS)
            .build()
    });
    (offender, listener)
}

/// A message that stops part way through ends the connection and nothing more.
#[test(tokio::test)]
async fn a_message_that_stops_part_way_through_blacklists_nobody() {
    let (mut offender, mut listener) = core_and_raw_peer();
    listener.listen().with_memory_addr_external().await;
    let (mut stream, _incoming) = open_raw_core_stream(&mut offender, &mut listener).await;

    // The start of a message, and then nothing. Every message is the same
    // fixed size, so a stream that ends here ended part way through one.
    stream.write_all(b"the start of a message").await.unwrap();
    stream.close().await.unwrap();

    assert_eq!(
        wait_for_blacklisting(&mut offender, &mut listener, Duration::from_secs(5)).await,
        None
    );
}

/// Closing with traffic in flight must not look like a fault either.
#[test(tokio::test)]
async fn closing_a_connection_carrying_traffic_blacklists_nobody() {
    // One message a round against a backlog of many, so the connection still
    // has messages to send when the two give up on each other, rather than
    // having gone quiet long before.
    let (mut one, mut other) = pair_with_share(WINDOW_IN_ROUNDS, NonZeroU64::new(1).unwrap());
    other.listen().with_memory_addr_external().await;
    one.connect_and_wait_for_upgrade(&mut other).await;

    // Enough distinct messages that a send is in flight whenever the close
    // lands, rather than the connection sitting idle at a boundary.
    for nonce in 0..64 {
        let message = TestEncapsulatedMessage::new_distinct(nonce, b"in flight");
        one.behaviour_mut()
            .publish_message_with_validated_header_to_current_epoch(message.as_ref())
            .unwrap();
    }

    let window = Duration::from_secs(u64::try_from(WINDOW_IN_ROUNDS.get()).unwrap() * ROUND.get());
    let blacklisted = timeout(window + Duration::from_secs(3), async {
        loop {
            select! {
                event = one.select_next_some() => {
                    if let SwarmEvent::Behaviour(Event::PeerBlacklisted { .. }) = event {
                        return "the sender blacklisted the peer that closed on it";
                    }
                }
                event = other.select_next_some() => {
                    if let SwarmEvent::Behaviour(Event::PeerBlacklisted { .. }) = event {
                        return "the receiver blacklisted the peer whose close interrupted a send";
                    }
                }
            }
        }
    })
    .await;

    if let Ok(blacklisted) = blacklisted {
        panic!("Closing must not be a fault, but {blacklisted}.");
    }
}

/// Ending the stream between messages is how a connection ends, not a fault:
/// blacklisting for it would exclude every peer that restarts. The line between
/// this and the test above is the whole of what makes a framing violation
/// attributable.
#[test(tokio::test)]
async fn closing_between_messages_is_not_a_fault() {
    let (mut offender, mut listener) = core_and_raw_peer();
    listener.listen().with_memory_addr_external().await;
    let (mut stream, _incoming) = open_raw_core_stream(&mut offender, &mut listener).await;

    stream.close().await.unwrap();

    assert_eq!(
        wait_for_blacklisting(&mut offender, &mut listener, Duration::from_secs(3)).await,
        None,
        "a peer that closes cleanly must not be blacklisted"
    );
}

/// Closing a connection is something the protocol tells nodes to do — for a
/// neighbour that has gone silent, for one offered above the peering degree,
/// at an epoch boundary. The peer on the other end must not read that as a
/// fault, or every ordinary disconnection becomes mutual exclusion for `W`
/// rounds and the peering degree starves.
#[test(tokio::test)]
async fn a_connection_closed_by_the_protocol_blacklists_nobody() {
    // A window short enough that both ends give up on each other, since
    // neither sends anything.
    let (mut one, mut other) = pair(WINDOW_IN_ROUNDS);
    other.listen().with_memory_addr_external().await;
    one.connect_and_wait_for_upgrade(&mut other).await;

    let window = Duration::from_secs(u64::try_from(WINDOW_IN_ROUNDS.get()).unwrap() * ROUND.get());
    let blacklisted = timeout(window + Duration::from_secs(3), async {
        loop {
            select! {
                event = one.select_next_some() => {
                    if let SwarmEvent::Behaviour(Event::PeerBlacklisted { .. }) = event {
                        return "the dialer blacklisted the peer that closed it";
                    }
                }
                event = other.select_next_some() => {
                    if let SwarmEvent::Behaviour(Event::PeerBlacklisted { .. }) = event {
                        return "the listener blacklisted the peer that closed it";
                    }
                }
            }
        }
    })
    .await;

    if let Ok(blacklisted) = blacklisted {
        panic!("Closing a connection must not be a fault, but {blacklisted}.");
    }
}

/// Asserts that the one thing the behaviour has queued for the swarm is a
/// refusal of the dial to `peer`, which is what stops the swarm tracking it and
/// sends it looking for another peer.
fn assert_dial_refused(behaviour: &TestBehaviour, peer: PeerId) {
    let failures = behaviour
        .events
        .iter()
        .filter_map(|event| match event {
            ToSwarm::GenerateEvent(Event::OutboundConnectionUpgradeFailed { peer, reason }) => {
                Some((*peer, reason))
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    let [(failed_peer, reason)] = failures[..] else {
        panic!("the swarm was told nothing about the dial it is still tracking: {failures:?}");
    };
    assert_eq!(failed_peer, peer);
    assert!(
        matches!(reason, ConnectionUpgradeFailureReason::Refused),
        "the refusal must not read as a transport failure, which the swarm answers with a retry ladder toward the same peer: {reason:?}"
    );
}

/// Blacklisting asks every connection with the peer to close, but a handshake
/// that finished just before cannot be recalled: the `FullyNegotiated` for it
/// is already on its way to the behaviour. Acting on it would file the peer as
/// a neighbour — holding a degree slot, earning liveness credit, and reported
/// to the swarm as a dial that worked — moments after this node decided it
/// wants nothing to do with it.
#[test(tokio::test)]
async fn a_handshake_that_finishes_after_its_peer_is_blacklisted_is_refused() {
    let (mut identities, _) = new_nodes_with_empty_address(1);
    let mut behaviour = BehaviourBuilder::new(&identities.next().unwrap()).build();

    let peer = PeerId::random();
    let connection = ConnectionId::new_unchecked(1);
    behaviour.connections_waiting_upgrade.insert(
        (peer, connection),
        PendingUpgrade {
            direction: ConnectionDirection::Outgoing,
            started_at: behaviour.current_round,
        },
    );
    behaviour.blacklist_peer(peer, BlacklistReason::InvalidProofOfQuota);
    behaviour.events.clear();

    behaviour.on_connection_handler_event(
        peer,
        connection,
        Either::Left(ToBehaviour::FullyNegotiated),
    );

    assert!(
        !behaviour.negotiated_peers.contains_key(&peer),
        "a blacklisted peer was taken on as a neighbour"
    );
    assert_dial_refused(&behaviour, peer);
}

/// The same race, but with a connection to the peer already in place. Here the
/// stale handshake is not merely admitted: the reverse-direction tie-break
/// hands it the record of the connection that is on its way out, so the peer
/// this node just blacklisted ends up holding a live slot with no close
/// pending against it.
#[test(tokio::test)]
async fn a_blacklisted_peer_does_not_take_over_the_connection_being_closed() {
    // `new_nodes_with_empty_address` orders identities by peer id, and every
    // identity is ed25519, so the two ids share a length and a prefix and read
    // in the same order as the base58 the tie-break compares. Taking the higher
    // one as the local node is what makes the tie-break hand the new connection
    // the record, which is the outcome worth guarding against.
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let peer = nodes[0].id;
    let mut behaviour = BehaviourBuilder::new(&identities.nth(1).unwrap()).build();
    assert!(
        behaviour.local_peer_id.to_base58() > peer.to_base58(),
        "the tie-break would not have replaced the connection, so the test would prove nothing"
    );

    let established = ConnectionId::new_unchecked(1);
    let negotiating = ConnectionId::new_unchecked(2);
    behaviour.negotiated_peers.insert(
        peer,
        RemotePeerConnectionDetails {
            direction: ConnectionDirection::Incoming,
            connection_id: established,
        },
    );
    behaviour.connections_waiting_upgrade.insert(
        (peer, negotiating),
        PendingUpgrade {
            direction: ConnectionDirection::Outgoing,
            started_at: behaviour.current_round,
        },
    );
    behaviour.blacklist_peer(peer, BlacklistReason::InvalidProofOfQuota);
    behaviour.events.clear();

    behaviour.on_connection_handler_event(
        peer,
        negotiating,
        Either::Left(ToBehaviour::FullyNegotiated),
    );

    assert_eq!(
        behaviour.negotiated_peers[&peer].connection_id, established,
        "the blacklisted peer was handed the record of the connection being closed, so nothing closes it any more"
    );
    assert_dial_refused(&behaviour, peer);
}

/// A peer can offend once per message it is allowed to send in a round, and
/// every one of them reaches the behaviour. Reporting each would turn a count
/// of the peers this node has had to shut out into a count of how talkative
/// they were on their way out.
#[test(tokio::test)]
async fn a_peer_that_offends_again_is_reported_once() {
    let (mut identities, _) = new_nodes_with_empty_address(1);
    let mut behaviour = BehaviourBuilder::new(&identities.next().unwrap()).build();
    let peer = PeerId::random();

    behaviour.blacklist_peer(peer, BlacklistReason::InvalidProofOfQuota);
    behaviour.blacklist_peer(peer, BlacklistReason::UndeserializableMessage);

    let times_reported = behaviour
        .events
        .iter()
        .filter(|event| matches!(event, ToSwarm::GenerateEvent(Event::PeerBlacklisted { .. })))
        .count();

    assert_eq!(times_reported, 1);
    assert_eq!(
        behaviour.blacklisted_peers().count(),
        1,
        "and the peer still occupies exactly one of the entries there is room for"
    );
}
