use core::time::Duration;

use futures::stream::repeat;
use lb_blend::{
    message::MAX_PAYLOAD_BODY_SIZE,
    proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs,
    scheduling::membership::Membership,
};
use lb_chain_service::Epoch;
use lb_core::crypto::ZkHash;
use lb_groth16::{AdditiveGroup as _, Fr};
use tokio::{
    sync::{mpsc, oneshot},
    time::{sleep, timeout},
};

use crate::{
    edge::{
        current_epoch::CurrentEpoch,
        handlers::Error,
        tests::utils::{
            MockLeaderProofsGenerator, NodeId, RunningEdgeService, TEST_DELIVERY_DEADLINE,
            TEST_ROUND, TestBackend, overwatch_handle, settings, spawn_run, spawn_run_with_pol,
            spawn_run_without_direct_broadcast,
        },
    },
    epoch_info::PolEpochInfo,
    membership::chain::BlendEpochState,
    message::{BlendPayload, ServiceMessage},
    pending::{NextLocalMessage, PendingTransactions, next_local_message},
    test_utils::{
        epoch::{GatedPolStreamProvider, PolGate},
        membership::membership,
    },
};

pub mod utils;

/// [`run`] forwards messages to the core nodes in the updated membership.
#[test_log::test(tokio::test(start_paused = true))]
#[ignore = "We need a different test setup since we are not blocking the edge tokio task until the secret PoL info is fetched, which makes this test flaky."]
async fn run_with_epoch_transition() {
    let local_node = NodeId(99);
    let mut core_node = NodeId(0);
    let minimal_network_size = 1;
    let RunningEdgeService {
        epochs: epoch_sender,
        messages: msg_sender,
        blended_to: mut node_id_receiver,
        ..
    } = spawn_run(
        local_node,
        minimal_network_size,
        Some(membership(&[core_node], local_node)),
    )
    .await;

    // A message should be forwarded to the core node 0.
    msg_sender
        .send(BlendPayload::BlockProposal(vec![0]).into())
        .await
        .expect("channel opened");
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
    msg_sender
        .send(BlendPayload::BlockProposal(vec![0]).into())
        .await
        .expect("channel opened");
    assert_eq!(
        node_id_receiver.recv().await.expect("channel opened"),
        core_node
    );
}

/// [`run`] broadcasts a block proposal in the clear once the Blend network has
/// had the delivery deadline to deliver it and has not.
///
/// An edge node holds no connections into the network and sees none of its
/// traffic, so the deadline is the only thing that tells it anything — and what
/// it does at the deadline is what a core node does, since a block that never
/// reaches the broadcasting channel is a slot the chain loses either way.
#[test_log::test(tokio::test(start_paused = true))]
async fn a_proposal_the_network_never_delivers_is_broadcast_in_the_clear() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let proposal = vec![7; 8];
    let RunningEdgeService {
        messages: msg_sender,
        blended_to: mut node_id_receiver,
        mut broadcasting_channel,
        ..
    } = spawn_run(local_node, 1, Some(membership(&[core_node], local_node))).await;

    msg_sender
        .send(BlendPayload::BlockProposal(proposal.clone()).into())
        .await
        .expect("channel opened");
    // It goes into the Blend network first: the direct broadcast is the last
    // step and never the first.
    assert_eq!(
        node_id_receiver.recv().await.expect("channel opened"),
        core_node
    );
    assert!(
        broadcasting_channel.dispatched.try_recv().is_err(),
        "nothing is revealed while the network still has time to deliver it"
    );

    let broadcast = timeout(
        TEST_ROUND * u32::try_from(TEST_DELIVERY_DEADLINE.get() + 4).unwrap(),
        broadcasting_channel.dispatched.recv(),
    )
    .await
    .expect("the deadline must expire within the deadline")
    .expect("channel opened");
    assert_eq!(broadcast, BlendPayload::BlockProposal(proposal));
}

/// [`run`] leaves a proposal alone once it has seen it on the broadcasting
/// channel, however it got there.
#[test_log::test(tokio::test(start_paused = true))]
async fn a_proposal_the_network_delivers_is_never_broadcast_in_the_clear() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let proposal = vec![7; 8];
    let RunningEdgeService {
        messages: msg_sender,
        blended_to: mut node_id_receiver,
        mut broadcasting_channel,
        ..
    } = spawn_run(local_node, 1, Some(membership(&[core_node], local_node))).await;

    msg_sender
        .send(BlendPayload::BlockProposal(proposal.clone()).into())
        .await
        .expect("channel opened");
    assert_eq!(
        node_id_receiver.recv().await.expect("channel opened"),
        core_node
    );

    // Some exit node broadcast it, which is all the sender ever learns.
    broadcasting_channel
        .carrying
        .send(BlendPayload::BlockProposal(proposal))
        .expect("the service is subscribed");

    assert!(
        timeout(
            TEST_ROUND * u32::try_from(TEST_DELIVERY_DEADLINE.get() + 4).unwrap(),
            broadcasting_channel.dispatched.recv(),
        )
        .await
        .is_err(),
        "a delivered proposal must not be revealed by its proposer"
    );
}

/// An operator that turns the direct broadcast off keeps the node unlinkable to
/// every payload it sends, and loses the slots the Blend network drops.
#[test_log::test(tokio::test(start_paused = true))]
async fn a_node_that_does_not_bypass_never_broadcasts_in_the_clear() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let RunningEdgeService {
        messages: msg_sender,
        blended_to: mut node_id_receiver,
        mut broadcasting_channel,
        ..
    } = spawn_run_without_direct_broadcast(
        local_node,
        1,
        Some(membership(&[core_node], local_node)),
    )
    .await;

    msg_sender
        .send(BlendPayload::BlockProposal(vec![7; 8]).into())
        .await
        .expect("channel opened");
    assert_eq!(
        node_id_receiver.recv().await.expect("channel opened"),
        core_node
    );

    assert!(
        timeout(
            TEST_ROUND * u32::try_from(TEST_DELIVERY_DEADLINE.get() + 4).unwrap(),
            broadcasting_channel.dispatched.recv(),
        )
        .await
        .is_err(),
        "nothing is revealed, however long the network takes"
    );
}

/// A transaction is watched for through the mempool exactly as a proposal is
/// watched for on the chain's topic.
#[test_log::test(tokio::test(start_paused = true))]
async fn a_transaction_the_network_never_delivers_is_broadcast_in_the_clear() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let transaction = vec![3; 8];
    let RunningEdgeService {
        messages: msg_sender,
        blended_to: mut node_id_receiver,
        mut broadcasting_channel,
        ..
    } = spawn_run(local_node, 1, Some(membership(&[core_node], local_node))).await;

    msg_sender
        .send(BlendPayload::Transaction(transaction.clone()).into())
        .await
        .expect("channel opened");
    assert_eq!(
        node_id_receiver.recv().await.expect("channel opened"),
        core_node
    );

    let broadcast = timeout(
        TEST_ROUND * u32::try_from(TEST_DELIVERY_DEADLINE.get() + 4).unwrap(),
        broadcasting_channel.dispatched.recv(),
    )
    .await
    .expect("the deadline must expire within the deadline")
    .expect("channel opened");
    assert_eq!(broadcast, BlendPayload::Transaction(transaction));
}

/// [`run`] blends a transaction, drawing its layer proofs from the `PoW` branch
/// rather than from leadership quota.
///
/// Unlike a block proposal, a transaction that arrives before the epoch's
/// secret `PoL` info does is not dropped: it waits in the queue until there is
/// a message handler to encapsulate it, which is the same queue that keeps the
/// puzzle search off the event loop.
#[test_log::test(tokio::test(start_paused = true))]
async fn run_blends_a_transaction() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let minimal_network_size = 1;
    let RunningEdgeService {
        epochs: _epoch_sender,
        messages: msg_sender,
        blended_to: mut node_id_receiver,
        ..
    } = spawn_run(
        local_node,
        minimal_network_size,
        Some(membership(&[core_node], local_node)),
    )
    .await;

    msg_sender
        .send(BlendPayload::Transaction(vec![0]).into())
        .await
        .expect("channel opened");
    assert_eq!(
        node_id_receiver.recv().await.expect("channel opened"),
        core_node
    );
}

/// [`run`] shuts down gracefully if a new membership is smaller than the
/// minimum network size.
#[test_log::test(tokio::test(start_paused = true))]
async fn run_shuts_down_if_new_membership_is_small() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let minimal_network_size = 1;
    let RunningEdgeService {
        handle: join_handle,
        epochs: epoch_sender,
        ..
    } = spawn_run(
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
#[test_log::test(tokio::test(start_paused = true))]
async fn run_fails_if_local_is_core_in_new_membership() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let minimal_network_size = 1;
    let RunningEdgeService {
        handle: join_handle,
        epochs: epoch_sender,
        ..
    } = spawn_run(
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
        winning_pol_info_stream: Box::pin(repeat(ProofOfLeadershipQuotaInputs {
            slot: 1,
            note_value: 1,
            transaction_hash: ZkHash::ZERO,
            output_number: 1,
            aged_path_and_selectors: [(ZkHash::ZERO, false); _],
            secret_key: ZkHash::ZERO,
        })),
    }
}

/// `handle_new_epoch_event` creates a new message handler with the provided
/// epoch's public and private inputs, and replaces it on the next epoch.
#[test_log::test(tokio::test(start_paused = true))]
async fn handle_new_secret_epoch_info_recreates_handler() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let (node_id_sender, _node_id_receiver) = mpsc::channel(1);

    let settings = settings(local_node, 1, node_id_sender);
    let overwatch = overwatch_handle();

    // Public + secret for epoch 2 -> handler is created.
    let public_2 = test_blend_epoch_state(Epoch::new(2), membership(&[core_node], local_node));
    let secret_2 = test_pol_epoch_info(Epoch::new(2));
    let current_epoch: CurrentEpoch<TestBackend, NodeId, MockLeaderProofsGenerator, usize> =
        CurrentEpoch::try_new(public_2, &settings)
            .unwrap()
            .with_secret_info(secret_2, settings.clone(), overwatch.clone());
    assert!(
        current_epoch.has_handler(),
        "Handler should be created when public and secret info for the same epoch are present"
    );
    assert_eq!(current_epoch.info().epoch, Epoch::new(2));

    // Public + secret for epoch 3 -> handler is replaced.
    let public_3 = test_blend_epoch_state(Epoch::new(3), membership(&[core_node], local_node));
    let secret_3 = test_pol_epoch_info(Epoch::new(3));
    let current_epoch: CurrentEpoch<TestBackend, NodeId, MockLeaderProofsGenerator, usize> =
        CurrentEpoch::try_new(public_3, &settings)
            .unwrap()
            .with_secret_info(secret_3, settings, overwatch);
    assert!(current_epoch.has_handler());
    assert_eq!(current_epoch.info().epoch, Epoch::new(3));
}

fn test_blend_epoch_state(epoch: Epoch, membership: Membership<NodeId>) -> BlendEpochState<NodeId> {
    BlendEpochState {
        pow_difficulty: ZkHash::ZERO,
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
#[test_log::test(tokio::test(start_paused = true))]
async fn two_publics_without_private_in_between() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let (node_id_sender, _node_id_receiver) = mpsc::channel(1);

    let settings = settings(local_node, 1, node_id_sender);
    let _overwatch = overwatch_handle();

    // First public, no secret yet -> handler must stay down.
    let current_epoch: CurrentEpoch<TestBackend, NodeId, MockLeaderProofsGenerator, usize> =
        CurrentEpoch::try_new(
            test_blend_epoch_state(Epoch::new(1), membership(&[core_node], local_node)),
            &settings,
        )
        .unwrap();
    assert!(
        !current_epoch.has_handler(),
        "Handler must not be created without the private info"
    );

    // Second public for a later epoch, still no secret -> handler stays down.
    let current_epoch: CurrentEpoch<TestBackend, NodeId, MockLeaderProofsGenerator, usize> =
        CurrentEpoch::try_new(
            test_blend_epoch_state(Epoch::new(2), membership(&[core_node], local_node)),
            &settings,
        )
        .unwrap();
    assert!(
        !current_epoch.has_handler(),
        "Handler must remain down: no private info has been received"
    );
}

/// Public arrives first, then private for the same epoch: handler is created
/// on the second call once both sides line up on the same epoch.
#[test_log::test(tokio::test(start_paused = true))]
async fn public_then_private_same_epoch_creates_handler() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let (node_id_sender, _node_id_receiver) = mpsc::channel(1);

    let settings = settings(local_node, 1, node_id_sender);
    let overwatch = overwatch_handle();

    let public_1 = test_blend_epoch_state(Epoch::new(1), membership(&[core_node], local_node));

    // Public for epoch 1, no secret yet -> no handler.
    let current_epoch: CurrentEpoch<TestBackend, NodeId, MockLeaderProofsGenerator, usize> =
        CurrentEpoch::try_new(public_1, &settings).unwrap();
    assert!(!current_epoch.has_handler());

    // Secret for epoch 1 arrives on the *same* epoch value -> handler is built.
    let current_epoch =
        current_epoch.with_secret_info(test_pol_epoch_info(Epoch::new(1)), settings, overwatch);
    assert!(
        current_epoch.has_handler(),
        "Handler must be created once public and secret line up"
    );
    assert_eq!(current_epoch.info().epoch, Epoch::new(1));
}

/// Secret arrives for an epoch ahead of the current public (mismatch), then
/// public catches up to the same epoch: handler is created on the match.
#[test_log::test(tokio::test(start_paused = true))]
async fn private_then_public_same_epoch_creates_handler() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let (node_id_sender, _node_id_receiver) = mpsc::channel(1);

    let settings = settings(local_node, 1, node_id_sender);
    let overwatch = overwatch_handle();

    // Secret for epoch 1 while public is still on epoch 0 -> epochs mismatch,
    // no handler. The secret must be retained for when the public catches up.
    let mut secret_1 = Some(test_pol_epoch_info(Epoch::new(1)));
    let current_epoch: CurrentEpoch<TestBackend, NodeId, MockLeaderProofsGenerator, usize> =
        CurrentEpoch::try_new(
            test_blend_epoch_state(Epoch::new(0), membership(&[core_node], local_node)),
            &settings,
        )
        .unwrap()
        .with_available_secret_info(&mut secret_1, settings.clone(), overwatch.clone());
    assert!(!current_epoch.has_handler());
    assert!(
        secret_1.is_some(),
        "Secret PoL info for a future epoch must be retained on mismatch."
    );

    // Public catches up to epoch 1 -> handler is created.
    let current_epoch: CurrentEpoch<TestBackend, NodeId, MockLeaderProofsGenerator, usize> =
        CurrentEpoch::try_new(
            test_blend_epoch_state(Epoch::new(1), membership(&[core_node], local_node)),
            &settings,
        )
        .unwrap()
        .with_available_secret_info(&mut secret_1, settings, overwatch);
    assert!(
        current_epoch.has_handler(),
        "Handler must be created once public and secret line up"
    );
    assert_eq!(current_epoch.info().epoch, Epoch::new(1));
}

/// A block proposal that arrives before this epoch's secret `PoL` info still
/// goes out once it lands.
///
/// An edge node has no message handler at all until the secret `PoL` info
/// arrives, and that regularly happens *after* the first proposal does — most
/// visibly at startup, when the node wins the very first slot it is asked to
/// lead. The proposal used to be dropped with a warning in that window,
/// silently losing a block this node had just produced.
#[test_log::test(tokio::test(start_paused = true))]
async fn a_proposal_arriving_before_the_pol_info_is_still_blended() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);

    // Installed before the service exists, so the shut gate is in place from the
    // first poll of the stream.
    let pol_gate = PolGate::setup();

    let RunningEdgeService {
        handle: _join_handle,
        epochs: _epoch_sender,
        messages: msg_sender,
        blended_to: mut node_id_receiver,
        ..
    } = spawn_run_with_pol::<GatedPolStreamProvider>(
        local_node,
        1,
        Some(membership(&[core_node], local_node)),
        true,
    )
    .await;

    // The gate is shut, so there is no handler yet: this is the window the
    // proposal used to die in.
    msg_sender
        .send(ServiceMessage::Blend(BlendPayload::BlockProposal(
            b"proposal".to_vec(),
        )))
        .await
        .unwrap();

    // Answering a request proves the service went round the inbound arm again,
    // so the proposal ahead of it has already been handled. Without this the
    // gate could open first and the test would pass on a proposal that was
    // never queued at all.
    let (reply, answered) = oneshot::channel();
    msg_sender
        .send(ServiceMessage::GetPendingTransactions { reply })
        .await
        .unwrap();
    answered.await.unwrap();

    pol_gate.release();

    assert_eq!(
        timeout(Duration::from_secs(5), node_id_receiver.recv())
            .await
            .expect("the proposal should have been held until a handler existed"),
        Some(core_node)
    );
}

/// A message that can never be encapsulated is dropped rather than left
/// blocking everything queued behind it.
///
/// The head of the queue is retried before anything else is looked at, so one
/// that keeps failing — a payload too large to fit, which will not shrink by
/// waiting — would take the whole queue down with it.
#[test_log::test(tokio::test(start_paused = true))]
async fn a_message_that_can_never_be_sent_does_not_block_the_rest() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);

    let RunningEdgeService {
        handle: _join_handle,
        epochs: _epoch_sender,
        messages: msg_sender,
        blended_to: mut node_id_receiver,
        ..
    } = spawn_run(local_node, 1, Some(membership(&[core_node], local_node))).await;

    // One byte over what a payload can hold, so encapsulating it fails the same
    // way however long it waits.
    msg_sender
        .send(ServiceMessage::Blend(BlendPayload::BlockProposal(vec![
            0;
            MAX_PAYLOAD_BODY_SIZE + 1
        ])))
        .await
        .unwrap();
    msg_sender
        .send(ServiceMessage::Blend(BlendPayload::Transaction(
            b"transaction".to_vec(),
        )))
        .await
        .unwrap();

    assert_eq!(
        timeout(Duration::from_secs(5), node_id_receiver.recv())
            .await
            .expect("the transaction behind the oversized proposal should still go out"),
        Some(core_node)
    );
}

/// Secret `PoL` info arriving is not an epoch change, so it must leave queued
/// proposals alone.
///
/// This is the window the queue exists for: a proposal that landed before this
/// epoch's leadership proofs were possible is waiting for exactly this event.
/// Discarding here would put back the bug the queue removes.
#[test_log::test(tokio::test(start_paused = true))]
async fn secret_pol_info_arriving_keeps_queued_proposals() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let (node_id_sender, _node_id_receiver) = mpsc::channel(1);
    let settings = settings(local_node, 1, node_id_sender);

    let mut current_epoch: CurrentEpoch<TestBackend, NodeId, MockLeaderProofsGenerator, usize> =
        CurrentEpoch::try_new(
            test_blend_epoch_state(Epoch::new(1), membership(&[core_node], local_node)),
            &settings,
        )
        .unwrap();
    current_epoch.queue_proposal(b"proposal".to_vec(), 2.try_into().unwrap());

    // The path the secret-`PoL` arm takes, which is not an epoch change: it
    // rebuilds the handler and must leave everything else this epoch owns.
    let current_epoch = current_epoch.with_secret_info(
        test_pol_epoch_info(Epoch::new(1)),
        settings,
        overwatch_handle(),
    );

    assert_eq!(
        next_local_message(current_epoch.proposals(), &PendingTransactions::new()),
        Some(NextLocalMessage::ProposalCopy(b"proposal")),
        "a proposal waiting for this epoch's leadership proofs must survive them arriving"
    );
}

/// Secret `PoL` info for an epoch this node has not reached leaves the epoch it
/// is on alone.
///
/// It used to cost the node its handler: the old code dropped that before
/// looking at which epoch the info named, so learning about the next epoch
/// early stopped this node blending for the current one until the public info
/// caught up. Nothing about a future epoch says anything about this one.
#[test_log::test(tokio::test(start_paused = true))]
async fn secret_pol_info_for_another_epoch_leaves_this_one_alone() {
    let local_node = NodeId(99);
    let core_node = NodeId(0);
    let (node_id_sender, _node_id_receiver) = mpsc::channel(1);
    let settings = settings(local_node, 1, node_id_sender);
    let overwatch = overwatch_handle();

    let current_epoch: CurrentEpoch<TestBackend, NodeId, MockLeaderProofsGenerator, usize> =
        CurrentEpoch::try_new(
            test_blend_epoch_state(Epoch::new(1), membership(&[core_node], local_node)),
            &settings,
        )
        .unwrap()
        .with_secret_info(
            test_pol_epoch_info(Epoch::new(1)),
            settings.clone(),
            overwatch.clone(),
        );
    assert!(current_epoch.has_handler());

    // Info for an epoch this node has not reached yet.
    let mut stashed_secret_info = Some(test_pol_epoch_info(Epoch::new(2)));
    let current_epoch =
        current_epoch.with_available_secret_info(&mut stashed_secret_info, settings, overwatch);

    assert!(
        current_epoch.has_handler(),
        "a secret for a future epoch must not cost this node the handler it is blending with"
    );
    assert!(
        stashed_secret_info.is_some(),
        "and it must stay stashed for the epoch it does name"
    );
}
