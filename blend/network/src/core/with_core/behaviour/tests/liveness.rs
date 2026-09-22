use core::{
    num::{NonZeroU64, NonZeroU128},
    time::Duration,
};

use futures::StreamExt as _;
use lb_blend_primitives::time::RoundCount;
use lb_libp2p::SwarmEvent;
use libp2p::identity::ed25519;
use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::{select, time::sleep};

use crate::core::{
    tests::utils::{TestEncapsulatedMessage, TestSwarm},
    with_core::behaviour::{
        Event,
        tests::utils::{
            BehaviourBuilder, PEERING_DEGREE, SwarmExt as _, new_nodes_with_empty_address,
        },
    },
};

const ROUND: NonZeroU64 = NonZeroU64::new(1).unwrap();
const WINDOW_IN_ROUNDS: NonZeroU128 = NonZeroU128::new(3).unwrap();

/// A longer round for the window-extension test, so the margin on either side
/// of its assertion is more than half a second rather than a scheduling
/// artefact.
const LONG_ROUND: NonZeroU64 = NonZeroU64::new(2).unwrap();
const LONG_WINDOW_IN_ROUNDS: NonZeroU128 = NonZeroU128::new(4).unwrap();

fn window() -> Duration {
    Duration::from_secs(ROUND.get())
        * u32::try_from(WINDOW_IN_ROUNDS.get()).expect("The window fits in a `u32`.")
}

#[test(tokio::test)]
async fn a_connection_that_delivers_nothing_is_closed() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    // Only the node under observation runs a window short enough to run out
    // here.
    let mut silent_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_liveness(ROUND, WINDOW_IN_ROUNDS)
            .build()
    });

    listening_swarm.listen().with_memory_addr_external().await;
    silent_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;

    // One message, so the connection is a working one that then falls silent
    // rather than one that never carried anything.
    silent_swarm
        .behaviour_mut()
        .publish_message_with_validated_header_to_current_epoch(
            TestEncapsulatedMessage::new(b"one").as_ref(),
        )
        .unwrap();
    loop {
        select! {
            _ = silent_swarm.select_next_some() => {}
            listening_event = listening_swarm.select_next_some() => {
                if matches!(listening_event, SwarmEvent::Behaviour(Event::Message { .. })) {
                    break;
                }
            }
        }
    }

    // From here neither side sends anything, so the observation window runs out.
    let mut closed = false;
    let timeout = sleep(window() * 5);
    tokio::pin!(timeout);
    loop {
        select! {
            () = &mut timeout => break,
            listening_event = listening_swarm.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, .. } = listening_event
                    && peer_id == *silent_swarm.local_peer_id()
                {
                    closed = true;
                    break;
                }
            }
            _ = silent_swarm.select_next_some() => {}
        }
    }

    assert!(
        closed,
        "the observing node should have closed the connection once its window ran out"
    );
    assert!(
        !listening_swarm
            .behaviour()
            .negotiated_peers()
            .contains_key(silent_swarm.local_peer_id()),
        "a peer whose connection was closed should not still be negotiated"
    );
}

/// A neighbour that delivers keeps its place: the window is measured from its
/// last delivery, not from when the connection came up.
///
/// What the test arranges, in the observing node's rounds of `LONG_ROUND`
/// seconds against a window of `LONG_WINDOW_IN_ROUNDS`. The window is counted
/// in rounds spent connected, which here is every round, since the connection
/// is held throughout:
///
/// ```text
/// time     round   what happens
/// 0s       0       connection negotiated; on the grace alone it falls silent at round 4
/// 6s       3       a message is delivered, so the window now runs to round 7
/// 6s-10s   3-5     no close may be observed, and this spans round 4, where the grace ran out
/// 10s      5       still negotiated, three rounds short of the window it earned
/// ```
#[test(tokio::test)]
async fn a_delivered_message_extends_the_window() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut talking_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_liveness(LONG_ROUND, LONG_WINDOW_IN_ROUNDS)
            .build()
    });

    listening_swarm.listen().with_memory_addr_external().await;
    talking_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;

    // Round 3: far enough into the grace that the window this delivery earns
    // reaches well past where the grace alone would have run out.
    let deadline = sleep(Duration::from_secs(LONG_ROUND.get()) * 3);
    tokio::pin!(deadline);
    loop {
        select! {
            () = &mut deadline => break,
            _ = talking_swarm.select_next_some() => {}
            _ = listening_swarm.select_next_some() => {}
        }
    }
    talking_swarm
        .behaviour_mut()
        .publish_message_with_validated_header_to_current_epoch(
            TestEncapsulatedMessage::new(b"still here").as_ref(),
        )
        .unwrap();

    // Rounds 3 to 5: past round 4, where the grace would have run out, and well
    // short of round 7, where the window the delivery earned runs out.
    let deadline = sleep(Duration::from_secs(LONG_ROUND.get()) * 2);
    tokio::pin!(deadline);
    loop {
        select! {
            () = &mut deadline => break,
            listening_event = listening_swarm.select_next_some() => {
                assert!(
                    !matches!(listening_event, SwarmEvent::ConnectionClosed { .. }),
                    "a connection that delivered a message must not be closed on the grace window alone"
                );
            }
            _ = talking_swarm.select_next_some() => {}
        }
    }

    assert!(
        listening_swarm
            .behaviour()
            .negotiated_peers()
            .contains_key(talking_swarm.local_peer_id()),
        "the connection should still be negotiated"
    );
}

/// A node below the connections the protocol asks it to hold is one a
/// partition or an eclipse would be working towards, so the spec asks that the
/// period be recorded. Recording it means noticing when it starts and when it
/// ends, rather than restating it every round for as long as it lasts.
#[test(tokio::test)]
async fn the_period_below_the_target_degree_is_tracked_from_start_to_end() {
    let mut behaviour = BehaviourBuilder::new(&ed25519::Keypair::generate()).build();

    behaviour.check_and_report_low_peering_degree();
    let entered = behaviour
        .below_target_degree_since
        .expect("a node holding no connections at all is below the floor");

    // The period continues rather than starting again on every round that
    // passes within it.
    behaviour.current_round = behaviour.current_round.saturating_add(RoundCount::new(
        NonZeroU128::new(5).expect("must be non-zero"),
    ));
    behaviour.check_and_report_low_peering_degree();
    assert_eq!(
        behaviour.below_target_degree_since,
        Some(entered),
        "the period restarted instead of continuing"
    );

    // Enough live connections, of which enough are this node's own, ends it.
    let mut peering = BehaviourBuilder::new(&ed25519::Keypair::generate())
        .with_existing_connections(1, PEERING_DEGREE.get() - 2)
        .build();
    peering.check_and_report_low_peering_degree();
    assert_eq!(
        peering.below_target_degree_since, None,
        "a node holding what the protocol asks for was reported as below it"
    );
}
