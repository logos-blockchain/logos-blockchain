use std::time::Duration;

use tokio::time::sleep;

use crate::{
    edge::{
        handlers::Error,
        tests::utils::{NodeId, resume_panic_from, spawn_run},
    },
    test_utils::membership::membership,
};

pub mod utils;

/// [`run`] forwards messages to the core nodes in the updated membership.
#[test_log::test(tokio::test)]
async fn run_with_session_transition() {
    let local_node = NodeId(99);
    let mut core_node = NodeId(0);
    let minimal_network_size = 1;
    let (_, session_sender, msg_sender, mut node_id_receiver) = spawn_run(
        local_node,
        minimal_network_size,
        Some(membership(&[core_node], local_node)),
    )
    .await;

    // A message should be forwarded to the core node 0.
    msg_sender.send(vec![0]).await.expect("channel opened");
    assert_eq!(
        node_id_receiver.recv().await.expect("channel opened"),
        core_node
    );

    // Send a new session with another core node 1.
    core_node = NodeId(1);
    session_sender
        .send(membership(&[core_node], local_node))
        .await
        .expect("channel opened");
    sleep(Duration::from_millis(100)).await;

    // A message should be forwarded to the core node 1.
    msg_sender.send(vec![0]).await.expect("channel opened");
    assert_eq!(
        node_id_receiver.recv().await.expect("channel opened"),
        core_node
    );
}

/// [`run`] panics if the initial membership is too small.
#[test_log::test(tokio::test)]
#[should_panic(
    expected = "The initial membership should satisfy the edge node condition: NetworkIsTooSmall"
)]
async fn run_panics_with_small_initial_membership() {
    let local_node = NodeId(99);
    let core_nodes = [NodeId(0)];
    let minimal_network_size = 2;
    let (join_handle, _, _, _) = spawn_run(
        local_node,
        minimal_network_size,
        Some(membership(&core_nodes, local_node)),
    )
    .await;

    resume_panic_from(join_handle).await;
}

/// [`run`] panics if the local node is not edge in the initial membership.
#[test_log::test(tokio::test)]
#[should_panic(
    expected = "The initial membership should satisfy the edge node condition: LocalIsCoreNode"
)]
async fn run_panics_with_local_is_core_in_initial_membership() {
    let local_node = NodeId(99);
    let core_nodes = [local_node];
    let minimal_network_size = 1;
    let (join_handle, _, _, _) = spawn_run(
        local_node,
        minimal_network_size,
        Some(membership(&core_nodes, local_node)),
    )
    .await;

    resume_panic_from(join_handle).await;
}

/// [`run`] fails if a new membership is smaller than the minimum network
/// size.
#[test_log::test(tokio::test)]
async fn run_fails_if_new_membership_is_small() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let minimal_network_size = 1;
    let (join_handle, session_sender, _, _) = spawn_run(
        local_node,
        minimal_network_size,
        Some(membership(&[core_node], local_node)),
    )
    .await;

    // Send a new session with an empty membership (smaller than the min size).
    session_sender
        .send(membership(&[], local_node))
        .await
        .expect("channel opened");
    assert!(matches!(
        join_handle.await.unwrap(),
        Err(Error::NetworkIsTooSmall(0))
    ));
}

/// [`run`] fails if the local node is not edge in a new membership.
#[test_log::test(tokio::test)]
async fn run_fails_if_local_is_core_in_new_membership() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let minimal_network_size = 1;
    let (join_handle, session_sender, _, _) = spawn_run(
        local_node,
        minimal_network_size,
        Some(membership(&[core_node], local_node)),
    )
    .await;

    // Send a new session with a membership where the local node is core.
    session_sender
        .send(membership(&[local_node], local_node))
        .await
        .expect("channel opened");
    assert!(matches!(
        join_handle.await.unwrap(),
        Err(Error::LocalIsCoreNode)
    ));
}
