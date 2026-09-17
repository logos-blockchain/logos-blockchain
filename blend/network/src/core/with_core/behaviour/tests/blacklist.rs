use core::{
    num::{NonZeroU64, NonZeroU128},
    time::Duration,
};

use futures::{AsyncWriteExt as _, StreamExt as _};
use lb_libp2p::SwarmEvent;
use libp2p::{Multiaddr, Stream};
use libp2p_stream::{Behaviour as StreamBehaviour, IncomingStreams};
use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::{select, time::timeout};

use crate::core::{
    tests::utils::{PROTOCOL_NAME, TestSwarm, undecodable_message_bytes},
    with_core::behaviour::{
        Event,
        blacklist::BlacklistReason,
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

/// Drives both swarms for `duration`, so connections close and rounds elapse
/// while nothing in particular is being waited for.
async fn drive_for(
    one: &mut TestSwarm<TestBehaviour>,
    other: &mut TestSwarm<TestBehaviour>,
    duration: Duration,
) {
    let _: Result<(), _> = timeout(duration, async {
        loop {
            select! {
                _ = one.select_next_some() => {}
                _ = other.select_next_some() => {}
            }
        }
    })
    .await;
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
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let offender = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_liveness(ROUND, window_in_rounds)
            .build()
    });
    let listener = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_liveness(ROUND, window_in_rounds)
            .build()
    });
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

/// A message that stops part way through is a framing violation, and the spec
/// makes it a blacklisting. This node does not act on it yet, and that is
/// deliberate: its own close drops the substreams outright, so a neighbour
/// closed while a send was in flight produces exactly this signal through no
/// fault of its own — and the protocol asks nodes to close, at every epoch
/// boundary among other times. Blacklisting here would turn a rotation into a
/// mutual partition. This assertion flips once closing flushes what is in
/// flight, at which point the signal becomes attributable.
#[test(tokio::test)]
async fn a_message_that_stops_part_way_through_is_not_yet_a_fault() {
    let (mut offender, mut listener) = core_and_raw_peer();
    listener.listen().with_memory_addr_external().await;
    let (mut stream, _incoming) = open_raw_core_stream(&mut offender, &mut listener).await;

    // The start of a message, and then nothing. Every message is the same
    // fixed size, so a stream that ends here ended part way through one.
    stream.write_all(b"the start of a message").await.unwrap();
    stream.close().await.unwrap();

    assert_eq!(
        wait_for_blacklisting(&mut offender, &mut listener, Duration::from_secs(3)).await,
        None,
        "a truncated message must not be attributed to the sender while an \
         ordinary close produces the same signal"
    );
}

/// Ending the stream between messages is how a connection ends, not a fault,
/// and must stay that way — blacklisting for it would exclude every peer that
/// restarts. This holds today because no read failure is a fault at all, and
/// it must still hold once truncation becomes one.
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
