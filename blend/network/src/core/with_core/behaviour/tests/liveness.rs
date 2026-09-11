use core::{
    num::{NonZeroU64, NonZeroU128},
    time::Duration,
};

use futures::StreamExt as _;
use lb_libp2p::SwarmEvent;
use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::{select, time::sleep};

use crate::core::{
    tests::utils::{TestEncapsulatedMessage, TestSwarm},
    with_core::behaviour::{
        Event,
        tests::utils::{BehaviourBuilder, SwarmExt as _, new_nodes_with_empty_address},
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
    // Both peers run the same rule, as they would in a real network, so both
    // sides of the connection let go of it.
    let mut silent_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_liveness(ROUND, WINDOW_IN_ROUNDS)
            .build()
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
        "the connection should have been closed once the observation window ran out"
    );
    assert!(
        !listening_swarm
            .behaviour()
            .negotiated_peers()
            .contains_key(silent_swarm.local_peer_id()),
        "a peer whose connection was closed should not still be negotiated"
    );
}

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

    // Deliver a message late in the window of grace the connection starts with,
    // so that the window it earns reaches well past the point the grace alone
    // would have run out.
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

    // Hold past the round the grace would have expired in, and well short of
    // the one the delivered message extends the window to.
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
