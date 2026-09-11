//! The list of peers blocked for spamming, which the behaviour owns: what a
//! block denies, what it leaves alone, and how long it lasts.

use core::time::Duration;

use futures::StreamExt as _;
use lb_blend_membership::Membership;
use lb_blend_message::serialize_encapsulated_message_with_verified_public_header;
use lb_cryptarchia_engine::Epoch;
use lb_libp2p::SwarmEvent;
use libp2p::swarm::dial_opts::{DialOpts, PeerCondition};
use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::{select, time::sleep};

use crate::core::{
    tests::utils::{TestEncapsulatedMessageWithEpoch, TestProofsVerifier, TestSwarm},
    with_core::behaviour::{
        Event, MAX_BLOCK_DURATION_IN_EPOCHS, SpamReason,
        tests::utils::{
            BehaviourBuilder, SwarmExt as _, build_memberships, new_nodes_with_empty_address,
        },
    },
};

/// A first strike blocks until the next epoch, each further strike adds an
/// epoch up to the cap, an expired block is lifted at the epoch transition, and
/// strikes are forgotten once the peer is neither blocked nor a member.
#[test(tokio::test)]
async fn block_duration_escalates_per_strike_and_expires_with_epochs() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let local_identity = identities.next().unwrap();
    let remote_peer = nodes[1].id;
    // The behaviour starts at epoch 0.
    let mut behaviour = BehaviourBuilder::new(&local_identity)
        .with_membership(&nodes)
        .build();
    let membership = Membership::new_without_local(&nodes);

    behaviour.block_peer(remote_peer, SpamReason::InvalidProofOfQuota);
    assert_eq!(behaviour.blocked_peers.get(&remote_peer), Some(&1.into()));
    assert!(behaviour.is_blocked(&remote_peer));

    // Lifted at epoch 1, but the strike is remembered while the peer is a member.
    behaviour.start_new_epoch(
        (membership.clone(), 1.into()),
        TestProofsVerifier::accepting(),
    );
    assert!(!behaviour.is_blocked(&remote_peer));
    assert_eq!(behaviour.spam_strikes.get(&remote_peer), Some(&1));

    // Second strike: two epochs, so still blocked at epoch 2 and lifted at 3.
    behaviour.block_peer(remote_peer, SpamReason::DuplicateMessage);
    assert_eq!(behaviour.blocked_peers.get(&remote_peer), Some(&3.into()));
    behaviour.start_new_epoch(
        (membership.clone(), 2.into()),
        TestProofsVerifier::accepting(),
    );
    assert!(behaviour.is_blocked(&remote_peer));
    behaviour.start_new_epoch((membership, 3.into()), TestProofsVerifier::accepting());
    assert!(!behaviour.is_blocked(&remote_peer));

    // The escalation is capped.
    for _ in 0..(MAX_BLOCK_DURATION_IN_EPOCHS + 2) {
        behaviour.block_peer(remote_peer, SpamReason::UndeserializableMessage);
    }
    assert_eq!(
        behaviour.blocked_peers.get(&remote_peer),
        Some(&(3 + MAX_BLOCK_DURATION_IN_EPOCHS).into())
    );

    // A peer that leaves the membership while blocked stays blocked until its
    // block expires, and its strikes are forgotten once it is neither.
    let empty_membership = Membership::new_without_local(&[]);
    behaviour.start_new_epoch(
        (empty_membership.clone(), 4.into()),
        TestProofsVerifier::accepting(),
    );
    assert!(behaviour.is_blocked(&remote_peer));
    assert!(behaviour.spam_strikes.contains_key(&remote_peer));
    behaviour.start_new_epoch(
        (empty_membership, (3 + MAX_BLOCK_DURATION_IN_EPOCHS).into()),
        TestProofsVerifier::accepting(),
    );
    assert!(!behaviour.is_blocked(&remote_peer));
    assert!(!behaviour.spam_strikes.contains_key(&remote_peer));
    assert_eq!(behaviour.num_blocked_peers(), 0);
}

/// A peer caught spamming is blocked when the verdict is issued, its new
/// connections are denied in the same epoch, and it connects again once its
/// block has expired at the next epoch.
#[expect(clippy::too_many_lines, reason = "Test function.")]
#[test(tokio::test)]
async fn blocked_peer_is_denied_until_its_block_expires() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialing_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let dialing_peer_id = *dialing_swarm.local_peer_id();
    let listening_peer_id = *listening_swarm.local_peer_id();

    listening_swarm.listen().with_memory_addr_external().await;
    dialing_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;

    // Garbage over the current-epoch connection: a spammy verdict.
    dialing_swarm
        .behaviour_mut()
        .force_send_serialized_message_to_current_epoch_peer(b"garbage".to_vec(), listening_peer_id)
        .unwrap();

    let mut blocked_event = false;
    let mut connection_closed = false;
    loop {
        select! {
            () = sleep(Duration::from_secs(10)) => { break; }
            _ = dialing_swarm.select_next_some() => {}
            event = listening_swarm.select_next_some() => {
                match event {
                    SwarmEvent::Behaviour(Event::PeerBlocked { peer_id, reason, until_epoch }) => {
                        assert_eq!(peer_id, dialing_peer_id);
                        assert_eq!(reason, SpamReason::UndeserializableMessage);
                        // Behaviours start at epoch 0: a first strike lasts one epoch.
                        assert_eq!(until_epoch, Epoch::from(1));
                        blocked_event = true;
                    }
                    SwarmEvent::ConnectionClosed { peer_id, .. } if peer_id == dialing_peer_id => {
                        connection_closed = true;
                    }
                    _ => {}
                }
            }
        }
        if blocked_event && connection_closed {
            break;
        }
    }
    assert!(blocked_event, "a spammy verdict must block the peer");
    assert!(connection_closed, "the spammy connection must be closed");
    assert!(listening_swarm.behaviour().is_blocked(&dialing_peer_id));
    assert_eq!(listening_swarm.behaviour().num_blocked_peers(), 1);

    // The blocked peer dials again: the connection is denied before any
    // upgrade, and the dialer sees it closed.
    let listening_address = listening_swarm
        .external_addresses()
        .next()
        .cloned()
        .expect("listening swarm must have an external address");
    dialing_swarm
        .dial(
            DialOpts::peer_id(listening_peer_id)
                .addresses(vec![listening_address.clone()])
                .condition(PeerCondition::Always)
                .build(),
        )
        .unwrap();

    let mut denied = false;
    let mut closed_on_dialer = false;
    loop {
        select! {
            () = sleep(Duration::from_secs(10)) => { break; }
            event = dialing_swarm.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, .. } = event && peer_id == listening_peer_id {
                    closed_on_dialer = true;
                }
            }
            event = listening_swarm.select_next_some() => {
                match event {
                    SwarmEvent::IncomingConnectionError { .. } => {
                        denied = true;
                    }
                    SwarmEvent::Behaviour(Event::InboundConnectionUpgradeSucceeded(peer_id)) => {
                        panic!("connection with blocked peer {peer_id:?} must not be upgraded");
                    }
                    _ => {}
                }
            }
        }
        if denied && closed_on_dialer {
            break;
        }
    }
    assert!(
        denied,
        "the listening swarm must deny the blocked peer's connection"
    );
    assert!(
        closed_on_dialer,
        "the blocked peer must see its connection closed"
    );
    assert!(listening_swarm.behaviour().negotiated_peers.is_empty());

    // The next epoch lifts the block, and the peer connects again.
    let memberships = build_memberships(&[&dialing_swarm, &listening_swarm]);
    listening_swarm.behaviour_mut().start_new_epoch(
        (memberships[1].clone(), 1.into()),
        TestProofsVerifier::accepting(),
    );
    assert!(!listening_swarm.behaviour().is_blocked(&dialing_peer_id));

    let mut unblocked_event = false;
    loop {
        select! {
            () = sleep(Duration::from_secs(5)) => { break; }
            _ = dialing_swarm.select_next_some() => {}
            event = listening_swarm.select_next_some() => {
                if let SwarmEvent::Behaviour(Event::PeerUnblocked(peer_id)) = event {
                    assert_eq!(peer_id, dialing_peer_id);
                    unblocked_event = true;
                    break;
                }
            }
        }
    }
    assert!(
        unblocked_event,
        "an expired block must be reported to the swarm"
    );

    tokio::time::timeout(
        Duration::from_secs(10),
        dialing_swarm.connect_and_wait_for_upgrade(&mut listening_swarm),
    )
    .await
    .expect("a peer whose block expired must be able to connect again");
    assert!(
        listening_swarm
            .behaviour()
            .negotiated_peers
            .contains_key(&dialing_peer_id)
    );
}

/// During the transition period a node holds two connections with the same
/// peer. A spammy verdict on the new-epoch one closes that connection only:
/// the old-epoch connection keeps carrying, and the node keeps processing, the
/// previous epoch's messages until the transition period ends.
#[expect(clippy::too_many_lines, reason = "Test function.")]
#[test(tokio::test)]
async fn verdict_on_new_epoch_connection_leaves_old_epoch_connection_open() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut dialing_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let dialing_peer_id = *dialing_swarm.local_peer_id();
    let listening_peer_id = *listening_swarm.local_peer_id();

    listening_swarm.listen().with_memory_addr_external().await;
    dialing_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;
    let old_epoch_connection = listening_swarm
        .behaviour()
        .negotiated_peers
        .get(&dialing_peer_id)
        .unwrap()
        .connection_id;

    // Both sides move to epoch 1; the connection above becomes old-epoch on
    // both, and a new one is opened for the new epoch.
    let memberships = build_memberships(&[&dialing_swarm, &listening_swarm]);
    dialing_swarm.behaviour_mut().start_new_epoch(
        (memberships[0].clone(), 1.into()),
        TestProofsVerifier::accepting(),
    );
    listening_swarm.behaviour_mut().start_new_epoch(
        (memberships[1].clone(), 1.into()),
        TestProofsVerifier::accepting(),
    );
    dialing_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;
    let new_epoch_connection = listening_swarm
        .behaviour()
        .negotiated_peers
        .get(&dialing_peer_id)
        .unwrap()
        .connection_id;
    assert_ne!(old_epoch_connection, new_epoch_connection);
    assert!(
        listening_swarm
            .behaviour()
            .old_epoch_peer_ids()
            .unwrap()
            .any(|peer_id| *peer_id == dialing_peer_id)
    );

    // Garbage over the new-epoch connection: the peer is blocked and that
    // connection is closed.
    dialing_swarm
        .behaviour_mut()
        .force_send_serialized_message_to_current_epoch_peer(b"garbage".to_vec(), listening_peer_id)
        .unwrap();

    let mut new_epoch_connection_closed = false;
    loop {
        select! {
            () = sleep(Duration::from_secs(10)) => { break; }
            _ = dialing_swarm.select_next_some() => {}
            event = listening_swarm.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, connection_id, .. } = event && peer_id == dialing_peer_id {
                    assert_eq!(connection_id, new_epoch_connection, "only the offending connection may be closed");
                    new_epoch_connection_closed = true;
                    break;
                }
            }
        }
    }
    assert!(new_epoch_connection_closed);
    assert!(listening_swarm.behaviour().is_blocked(&dialing_peer_id));
    assert!(
        listening_swarm
            .behaviour()
            .old_epoch_peer_ids()
            .unwrap()
            .any(|peer_id| *peer_id == dialing_peer_id),
        "the old-epoch connection must survive the verdict"
    );

    // An old-epoch message from the blocked peer is still delivered over the
    // old-epoch connection.
    let old_epoch_message = TestEncapsulatedMessageWithEpoch::new(0.into(), b"last-epoch");
    dialing_swarm
        .behaviour_mut()
        .force_send_serialized_message_to_peer_at_epoch(
            serialize_encapsulated_message_with_verified_public_header(&old_epoch_message),
            listening_peer_id,
            0.into(),
        )
        .unwrap();

    let mut old_epoch_message_delivered = false;
    loop {
        select! {
            () = sleep(Duration::from_secs(10)) => { break; }
            _ = dialing_swarm.select_next_some() => {}
            event = listening_swarm.select_next_some() => {
                match event {
                    SwarmEvent::Behaviour(Event::Message { sender, epoch, .. }) => {
                        assert_eq!(sender, dialing_peer_id);
                        assert_eq!(epoch, Epoch::from(0));
                        old_epoch_message_delivered = true;
                        break;
                    }
                    SwarmEvent::ConnectionClosed { peer_id, connection_id, .. } if peer_id == dialing_peer_id => {
                        panic!("old-epoch connection {connection_id:?} must not be closed by a verdict on another connection");
                    }
                    _ => {}
                }
            }
        }
    }
    assert!(
        old_epoch_message_delivered,
        "messages of the previous epoch must keep flowing over the old-epoch connection"
    );
}
