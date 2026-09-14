use core::{num::NonZeroU64, time::Duration};
use std::time::Instant;

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

/// One message per round, so the rounds a batch takes to arrive are countable.
const SHARE: NonZeroU64 = NonZeroU64::new(1).unwrap();
const ROUND: Duration = Duration::from_secs(1);
const MESSAGES: u64 = 4;

#[test(tokio::test)]
async fn a_connection_is_read_no_faster_than_its_share() {
    let (mut identities, nodes) = new_nodes_with_empty_address(2);
    let mut sending_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id).with_membership(&nodes).build()
    });
    let mut listening_swarm = TestSwarm::new(&identities.next().unwrap(), |id| {
        BehaviourBuilder::new(id)
            .with_membership(&nodes)
            .with_connection_share_per_round(SHARE)
            .build()
    });

    listening_swarm.listen().with_memory_addr_external().await;
    sending_swarm
        .connect_and_wait_for_upgrade(&mut listening_swarm)
        .await;

    // Everything goes out at once, so what paces their arrival is the share the
    // listener reads the connection under, not the rate they were sent at.
    for nonce in 0..MESSAGES {
        sending_swarm
            .behaviour_mut()
            .publish_message_with_validated_header_to_current_epoch(
                TestEncapsulatedMessage::new_distinct(nonce, b"batched").as_ref(),
            )
            .unwrap();
    }

    let mut received = 0u64;
    let mut first_received_at = None;
    let mut last_received_at = None;
    let timeout = sleep(ROUND * u32::try_from(MESSAGES).unwrap() * 4);
    tokio::pin!(timeout);
    loop {
        select! {
            () = &mut timeout => break,
            _ = sending_swarm.select_next_some() => {}
            listening_event = listening_swarm.select_next_some() => {
                if matches!(listening_event, SwarmEvent::Behaviour(Event::Message { .. })) {
                    received += 1;
                    first_received_at.get_or_insert_with(Instant::now);
                    last_received_at = Some(Instant::now());
                    if received == MESSAGES {
                        break;
                    }
                }
            }
        }
    }

    assert_eq!(
        received, MESSAGES,
        "throttling a connection must delay its messages, not drop them"
    );

    // The first message arrives at some unknown point within a round, so only
    // the rounds *after* it are guaranteed to have been whole ones. Reading
    // `MESSAGES` of them one per round therefore takes at least `MESSAGES - 2`
    // full rounds from the first arrival.
    let spread = last_received_at
        .expect("a message was received")
        .duration_since(first_received_at.expect("a message was received"));
    let minimum = ROUND * u32::try_from(MESSAGES - 2).unwrap();
    assert!(
        spread >= minimum,
        "reading {MESSAGES} messages under a share of {SHARE} per round should have taken at \
         least {minimum:?} from the first, but they all arrived within {spread:?}"
    );
}
