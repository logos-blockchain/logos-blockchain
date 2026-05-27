use core::time::Duration;

use lb_blend::proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs;
use lb_chain_service::Epoch;
use lb_core::{crypto::ZkHash, proofs::leader_proof::LeaderPublic};
use lb_groth16::{Field as _, Fr};
use tokio::time::sleep;

use crate::{
    edge::{
        PendingEpochInfo, PendingEpochInfoType,
        handlers::Error,
        tests::utils::{
            MockLeaderProofsGenerator, NodeId, TestBackend, overwatch_handle, settings, spawn_run,
        },
    },
    epoch_info::PolEpochInfo,
    membership::MembershipInfo,
    test_utils::membership::membership,
};

pub mod utils;

/// [`run`] forwards messages to the core nodes in the updated membership.
#[test_log::test(tokio::test)]
#[ignore = "We need a different test setup since we are not blocking the edge tokio task until the secret PoL info is fetched, which makes this test flaky."]
async fn run_with_epoch_transition() {
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

/// [`run`] shuts down gracefully if a new membership is smaller than the
/// minimum network size.
#[test_log::test(tokio::test)]
async fn run_shuts_down_if_new_membership_is_small() {
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
    assert!(matches!(join_handle.await.unwrap(), Ok(())));
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

fn test_pol_epoch_info(epoch: Epoch) -> PolEpochInfo {
    PolEpochInfo {
        epoch,
        poq_public_inputs: LeaderPublic {
            slot: 1,
            latest_root: Fr::ZERO,
            lottery_0: Fr::ZERO,
            lottery_1: Fr::ZERO,
            epoch_nonce: ZkHash::ZERO,
            aged_root: ZkHash::ZERO,
        },
        poq_private_inputs: ProofOfLeadershipQuotaInputs {
            slot: 1,
            note_value: 1,
            transaction_hash: ZkHash::ZERO,
            output_number: 1,
            aged_path_and_selectors: [(ZkHash::ZERO, false); _],
            secret_key: ZkHash::ZERO,
        },
    }
}

/// `handle_new_secret_epoch_info` creates a new message handler with the
/// provided epoch's public and private inputs.
#[test_log::test(tokio::test)]
async fn handle_new_secret_epoch_info_recreates_handler() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let (node_id_sender, _node_id_receiver) = tokio::sync::mpsc::channel(1);

    let edge_membership = membership(&[core_node], local_node);
    let membership_info = MembershipInfo::from(edge_membership);

    let settings = settings(local_node, 1, node_id_sender);
    let overwatch = overwatch_handle();

    // Start with no handler (e.g. after an epoch transition shut it down).
    let mut handler_state: Option<
        super::handlers::MessageHandler<TestBackend, NodeId, MockLeaderProofsGenerator, usize>,
    > = None;
    let mut buffered_epoch_info = Some(PendingEpochInfo {
        epoch: Epoch::new(2),
        info_type: PendingEpochInfoType::Public(Box::new(membership_info.clone())),
    });

    // Provide secret PoL info for epoch 2.
    let new_pol_info = test_pol_epoch_info(Epoch::new(2));
    super::handle_new_secret_epoch_info(
        new_pol_info,
        settings.clone(),
        &overwatch,
        &mut handler_state,
        &mut buffered_epoch_info,
    );
    assert!(
        handler_state.is_some(),
        "Handler should be created after secret PoL info is provided"
    );
    assert_eq!(handler_state.as_ref().unwrap().epoch(), Epoch::new(2));
    assert!(
        buffered_epoch_info.is_none(),
        "Buffered epoch info should be consumed when handler is created"
    );

    handler_state = None; // Simulate handler shutdown after an epoch transition.
    buffered_epoch_info = Some(PendingEpochInfo {
        epoch: Epoch::new(3),
        info_type: PendingEpochInfoType::Public(Box::new(membership_info)),
    });
    // Provide secret PoL info for epoch 3 - handler should be replaced.
    let newer_pol_info = test_pol_epoch_info(Epoch::new(3));
    super::handle_new_secret_epoch_info(
        newer_pol_info,
        settings,
        &overwatch,
        &mut handler_state,
        &mut buffered_epoch_info,
    );
    assert!(handler_state.is_some());
    assert_eq!(handler_state.as_ref().unwrap().epoch(), Epoch::new(3));
    assert!(
        buffered_epoch_info.is_none(),
        "Buffered epoch info should be consumed when handler is created"
    );
}
