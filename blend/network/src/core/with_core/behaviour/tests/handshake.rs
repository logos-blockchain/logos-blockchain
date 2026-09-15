use core::{num::NonZeroU128, time::Duration};

use futures::StreamExt as _;
use lb_blend_primitives::time::RoundCount;
use lb_libp2p::SwarmEvent;
use test_log::test;
use tokio::time::timeout;

use crate::core::{
    tests::utils::TestSwarm,
    with_core::behaviour::{ConnectionUpgradeFailureReason, Event, tests::utils::BehaviourBuilder},
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
