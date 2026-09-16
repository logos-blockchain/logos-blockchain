use core::{num::NonZeroU64, time::Duration};

use futures::StreamExt as _;
use lb_libp2p::SwarmEvent;
use libp2p::{
    PeerId,
    identity::{PublicKey, ed25519},
};
use libp2p_stream::Behaviour as StreamBehaviour;
use libp2p_swarm_test::SwarmExt as _;
use test_log::test;
use tokio::select;

use crate::core::{
    tests::utils::{TestSwarm, drive_for},
    with_edge::behaviour::tests::utils::{
        BehaviourBuilder, StreamBehaviourExt as _, TestBehaviour,
    },
};

const ONE_PER_ROUND: NonZeroU64 = NonZeroU64::new(1).unwrap();
/// Long enough that a test spends its whole life inside one round, so what it
/// observes is the share running out and never the round turning over.
const LONG_ROUND: NonZeroU64 = NonZeroU64::new(600).unwrap();
const SHORT_ROUND: NonZeroU64 = NonZeroU64::new(1).unwrap();

fn core_node(round_duration_in_seconds: NonZeroU64) -> TestSwarm<TestBehaviour> {
    TestSwarm::new_ephemeral(|_| {
        // A random peer in the membership is what makes the swarms that connect
        // to this one edge nodes rather than core ones.
        BehaviourBuilder::new(PeerId::random())
            .with_accepts_per_round(ONE_PER_ROUND)
            .with_round_duration_in_seconds(round_duration_in_seconds)
            // Longer than any of these tests, so nothing is closed for having
            // failed to send its message in `T_E`.
            .with_timeout(Duration::from_secs(600))
            .build()
    })
}

/// Drives both swarms until the core node closes the edge node's connection.
async fn wait_until_closed(
    edge: &mut TestSwarm<StreamBehaviour>,
    core: &mut TestSwarm<TestBehaviour>,
) {
    loop {
        select! {
            _ = edge.select_next_some() => {}
            core_event = core.select_next_some() => {
                if let SwarmEvent::ConnectionClosed { peer_id, endpoint, .. } = core_event
                    && peer_id == *edge.local_peer_id()
                {
                    assert!(endpoint.is_listener());
                    return;
                }
            }
        }
    }
}

/// `Φ_CE^Max` bounds how many edge connections are open at once; `r_E` bounds
/// how fast new ones arrive. This is the second bound: with room to spare under
/// the first, a node still takes only its round's allowance.
#[test(tokio::test)]
async fn an_edge_connection_offered_above_the_round_share_is_refused() {
    let mut first = TestSwarm::new_ephemeral(|_| StreamBehaviour::new());
    let mut second = TestSwarm::new_ephemeral(|_| StreamBehaviour::new());
    let mut core = core_node(LONG_ROUND);

    core.listen().with_memory_addr_external().await;

    // Holding the stream keeps the first connection from being dropped, so the
    // second is refused for the share and not for the maximum.
    let _stream = first.connect_and_upgrade_to_blend(&mut core).await;

    second.connect(&mut core).await;
    wait_until_closed(&mut second, &mut core).await;
}

/// A connection this node was never going to serve must not take an edge
/// node's place in the round. Core peers dial in bursts — every one of them at
/// an epoch transition — and each is refused here as a core node; spending the
/// edge allowance on the way out would turn genuine edge nodes away for a round
/// every time that happens.
#[test(tokio::test)]
async fn a_refused_core_peer_does_not_spend_the_edge_allowance() {
    // The swarm must carry the identity the membership below names, so it has
    // to be built from a keypair rather than an ephemeral one.
    let core_peer_identity = ed25519::Keypair::generate();
    let core_peer_id = PeerId::from(PublicKey::from(core_peer_identity.public()));
    let mut core_peer = TestSwarm::new(&core_peer_identity, |_| StreamBehaviour::new());
    let mut edge = TestSwarm::new_ephemeral(|_| StreamBehaviour::new());
    // The node under test knows `core_peer` as a core node, so its connection is
    // refused as such.
    let mut core = TestSwarm::new_ephemeral(|_| {
        BehaviourBuilder::new(core_peer_id)
            .with_accepts_per_round(ONE_PER_ROUND)
            .with_round_duration_in_seconds(LONG_ROUND)
            .with_timeout(Duration::from_secs(600))
            .build()
    });

    core.listen().with_memory_addr_external().await;

    // The core peer is turned away, inside the same round.
    core_peer.connect(&mut core).await;
    wait_until_closed(&mut core_peer, &mut core).await;

    // The round's one allowance must still be there for an edge node. This
    // hangs, and the test times out, if the refusal above consumed it.
    let _stream = edge.connect_and_upgrade_to_blend(&mut core).await;
}

/// And the allowance is per round, not a total: the next round admits again.
#[test(tokio::test)]
async fn the_accept_share_refills_with_the_round() {
    let mut first = TestSwarm::new_ephemeral(|_| StreamBehaviour::new());
    let mut second = TestSwarm::new_ephemeral(|_| StreamBehaviour::new());
    let mut core = core_node(SHORT_ROUND);

    core.listen().with_memory_addr_external().await;
    let _stream = first.connect_and_upgrade_to_blend(&mut core).await;

    drive_for(
        &mut second,
        &mut core,
        Duration::from_secs(2 * SHORT_ROUND.get()),
    )
    .await;

    // Upgrading at all is the assertion: this hangs, and the test times out, if
    // the share were spent for good rather than for a round.
    let _second_stream = second.connect_and_upgrade_to_blend(&mut core).await;
}
