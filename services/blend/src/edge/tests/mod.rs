use core::time::Duration;

use lb_blend::{
    message::crypto::proofs::PoQVerificationInputsMinusSigningKey,
    proofs::quota::inputs::prove::{
        private::ProofOfLeadershipQuotaInputs,
        public::{CoreInputs, LeaderInputs},
    },
};
use lb_chain_service::Epoch;
use lb_core::{crypto::ZkHash, proofs::leader_proof::LeaderPublic};
use lb_groth16::{Field as _, Fr};
use lb_time_service::SlotTick;
use tokio::time::sleep;

use crate::{
    edge::{
        handlers::Error,
        tests::utils::{
            MockLeaderProofsGenerator, NodeId, TestBackend, overwatch_handle, settings, spawn_run,
        },
    },
    epoch_info::{EpochHandler, PolEpochInfo},
    membership::MembershipInfo,
    test_utils::{epoch::TestChainService, membership::membership},
};

pub mod utils;

/// [`run`] forwards messages to the core nodes in the updated membership.
#[test_log::test(tokio::test)]
#[ignore = "We need a different test setup since we are not blocking the edge tokio task until the secret PoL info is fetched, which makes this test flaky."]
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

fn poq_verification_inputs() -> PoQVerificationInputsMinusSigningKey {
    PoQVerificationInputsMinusSigningKey {
        leader: LeaderInputs {
            pol_ledger_aged: ZkHash::ZERO,
            pol_epoch_nonce: ZkHash::ZERO,
            message_quota: 10,
            lottery_0: Fr::ZERO,
            lottery_1: Fr::ZERO,
        },
        core: CoreInputs {
            quota: 1,
            zk_root: Fr::ZERO,
        },
        session: 1,
    }
}

/// `handle_clock_event` shuts down the message handler when a new epoch is
/// detected ahead of the current handler's epoch.
#[test_log::test(tokio::test)]
async fn handle_clock_event_new_epoch_shuts_down_handler() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let (node_id_sender, _node_id_receiver) = tokio::sync::mpsc::channel(1);

    let edge_membership = membership(&[core_node], local_node);
    let pol_info = test_pol_epoch_info(Epoch::new(1));

    let handler = super::handlers::MessageHandler::<
        TestBackend,
        NodeId,
        MockLeaderProofsGenerator,
        usize,
    >::try_new_with_edge_condition_check(
        settings(local_node, 1, node_id_sender),
        edge_membership,
        poq_verification_inputs(),
        pol_info.poq_private_inputs.clone(),
        overwatch_handle(),
        Epoch::new(1),
    )
    .unwrap();

    let mut handler_state = Some((pol_info, handler));

    // Create an EpochHandler and initialize it with an epoch-1 tick.
    let mut epoch_handler = EpochHandler::new(TestChainService, 1.try_into().unwrap());
    super::handle_clock_event::<TestBackend, NodeId, MockLeaderProofsGenerator, _, _>(
        SlotTick {
            epoch: Epoch::new(1),
            slot: 1.into(),
        },
        &mut epoch_handler,
        &mut handler_state,
    )
    .await;
    // After the first tick (matching handler's epoch), handler should still be up.
    assert!(
        handler_state.is_some(),
        "Handler should remain active for a tick within the same epoch"
    );

    // Send a tick for epoch 2, which is ahead of the handler's epoch (1).
    super::handle_clock_event::<TestBackend, NodeId, MockLeaderProofsGenerator, _, _>(
        SlotTick {
            epoch: Epoch::new(2),
            slot: 2.into(),
        },
        &mut epoch_handler,
        &mut handler_state,
    )
    .await;
    // The handler should be shut down because epoch 2 > handler's epoch 1.
    assert!(
        handler_state.is_none(),
        "Handler should be shut down when clock tick reveals a newer epoch"
    );
}

/// `handle_new_secret_epoch_info` creates a new message handler with the
/// provided epoch's public and private inputs.
#[test_log::test(tokio::test)]
async fn handle_new_secret_epoch_info_recreates_handler() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let (node_id_sender, _node_id_receiver) = tokio::sync::mpsc::channel(1);

    let edge_membership = membership(&[core_node], local_node);
    let membership_info = MembershipInfo::from_membership_and_session_number(edge_membership, 1);

    let settings = settings(local_node, 1, node_id_sender);
    let overwatch = overwatch_handle();

    // Start with no handler (e.g. after an epoch transition shut it down).
    let mut handler_state: Option<(
        PolEpochInfo,
        super::handlers::MessageHandler<TestBackend, NodeId, MockLeaderProofsGenerator, usize>,
    )> = None;

    // Provide secret PoL info for epoch 2.
    let new_pol_info = test_pol_epoch_info(Epoch::new(2));
    super::handle_new_secret_epoch_info(
        &new_pol_info,
        settings.clone(),
        &overwatch,
        &membership_info,
        &mut handler_state,
    );
    assert!(
        handler_state.is_some(),
        "Handler should be created after secret PoL info is provided"
    );
    assert_eq!(handler_state.as_ref().unwrap().0.epoch, Epoch::new(2));

    // Provide secret PoL info for epoch 3 - handler should be replaced.
    let newer_pol_info = test_pol_epoch_info(Epoch::new(3));
    super::handle_new_secret_epoch_info(
        &newer_pol_info,
        settings,
        &overwatch,
        &membership_info,
        &mut handler_state,
    );
    assert!(handler_state.is_some());
    assert_eq!(handler_state.as_ref().unwrap().0.epoch, Epoch::new(3));
}

/// Full epoch lifecycle: handler active → clock advances epoch → handler shut
/// down → new secret `PoL` info → handler recreated.
#[test_log::test(tokio::test)]
async fn epoch_transition_full_lifecycle() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let (node_id_sender, _node_id_receiver) = tokio::sync::mpsc::channel(1);

    let edge_membership = membership(&[core_node], local_node);
    let membership_info =
        MembershipInfo::from_membership_and_session_number(edge_membership.clone(), 1);
    let settings = settings(local_node, 1, node_id_sender);
    let overwatch = overwatch_handle();

    // Start with handler active at epoch 1.
    let pol_info = test_pol_epoch_info(Epoch::new(1));
    let handler = super::handlers::MessageHandler::<
        TestBackend,
        NodeId,
        MockLeaderProofsGenerator,
        usize,
    >::try_new_with_edge_condition_check(
        settings.clone(),
        edge_membership,
        poq_verification_inputs(),
        pol_info.poq_private_inputs.clone(),
        overwatch.clone(),
        Epoch::new(1),
    )
    .unwrap();
    let mut handler_state = Some((pol_info, handler));

    let mut epoch_handler = EpochHandler::new(TestChainService, 1.try_into().unwrap());

    // Initialize epoch handler with epoch-1 tick.
    super::handle_clock_event::<TestBackend, NodeId, MockLeaderProofsGenerator, _, _>(
        SlotTick {
            epoch: Epoch::new(1),
            slot: 1.into(),
        },
        &mut epoch_handler,
        &mut handler_state,
    )
    .await;
    assert!(handler_state.is_some());

    // Clock advances to epoch 2 - handler shut down.
    super::handle_clock_event::<TestBackend, NodeId, MockLeaderProofsGenerator, _, _>(
        SlotTick {
            epoch: Epoch::new(2),
            slot: 2.into(),
        },
        &mut epoch_handler,
        &mut handler_state,
    )
    .await;
    assert!(
        handler_state.is_none(),
        "Handler should be shut down on epoch advance"
    );

    // Secret PoL info arrives for epoch 2 - handler recreated.
    let new_pol_info = test_pol_epoch_info(Epoch::new(2));
    super::handle_new_secret_epoch_info(
        &new_pol_info,
        settings,
        &overwatch,
        &membership_info,
        &mut handler_state,
    );
    assert!(
        handler_state.is_some(),
        "Handler should be recreated after secret PoL info"
    );
    assert_eq!(handler_state.as_ref().unwrap().0.epoch, Epoch::new(2));
}
