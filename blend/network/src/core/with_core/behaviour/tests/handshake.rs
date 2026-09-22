use core::{num::NonZeroU128, time::Duration};

use futures::StreamExt as _;
use lb_blend_primitives::time::RoundCount;
use lb_libp2p::SwarmEvent;
use libp2p::{
    Multiaddr,
    swarm::{ConnectionId, NetworkBehaviour as _},
};
use test_log::test;
use tokio::time::timeout;

use crate::core::{
    tests::utils::TestSwarm,
    with_core::behaviour::{
        ConnectionUpgradeFailureReason, Event,
        tests::utils::{
            BehaviourBuilder, PEERING_DEGREE, maximum_accepted_peers, new_nodes_with_empty_address,
        },
    },
};

const HANDSHAKE_DEADLINE_IN_ROUNDS: RoundCount = RoundCount::new(NonZeroU128::new(2).unwrap());

/// `T_H`: a handshake in progress holds a degree slot, so one that never
/// completes would hold it for the rest of the epoch. A peer that connects and
/// then neither speaks nor hangs up could take a slot each, and cost this node
/// its peering degree without ever having to become a neighbour.
#[test(tokio::test)]
async fn a_handshake_that_does_not_complete_within_its_deadline_is_abandoned() {
    let mut core = TestSwarm::new_ephemeral(|id| {
        BehaviourBuilder::new(id)
            .with_handshake_deadline_in_rounds(HANDSHAKE_DEADLINE_IN_ROUNDS)
            .with_handshakes_in_progress(1)
            .build()
    });

    let slots_while_pending = core.behaviour().available_connection_slots();

    let abandoned = timeout(Duration::from_secs(10), async {
        loop {
            if let SwarmEvent::Behaviour(Event::InboundConnectionUpgradeFailed { reason, .. }) =
                core.select_next_some().await
            {
                return reason;
            }
        }
    })
    .await
    .expect("a handshake must be given up on, not waited on for the rest of the epoch");

    assert!(matches!(
        abandoned,
        ConnectionUpgradeFailureReason::HandshakeTimedOut
    ));
    assert_eq!(
        core.behaviour().available_connection_slots(),
        slots_while_pending + 1,
        "and the degree slot it was holding must be handed back"
    );
}

/// A handshake in progress holds a degree slot, so an inbound one has to count
/// against the share of slots the node lets other nodes fill, not just against
/// the total.
///
/// Counting it only against the total is what lets a handful of peers shut a
/// node in: they offer connections, stall before completing the upgrade, and
/// the node reaches its maximum without a single negotiated neighbour. It then
/// opens nothing itself, because there is no slot left to open into, and the
/// floor of `Φ_CC - 2` connections it is supposed to have dialed is never met.
/// `T_H` hands each slot back, but the same peers take them again.
#[test(tokio::test)]
async fn pending_inbound_handshakes_count_against_what_the_node_accepts() {
    let filled = TestSwarm::new_ephemeral(|id| {
        BehaviourBuilder::new(id)
            .with_handshakes_in_progress(maximum_accepted_peers())
            .build()
    });

    assert!(
        !filled.behaviour().can_accept_connection(),
        "the node accepted a connection beyond the share it lets others fill"
    );
    assert_eq!(
        filled.behaviour().connections_to_open(),
        PEERING_DEGREE.get() - 2,
        "peers stalling handshakes left the node with nothing to dial into"
    );

    let with_room = TestSwarm::new_ephemeral(|id| {
        BehaviourBuilder::new(id)
            .with_handshakes_in_progress(maximum_accepted_peers() - 1)
            .build()
    });
    assert!(
        with_room.behaviour().can_accept_connection(),
        "a node below its accepted share must still take a connection"
    );
}

/// A peer part way through a handshake is already a neighbour as far as the
/// degree rule is concerned, so the node must not also draw it at random and
/// dial it.
///
/// Dialing it opens a second connection, which holds a second degree slot
/// until the two are resolved against each other by comparing identities. The
/// swarm decides who to dial, so what the behaviour owes it is the list.
#[test(tokio::test)]
async fn a_peer_part_way_through_a_handshake_is_reported_as_one_to_leave_alone() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let local_identity = identities.next().unwrap();
    let peer_id = nodes[1].id;

    let mut behaviour = BehaviourBuilder::new(&local_identity)
        .with_membership(&nodes)
        .build();

    assert_eq!(behaviour.peers_with_handshake_in_progress().count(), 0);

    let addr = Multiaddr::empty();
    let _handler = behaviour
        .handle_established_inbound_connection(
            ConnectionId::new_unchecked(0),
            peer_id,
            &addr,
            &addr,
        )
        .expect("an inbound connection with a core peer is accepted");

    assert!(
        behaviour
            .peers_with_handshake_in_progress()
            .any(|pending| *pending == peer_id),
        "a peer being accepted was left open to being dialed as well"
    );
}
