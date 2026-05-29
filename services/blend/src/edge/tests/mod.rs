use core::time::Duration;

use lb_blend::{
    proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs,
    scheduling::membership::Membership,
};
use lb_chain_service::Epoch;
use lb_core::crypto::ZkHash;
use lb_groth16::{Field as _, Fr};
use tokio::time::sleep;

use crate::{
    edge::{
        handlers::Error,
        tests::utils::{
            MockLeaderProofsGenerator, NodeId, TestBackend, overwatch_handle, settings, spawn_run,
        },
    },
    epoch_info::PolEpochInfo,
    membership::chain::BlendEpochState,
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
    let (_, epoch_sender, msg_sender, mut node_id_receiver) = spawn_run(
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

    // Send a new epoch with another core node 1.
    core_node = NodeId(1);
    epoch_sender
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
    let (join_handle, epoch_sender, _, _) = spawn_run(
        local_node,
        minimal_network_size,
        Some(membership(&[core_node], local_node)),
    )
    .await;

    // Send a new epoch with an empty membership (smaller than the min size).
    epoch_sender
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
    let (join_handle, epoch_sender, _, _) = spawn_run(
        local_node,
        minimal_network_size,
        Some(membership(&[core_node], local_node)),
    )
    .await;

    // Send a new epoch with a membership where the local node is core.
    epoch_sender
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

/// `handle_new_epoch_event` creates a new message handler with the provided
/// epoch's public and private inputs, and replaces it on the next epoch.
#[test_log::test(tokio::test)]
async fn handle_new_secret_epoch_info_recreates_handler() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let (node_id_sender, _node_id_receiver) = tokio::sync::mpsc::channel(1);

    let settings = settings(local_node, 1, node_id_sender);
    let overwatch = overwatch_handle();

    let mut handler_state: Option<
        super::handlers::MessageHandler<TestBackend, NodeId, MockLeaderProofsGenerator, usize>,
    > = None;

    // Public + secret for epoch 2 -> handler is created.
    let public_2 = test_blend_epoch_state(Epoch::new(2), membership(&[core_node], local_node));
    let secret_2 = test_pol_epoch_info(Epoch::new(2));
    super::handle_new_epoch_event(
        &public_2,
        Some(&secret_2),
        &mut handler_state,
        settings.clone(),
        overwatch.clone(),
    )
    .unwrap();
    assert!(
        handler_state.is_some(),
        "Handler should be created when public and secret info for the same epoch are present"
    );
    assert_eq!(handler_state.as_ref().unwrap().epoch(), Epoch::new(2));

    // Public + secret for epoch 3 -> handler is replaced.
    let public_3 = test_blend_epoch_state(Epoch::new(3), membership(&[core_node], local_node));
    let secret_3 = test_pol_epoch_info(Epoch::new(3));
    super::handle_new_epoch_event(
        &public_3,
        Some(&secret_3),
        &mut handler_state,
        settings,
        overwatch,
    )
    .unwrap();
    assert!(handler_state.is_some());
    assert_eq!(handler_state.as_ref().unwrap().epoch(), Epoch::new(3));
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
/// as long as no secret `PoL` info is available.
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

    // First public, no secret yet -> handler must stay down.
    super::handle_new_epoch_event(
        &test_blend_epoch_state(Epoch::new(1), membership(&[core_node], local_node)),
        None,
        &mut handler_state,
        settings.clone(),
        overwatch.clone(),
    )
    .unwrap();
    assert!(
        handler_state.is_none(),
        "Handler must not be created without the private info"
    );

    // Second public for a later epoch, still no secret -> handler stays down.
    super::handle_new_epoch_event(
        &test_blend_epoch_state(Epoch::new(2), membership(&[core_node], local_node)),
        None,
        &mut handler_state,
        settings,
        overwatch,
    )
    .unwrap();
    assert!(
        handler_state.is_none(),
        "Handler must remain down: no private info has been received"
    );
}

/// Public arrives first, then private for the same epoch: handler is created
/// on the second call once both sides line up on the same epoch.
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

    let public_1 = test_blend_epoch_state(Epoch::new(1), membership(&[core_node], local_node));

    // Public for epoch 1, no secret yet -> no handler.
    super::handle_new_epoch_event(
        &public_1,
        None,
        &mut handler_state,
        settings.clone(),
        overwatch.clone(),
    )
    .unwrap();
    assert!(handler_state.is_none());

    // Secret for epoch 1 arrives, same public -> handler is created.
    super::handle_new_epoch_event(
        &public_1,
        Some(&test_pol_epoch_info(Epoch::new(1))),
        &mut handler_state,
        settings,
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
}

/// Secret arrives for an epoch ahead of the current public (mismatch), then
/// public catches up to the same epoch: handler is created on the match.
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

    // Secret for epoch 1 while public is still on epoch 0 -> epochs mismatch,
    // no handler.
    let secret_1 = test_pol_epoch_info(Epoch::new(1));
    super::handle_new_epoch_event(
        &test_blend_epoch_state(Epoch::new(0), membership(&[core_node], local_node)),
        Some(&secret_1),
        &mut handler_state,
        settings.clone(),
        overwatch.clone(),
    )
    .unwrap();
    assert!(handler_state.is_none());

    // Public catches up to epoch 1 -> handler is created.
    super::handle_new_epoch_event(
        &test_blend_epoch_state(Epoch::new(1), membership(&[core_node], local_node)),
        Some(&secret_1),
        &mut handler_state,
        settings,
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
}
