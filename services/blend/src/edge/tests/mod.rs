use core::time::Duration;

use lb_blend::{
    proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs,
    scheduling::membership::Membership,
};
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
    membership::{MembershipInfo, chain::BlendEpochState},
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

fn test_blend_epoch_state(epoch: Epoch, membership: Membership<NodeId>) -> BlendEpochState<NodeId> {
    BlendEpochState {
        epoch,
        nonce: Fr::ZERO,
        aged: Fr::ZERO,
        lottery_0: Fr::ZERO,
        lottery_1: Fr::ZERO,
        membership_info: membership.into(),
    }
}

/// Two consecutive public epoch infos with no private in between (e.g. the
/// node had no winning slot in the first epoch). The handler must stay down
/// and the pending buffer must hold the latest public info.
#[test_log::test(tokio::test)]
async fn two_publics_without_private_in_between() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let (node_id_sender, _node_id_receiver) = tokio::sync::mpsc::channel(1);

    let settings = settings(local_node, 1, node_id_sender);
    let overwatch = overwatch_handle();

    let mut handler_state: Option<
        super::handlers::MessageHandler<TestBackend, NodeId, MockLeaderProofsGenerator, usize>,
    > = None;
    let mut pending_epoch_info: Option<PendingEpochInfo<NodeId>> = None;

    // First public: nothing buffered, nothing running -> buffer Public(1).
    super::handle_new_epoch_info(
        test_blend_epoch_state(Epoch::new(1), membership(&[core_node], local_node)),
        settings.clone(),
        &mut pending_epoch_info,
        &mut handler_state,
        overwatch.clone(),
    )
    .unwrap();

    assert!(
        handler_state.is_none(),
        "Handler must not be created without the private info"
    );
    let pending = pending_epoch_info
        .as_ref()
        .expect("Public info must be buffered");
    assert_eq!(pending.epoch, Epoch::new(1));
    assert!(matches!(pending.info_type, PendingEpochInfoType::Public(_)));

    // Second public for a later epoch with no private in between: stale public
    // gets overwritten by the new one, handler stays down.
    super::handle_new_epoch_info(
        test_blend_epoch_state(Epoch::new(2), membership(&[core_node], local_node)),
        settings,
        &mut pending_epoch_info,
        &mut handler_state,
        overwatch,
    )
    .unwrap();

    assert!(
        handler_state.is_none(),
        "Handler must remain down: no private info has been received"
    );
    let pending = pending_epoch_info
        .as_ref()
        .expect("Latest public info must be buffered");
    assert_eq!(pending.epoch, Epoch::new(2));
    assert!(matches!(pending.info_type, PendingEpochInfoType::Public(_)));
}

/// Public arrives first, then private for the same epoch: handler is created,
/// pending buffer is cleared.
#[test_log::test(tokio::test)]
async fn public_then_private_same_epoch_creates_handler() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let (node_id_sender, _node_id_receiver) = tokio::sync::mpsc::channel(1);

    let settings = settings(local_node, 1, node_id_sender);
    let overwatch = overwatch_handle();

    let mut handler_state: Option<
        super::handlers::MessageHandler<TestBackend, NodeId, MockLeaderProofsGenerator, usize>,
    > = None;
    let mut pending_epoch_info: Option<PendingEpochInfo<NodeId>> = None;

    super::handle_new_epoch_info(
        test_blend_epoch_state(Epoch::new(1), membership(&[core_node], local_node)),
        settings.clone(),
        &mut pending_epoch_info,
        &mut handler_state,
        overwatch.clone(),
    )
    .unwrap();
    assert!(handler_state.is_none());
    assert!(pending_epoch_info.is_some());

    super::handle_new_secret_epoch_info(
        test_pol_epoch_info(Epoch::new(1)),
        settings,
        &overwatch,
        &mut handler_state,
        &mut pending_epoch_info,
    );

    assert_eq!(
        handler_state
            .as_ref()
            .expect("Handler must be created")
            .epoch(),
        Epoch::new(1)
    );
    assert!(
        pending_epoch_info.is_none(),
        "Pending buffer must be consumed when the handler is created"
    );
}

/// Private arrives first, then public for the same epoch: handler is created,
/// pending buffer is cleared.
#[test_log::test(tokio::test)]
async fn private_then_public_same_epoch_creates_handler() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let (node_id_sender, _node_id_receiver) = tokio::sync::mpsc::channel(1);

    let settings = settings(local_node, 1, node_id_sender);
    let overwatch = overwatch_handle();

    let mut handler_state: Option<
        super::handlers::MessageHandler<TestBackend, NodeId, MockLeaderProofsGenerator, usize>,
    > = None;
    let mut pending_epoch_info: Option<PendingEpochInfo<NodeId>> = None;

    super::handle_new_secret_epoch_info(
        test_pol_epoch_info(Epoch::new(1)),
        settings.clone(),
        &overwatch,
        &mut handler_state,
        &mut pending_epoch_info,
    );
    assert!(handler_state.is_none());
    let pending = pending_epoch_info
        .as_ref()
        .expect("Private info must be buffered");
    assert_eq!(pending.epoch, Epoch::new(1));
    assert!(matches!(
        pending.info_type,
        PendingEpochInfoType::Private(_)
    ));

    super::handle_new_epoch_info(
        test_blend_epoch_state(Epoch::new(1), membership(&[core_node], local_node)),
        settings,
        &mut pending_epoch_info,
        &mut handler_state,
        overwatch,
    )
    .unwrap();

    assert_eq!(
        handler_state
            .as_ref()
            .expect("Handler must be created")
            .epoch(),
        Epoch::new(1)
    );
    assert!(
        pending_epoch_info.is_none(),
        "Pending buffer must be consumed when the handler is created"
    );
}
