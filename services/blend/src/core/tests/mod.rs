use core::{iter::once, num::NonZeroU64, time::Duration};
use std::{collections::VecDeque, sync::Arc};

use futures::{StreamExt as _, stream::repeat};
use lb_blend::{
    message::{
        MAX_PAYLOAD_BODY_SIZE,
        reward::{ActivityProof, BlendingToken, EpochBlendingTokenCollector},
    },
    proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection},
    scheduling::{
        EpochMessageScheduler, epoch::EpochEvent,
        message_blend::crypto::EpochCryptographicProcessorSettings,
    },
};
use lb_chain_service::Epoch;
use lb_core::{crypto::ZkHash, sdp::ActivityMetadata};
use lb_groth16::AdditiveGroup as _;
use lb_key_management_system_service::keys::Ed25519Key;
use lb_poq::{CORE_MERKLE_TREE_HEIGHT, Quota};
use lb_utils::blake_rng::BlakeRng;
use rand::SeedableRng as _;
use rayon::ThreadPoolBuilder;
use tokio::sync::oneshot;

use crate::{
    core::{
        HandleEpochEventOutput,
        backends::BlendBackend,
        epoch_stages::running::CurrentEpoch,
        handle_epoch_event, handle_epoch_transition_expired, handle_incoming_blend_message,
        initialize, post_initialize, retire, run_event_loop,
        state::ServiceState,
        tests::utils::{
            MockKmsAdapter, MockProofsVerifier, NodeId, TestBlendBackend, TestBlendBackendEvent,
            TestPayloadDispatcher, backend_epoch_info, dummy_overwatch_resources,
            dummy_pol_private_inputs, new_crypto_processor, new_epoch_info, new_membership,
            new_stream, outgoing_messages_recorder, published_epochs_recorder,
            recorded_set_epoch_private_calls, reset_set_epoch_private_calls, reward_epoch_info,
            scheduler_epoch_info, scheduler_settings, sdp_relay, seeded_release_delay_rng,
            settings, timing_settings, wait_for_blend_backend_event,
        },
    },
    epoch::{CoreEpochInfo, CoreEpochPublicInfo},
    epoch_info::PolEpochInfo,
    membership::{MembershipInfo, ZkInfo, chain::BlendEpochState},
    message::{BlendPayload, ServiceMessage},
    pending::{NextLocalMessage, PendingLocalMessages},
    test_utils::{
        crypto::{
            GatedPowProofsGenerator, MockCoreAndLeaderProofsGenerator, PolAwareProofsGenerator,
            PowGate, recorded_starting_core_key_indices, reset_starting_core_key_indices,
        },
        epoch::{GatedPolStreamProvider, OncePolStreamProvider, PolGate},
    },
};

mod utils;

type RuntimeServiceId = ();

fn test_blend_epoch_state(
    epoch: u32,
    membership_info: MembershipInfo<NodeId>,
) -> BlendEpochState<NodeId> {
    BlendEpochState {
        pow_difficulty: ZkHash::ZERO,
        epoch: epoch.into(),
        nonce: ZkHash::ZERO,
        aged: ZkHash::ZERO,
        lottery_0: ZkHash::ZERO,
        lottery_1: ZkHash::ZERO,
        membership_info,
    }
}

/// Check if incoming encapsulated messages are properly decapsulated and
/// scheduled by [`handle_incoming_blend_message`].
#[test_log::test(tokio::test)]
#[expect(clippy::too_many_lines, reason = "Test function.")]
async fn test_handle_incoming_blend_message() {
    let (_, _, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();

    // Prepare a encapsulated message.
    let mut epoch = 0.into();
    let minimal_network_size = 1;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    let public_info = new_epoch_info(epoch, membership.clone(), &settings);
    let mut processor = new_crypto_processor(
        EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
            spent_core_quota: Quota::ZERO,
        },
        &public_info,
        (),
    );
    let payload = vec![];
    let msg = processor
        .encapsulate_block_proposal_payload(&payload)
        .await
        .expect("encapsulation must succeed");

    // Check that the message is successfully decapsulated and scheduled.
    let scheduler_settings = scheduler_settings(&timing_settings(), settings.num_blend_layers);
    let mut scheduler = EpochMessageScheduler::new(
        scheduler_epoch_info(&public_info),
        BlakeRng::from_entropy(),
        scheduler_settings,
    );
    let recovery_checkpoint = ServiceState::with_epoch(
        epoch,
        VecDeque::new(),
        EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info)),
        None,
        state_updater,
    )
    .unwrap();
    let recovery_checkpoint = handle_incoming_blend_message(
        (msg.clone(), 0.into()),
        &mut scheduler,
        None,
        processor.receiver(),
        None,
        recovery_checkpoint,
    );
    assert_eq!(scheduler.release_delayer().unreleased_messages().len(), 1);
    assert_eq!(
        recovery_checkpoint
            .current_epoch_token_collector()
            .tokens()
            .len(),
        1
    );

    // Creates a new processor/scheduler/token_collector with the new epoch
    // number. The outgoing processor is retired into its receive-only form,
    // which is all it is good for during the transition period.
    let processor = processor.rotate_epoch();
    epoch = epoch.strict_add(1.into());
    let public_info = new_epoch_info(epoch, membership.clone(), &settings);
    let mut new_processor = new_crypto_processor(
        EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
            spent_core_quota: Quota::ZERO,
        },
        &public_info,
        (),
    );
    let (mut new_scheduler, mut scheduler) =
        scheduler.rotate_epoch(scheduler_epoch_info(&public_info), scheduler_settings);
    let (_, _, _, _, _, current_token_collector, _, state_updater) =
        recovery_checkpoint.into_components();
    let (new_token_collector, old_token_collector) =
        EpochBlendingTokenCollector::clone(&current_token_collector)
            .rotate_epoch(&reward_epoch_info(&public_info));

    // Check that decapsulating the same message fails with the new processor
    // but succeeds with the old one. Also, it should be scheduled in the old
    // scheduler.
    let recovery_checkpoint = ServiceState::with_epoch(
        epoch,
        VecDeque::new(),
        new_token_collector,
        Some(old_token_collector),
        state_updater,
    )
    .unwrap();
    let recovery_checkpoint = handle_incoming_blend_message(
        (msg.clone(), 0.into()),
        &mut new_scheduler,
        Some(&mut scheduler),
        new_processor.receiver(),
        Some(&processor),
        recovery_checkpoint,
    );
    assert_eq!(
        new_scheduler.release_delayer().unreleased_messages().len(),
        0
    );
    assert_eq!(scheduler.release_delayer().unreleased_messages().len(), 2);
    assert_eq!(
        recovery_checkpoint
            .current_epoch_token_collector()
            .tokens()
            .len(),
        0
    );
    // No new token should be collected from the same message.
    assert_eq!(
        recovery_checkpoint
            .clone()
            .start_updating()
            .clear_old_epoch_token_collector()
            .unwrap()
            .tokens()
            .len(),
        1
    );

    // Check that a new message built with the new processor is decapsulated
    // with the new processor and scheduled in the new scheduler.
    let msg = new_processor
        .encapsulate_block_proposal_payload(&payload)
        .await
        .expect("encapsulation must succeed");
    let recovery_checkpoint = handle_incoming_blend_message(
        (msg, 1.into()),
        &mut new_scheduler,
        Some(&mut scheduler),
        new_processor.receiver(),
        Some(&processor),
        recovery_checkpoint,
    );
    assert_eq!(
        new_scheduler.release_delayer().unreleased_messages().len(),
        1
    );
    assert_eq!(scheduler.release_delayer().unreleased_messages().len(), 2);
    assert_eq!(
        recovery_checkpoint
            .current_epoch_token_collector()
            .tokens()
            .len(),
        1
    );
    assert_eq!(
        recovery_checkpoint
            .clone()
            .start_updating()
            .clear_old_epoch_token_collector()
            .unwrap()
            .tokens()
            .len(),
        1
    );

    // Check that a message built with a future epoch cannot be
    // decapsulated by either processor, and thus not scheduled.
    epoch = epoch.strict_add(1.into());
    let mut future_processor = new_crypto_processor(
        EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
            spent_core_quota: Quota::ZERO,
        },
        &new_epoch_info(epoch, membership, &settings),
        (),
    );
    let msg = future_processor
        .encapsulate_block_proposal_payload(&payload)
        .await
        .expect("encapsulation must succeed");
    let recovery_checkpoint = handle_incoming_blend_message(
        (msg, 2.into()),
        &mut new_scheduler,
        Some(&mut scheduler),
        new_processor.receiver(),
        Some(&processor),
        recovery_checkpoint,
    );
    // Nothing changed.
    assert_eq!(
        new_scheduler.release_delayer().unreleased_messages().len(),
        1
    );
    assert_eq!(scheduler.release_delayer().unreleased_messages().len(), 2);
    assert_eq!(
        recovery_checkpoint
            .current_epoch_token_collector()
            .tokens()
            .len(),
        1
    );
    assert_eq!(
        recovery_checkpoint
            .start_updating()
            .clear_old_epoch_token_collector()
            .unwrap()
            .tokens()
            .len(),
        1
    );
}

/// Regression test for audit finding #1: two replicas of one data message must
/// not crash the core service. The service emits `data_replication_factor + 1`
/// copies, each with fresh random layers (distinct encapsulated IDs), but
/// decapsulation yields the same inner `NetworkMessage` — and
/// `ProcessedMessage` hashes on that content:
///
/// ```text
///   replica A ─encap(rand)→ ID_a ─┐ swarm dedups on ID, so both pass
///   replica B ─encap(rand)→ ID_b ─┘
///                                  │  decapsulate at same final node
///                                  ▼
///        both → ProcessedMessage::Network(same bytes)   (Eq/Hash on content)
///                                  │
///                                  ▼  add_unsent_processed_message(..)
///            A: Ok ──► inserted        B: Err ──► dropped as duplicate ✓
/// ```
///
/// `handle_decapsulated_incoming_message_from_current_epoch` now treats a
/// duplicate insert as already-known rather than asserting uniqueness, so the
/// second replica is dropped gracefully and exactly one copy stays pending
/// release. Size-1 membership here forces local full decapsulation.
#[test_log::test(tokio::test)]
async fn test_duplicate_decapsulated_replica_handled_gracefully() {
    let (_, _, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();

    // Size-1 membership: the only node is the local node, so every encapsulated
    // message is fully decapsulated locally into its inner `NetworkMessage`.
    let epoch = 0.into();
    let minimal_network_size = 1;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        // data_replication_factor: the panic is independent of this value; the
        // realistic trigger is the >1 replicas the service emits per message.
        1,
    );
    let public_info = new_epoch_info(epoch, membership.clone(), &settings);
    let mut processor = new_crypto_processor(
        EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
            spent_core_quota: Quota::ZERO,
        },
        &public_info,
        (),
    );

    // One logical data message, serialized once...
    let payload = vec![];

    // ...encapsulated twice. Each call draws fresh randomness, so these are two
    // distinct encapsulated messages (different identifiers) that the swarm
    // would forward independently — exactly the replicas the service produces.
    let replica_a = processor
        .encapsulate_block_proposal_payload(&payload)
        .await
        .expect("encapsulation must succeed");
    let replica_b = processor
        .encapsulate_block_proposal_payload(&payload)
        .await
        .expect("encapsulation must succeed");
    assert_ne!(
        replica_a, replica_b,
        "the two replicas must be distinct encapsulations (so the swarm does not dedup them)"
    );

    let scheduler_settings = scheduler_settings(&timing_settings(), settings.num_blend_layers);
    let mut scheduler = EpochMessageScheduler::new(
        scheduler_epoch_info(&public_info),
        BlakeRng::from_entropy(),
        scheduler_settings,
    );
    let recovery_checkpoint = ServiceState::with_epoch(
        epoch,
        VecDeque::new(),
        EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info)),
        None,
        state_updater,
    )
    .unwrap();

    // First replica: decapsulated and recorded as an unsent processed message.
    let recovery_checkpoint = handle_incoming_blend_message(
        (replica_a, epoch),
        &mut scheduler,
        None,
        processor.receiver(),
        None,
        recovery_checkpoint,
    );
    assert_eq!(
        recovery_checkpoint.unsent_processed_messages().len(),
        1,
        "the first replica must be recorded as an unsent processed message"
    );

    // Second replica: decapsulates to the *same* `ProcessedMessage::Network`.
    // The insert into `unsent_processed_messages` returns `Err`, but it is now
    // treated as a known duplicate instead of panicking the task.
    let recovery_checkpoint = handle_incoming_blend_message(
        (replica_b, epoch),
        &mut scheduler,
        None,
        processor.receiver(),
        None,
        recovery_checkpoint,
    );
    assert_eq!(
        recovery_checkpoint.unsent_processed_messages().len(),
        1,
        "the duplicate replica must be dropped, leaving exactly one pending message"
    );
}

#[test_log::test(tokio::test)]
async fn test_handle_incoming_blend_message_with_invalid_poq() {
    let (_, _, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();

    let minimal_network_size = 1;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );

    // Create epoch 0 processor and build a message with epoch 0 proofs.
    let epoch_0 = 0.into();
    let public_info_0 = new_epoch_info(epoch_0, membership.clone(), &settings);
    let mut processor_0 = new_crypto_processor(
        EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
            spent_core_quota: Quota::ZERO,
        },
        &public_info_0,
        (),
    );

    let payload = vec![];
    let msg = processor_0
        .encapsulate_block_proposal_payload(&payload)
        .await
        .expect("encapsulation must succeed");

    // Create epoch 1 processor - its MockProofsVerifier expects epoch 1
    // proofs.
    let epoch_1 = 1.into();
    let public_info_1 = new_epoch_info(epoch_1, membership, &settings);
    let processor_1 = new_crypto_processor(
        EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
            spent_core_quota: Quota::ZERO,
        },
        &public_info_1,
        (),
    );

    let scheduler_settings = scheduler_settings(&timing_settings(), settings.num_blend_layers);
    let mut scheduler = EpochMessageScheduler::new(
        scheduler_epoch_info(&public_info_1),
        BlakeRng::from_entropy(),
        scheduler_settings,
    );
    let recovery_checkpoint = ServiceState::with_epoch(
        epoch_1,
        VecDeque::new(),
        EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info_1)),
        None,
        state_updater,
    )
    .unwrap();

    // Send epoch 0 message claiming to be for epoch 1.
    // Signature is valid (built correctly) but PoQ will fail because the
    // MockProofsVerifier for epoch 1 expects epoch 1 proofs.
    drop(handle_incoming_blend_message(
        (msg, epoch_1),
        &mut scheduler,
        None,
        processor_1.receiver(),
        None,
        recovery_checkpoint,
    ));

    // Nothing should be scheduled - PoQ validation must have failed.
    assert_eq!(
        scheduler.release_delayer().unreleased_messages().len(),
        0,
        "Message with invalid PoQ should not be scheduled"
    );
}

#[test_log::test(tokio::test)]
async fn test_handle_epoch_transition_expired() {
    let (overwatch_handle, _, _, _) = dummy_overwatch_resources::<(), (), RuntimeServiceId>();

    // Prepare settings.
    let epoch = 0.into();
    let minimal_network_size = 1;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let mut settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    // Set a long rounds_per_epoch to make the core quota large enough,
    // since we want the activity threshold to be sufficiently high.
    settings.time.rounds_per_epoch = 648_000.try_into().unwrap();

    // Create backend.
    let public_info = new_epoch_info(epoch, membership.clone(), &settings);
    let mut backend = <TestBlendBackend as BlendBackend<_, _, _, _>>::new(
        settings.clone(),
        overwatch_handle.clone(),
        backend_epoch_info(&public_info),
        BlakeRng::from_entropy(),
    );
    let mut backend_event_receiver = backend.subscribe_to_events();

    // Create token collector and collect a token.
    let mut token_collector = EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info))
        .rotate_epoch(&reward_epoch_info(&new_epoch_info(
            epoch.strict_add(1.into()),
            membership.clone(),
            &settings,
        )))
        .1;
    let token = BlendingToken::new(
        Ed25519Key::from_bytes(&[0; _]).public_key(),
        VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
        VerifiedProofOfSelection::from_bytes_unchecked([0; _]),
    );
    token_collector.collect(token.clone());

    // Create SDP relay.
    let (sdp_relay, mut sdp_relay_receiver) = sdp_relay();

    // Call `handle_epoch_transition_expired`.
    handle_epoch_transition_expired::<_, NodeId, BlakeRng, MockProofsVerifier, _>(
        &mut backend,
        token_collector,
        &sdp_relay,
    )
    .await;

    // Check that the backend handled the transition completion.
    wait_for_blend_backend_event(
        &mut backend_event_receiver,
        TestBlendBackendEvent::EpochTransitionCompleted,
    )
    .await;

    // Check that an activity proof has been submitted to SDP service.
    let lb_sdp_service::SdpMessage::PostActivity {
        metadata: ActivityMetadata::Blend(activity_proof),
    } = sdp_relay_receiver
        .try_recv()
        .expect("an activity proof must be submitted")
    else {
        panic!("expected PostActivity with ActivityMetadata::Blend");
    };
    assert_eq!(*activity_proof, (&ActivityProof::new(epoch, token)).into());
}

/// A proposal still queued when the epoch rotates is dropped; a transaction is
/// not.
///
/// Leadership quota is one message's worth per winning slot, so a proposal that
/// missed its epoch would spend the quota the new epoch's own block needs.
/// Already-encapsulated messages are unaffected: they have left the queue for
/// the scheduler, and the previous epoch's scheduler keeps releasing them for
/// the transition period, when peers still hold that epoch's verifier.
#[test_log::test(tokio::test)]
async fn test_handle_epoch_event_discards_queued_proposals() {
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();

    // Prepare components for epoch event handling.
    let epoch = 0.into();
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    let public_info = new_epoch_info(epoch, membership.clone(), &settings);
    let crypto_processor = new_crypto_processor(
        EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
            spent_core_quota: Quota::ZERO,
        },
        &public_info,
        (),
    );
    let scheduler = EpochMessageScheduler::new(
        scheduler_epoch_info(&public_info),
        BlakeRng::from_entropy(),
        scheduler_settings(&settings.time, settings.num_blend_layers),
    );
    let token_collector = EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info));
    let mut backend = <TestBlendBackend as BlendBackend<_, _, _, _>>::new(
        settings.clone(),
        overwatch_handle.clone(),
        backend_epoch_info(&public_info),
        BlakeRng::from_entropy(),
    );
    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();

    let mut pending_messages = PendingLocalMessages::new();
    pending_messages.queue_proposal(b"proposal".to_vec(), 2.try_into().unwrap());
    pending_messages.queue_transaction(b"transaction".to_vec());

    let _output = handle_epoch_event(
        EpochEvent::NewEpoch(
            CoreEpochInfo {
                public: CoreEpochPublicInfo {
                    epoch: epoch.strict_add(1.into()),
                    ..public_info.clone()
                },
                core_poq_generator: Some(()),
            }
            .into(),
        ),
        &settings,
        crypto_processor,
        scheduler,
        public_info,
        ServiceState::with_epoch(
            epoch,
            VecDeque::new(),
            token_collector,
            None,
            state_updater.clone(),
        )
        .unwrap(),
        &mut backend,
        &sdp_relay,
        &mut None,
        &mut pending_messages,
    )
    .await;

    assert_eq!(
        pending_messages.next(),
        Some(NextLocalMessage::Transaction(b"transaction")),
        "the proposal must not survive the rotation, and the transaction must"
    );
}

#[test_log::test(tokio::test)]
#[expect(clippy::too_many_lines, reason = "Test function.")]
async fn test_handle_epoch_event() {
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();

    // Prepare components for epoch event handling.
    let epoch = 0.into();
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    let public_info = new_epoch_info(epoch, membership.clone(), &settings);
    let crypto_processor = new_crypto_processor(
        EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
            spent_core_quota: Quota::ZERO,
        },
        &public_info,
        (),
    );
    let scheduler = EpochMessageScheduler::new(
        scheduler_epoch_info(&public_info),
        BlakeRng::from_entropy(),
        scheduler_settings(&settings.time, settings.num_blend_layers),
    );
    let token_collector = EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info));
    let mut backend = <TestBlendBackend as BlendBackend<_, _, _, _>>::new(
        settings.clone(),
        overwatch_handle.clone(),
        backend_epoch_info(&public_info),
        BlakeRng::from_entropy(),
    );
    let mut backend_event_receiver = backend.subscribe_to_events();
    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();

    // Handle a NewEpoch event, expecting Transitioning output.
    let output = handle_epoch_event(
        EpochEvent::NewEpoch(
            CoreEpochInfo {
                public: CoreEpochPublicInfo {
                    epoch: epoch.strict_add(1.into()),
                    ..public_info.clone()
                },
                core_poq_generator: Some(()),
            }
            .into(),
        ),
        &settings,
        crypto_processor,
        scheduler,
        public_info,
        ServiceState::with_epoch(
            epoch,
            VecDeque::new(),
            token_collector,
            None,
            state_updater.clone(),
        )
        .unwrap(),
        &mut backend,
        &sdp_relay,
        &mut None,
        &mut PendingLocalMessages::new(),
    )
    .await;
    let HandleEpochEventOutput::Transitioning {
        new_crypto_processor,
        new_scheduler,
        new_epoch_info,
        new_recovery_checkpoint,
        old_epoch_components,
    } = output
    else {
        panic!("expected Transitioning output");
    };
    assert_eq!(new_crypto_processor.epoch(), epoch.strict_add(1.into()));
    assert_eq!(old_epoch_components.epoch(), epoch);
    assert_eq!(
        new_scheduler.release_delayer().unreleased_messages().len(),
        0
    );
    assert_eq!(
        old_epoch_components
            .scheduler()
            .release_delayer()
            .unreleased_messages()
            .len(),
        0
    );
    assert_eq!(new_epoch_info.epoch, epoch.strict_add(1.into()));
    assert!(
        new_recovery_checkpoint
            .clone()
            .start_updating()
            .clear_old_epoch_token_collector()
            .is_some()
    );

    // Handle a TransitionExpired event, expecting TransitionCompleted output.
    let output = handle_epoch_event(
        EpochEvent::TransitionPeriodExpired,
        &settings,
        new_crypto_processor,
        new_scheduler,
        new_epoch_info,
        new_recovery_checkpoint,
        &mut backend,
        &sdp_relay,
        &mut None,
        &mut PendingLocalMessages::new(),
    )
    .await;
    let HandleEpochEventOutput::TransitionCompleted {
        current_crypto_processor,
        current_scheduler,
        current_epoch_info,
        new_recovery_checkpoint,
    } = output
    else {
        panic!("expected TransitionCompleted output");
    };
    assert_eq!(current_crypto_processor.epoch(), epoch.strict_add(1.into()));
    assert_eq!(current_epoch_info.epoch, epoch.strict_add(1.into()));
    assert!(
        new_recovery_checkpoint
            .clone()
            .start_updating()
            .clear_old_epoch_token_collector()
            .is_none()
    );
    wait_for_blend_backend_event(
        &mut backend_event_receiver,
        TestBlendBackendEvent::EpochTransitionCompleted,
    )
    .await;

    // Handle a NewEpoch event with a new too small membership,
    // expecting Retiring output.
    let output = handle_epoch_event(
        EpochEvent::NewEpoch(
            CoreEpochInfo {
                public: CoreEpochPublicInfo {
                    membership: new_membership(minimal_network_size - 1).0,
                    epoch: epoch.strict_add(2.into()),
                    ..current_epoch_info.clone()
                },
                core_poq_generator: Some(()),
            }
            .into(),
        ),
        &settings,
        current_crypto_processor,
        current_scheduler,
        current_epoch_info,
        new_recovery_checkpoint,
        &mut backend,
        &sdp_relay,
        &mut None,
        &mut PendingLocalMessages::new(),
    )
    .await;
    let HandleEpochEventOutput::Retiring { retiring_epoch } = output else {
        panic!("expected Retiring output");
    };
    assert_eq!(retiring_epoch.epoch(), epoch.strict_add(1.into()));
}

/// On an epoch change where the membership actually changes (and the local node
/// remains part of the core), the service must transition: build a new
/// cryptographic generator bound to the new epoch, retain the old one for the
/// old epoch, and propagate the *new* membership to the backend via
/// `rotate_epoch`.
#[test_log::test(tokio::test)]
async fn test_handle_epoch_event_membership_change_rewires_backend_and_generators() {
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();

    let epoch = 0.into();
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    let public_info = new_epoch_info(epoch, membership.clone(), &settings);
    let crypto_processor = new_crypto_processor(
        EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
            spent_core_quota: Quota::ZERO,
        },
        &public_info,
        (),
    );
    let scheduler = EpochMessageScheduler::new(
        scheduler_epoch_info(&public_info),
        BlakeRng::from_entropy(),
        scheduler_settings(&settings.time, settings.num_blend_layers),
    );
    let token_collector = EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info));
    let mut backend = <TestBlendBackend as BlendBackend<_, _, _, _>>::new(
        settings.clone(),
        overwatch_handle.clone(),
        backend_epoch_info(&public_info),
        BlakeRng::from_entropy(),
    );
    let mut backend_event_receiver = backend.subscribe_to_events();
    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();

    // The new epoch has a *different* (larger) membership; `new_membership`
    // always includes the local node, so the node stays part of the core.
    let new_epoch = epoch.strict_add(1.into());
    let new_membership = new_membership(minimal_network_size + 1).0;
    assert_ne!(
        new_membership.size(),
        membership.size(),
        "the test must exercise an actual membership change"
    );
    let new_public_info = new_epoch_info(new_epoch, new_membership.clone(), &settings);

    let output = handle_epoch_event(
        EpochEvent::NewEpoch(
            CoreEpochInfo {
                public: new_public_info.clone(),
                core_poq_generator: Some(()),
            }
            .into(),
        ),
        &settings,
        crypto_processor,
        scheduler,
        public_info,
        ServiceState::with_epoch(
            epoch,
            VecDeque::new(),
            token_collector,
            None,
            state_updater.clone(),
        )
        .unwrap(),
        &mut backend,
        &sdp_relay,
        &mut None,
        &mut PendingLocalMessages::new(),
    )
    .await;

    let HandleEpochEventOutput::Transitioning {
        new_crypto_processor,
        old_epoch_components,
        new_epoch_info: returned_epoch_info,
        ..
    } = output
    else {
        panic!("expected Transitioning output");
    };

    // A fresh generator is built for the new epoch, and the previous one is
    // retained for the old epoch.
    assert_eq!(new_crypto_processor.epoch(), new_epoch);
    assert_eq!(old_epoch_components.epoch(), epoch);
    // The returned public info carries the new membership.
    assert_eq!(returned_epoch_info.epoch, new_epoch);
    assert_eq!(returned_epoch_info.membership.size(), new_membership.size());

    // The backend was rotated to the new epoch, carrying the *new* membership.
    wait_for_blend_backend_event(
        &mut backend_event_receiver,
        TestBlendBackendEvent::EpochRotated {
            epoch: new_epoch,
            membership_size: new_membership.size(),
        },
    )
    .await;
}

async fn transition_to_new_epoch_with_secret(secret_epoch: Epoch) -> Vec<Epoch> {
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();
    let epoch = 0.into();
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    let public_info = new_epoch_info(epoch, membership.clone(), &settings);
    let crypto_processor = new_crypto_processor(
        EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
            spent_core_quota: Quota::ZERO,
        },
        &public_info,
        (),
    );
    let scheduler = EpochMessageScheduler::new(
        scheduler_epoch_info(&public_info),
        BlakeRng::from_entropy(),
        scheduler_settings(&settings.time, settings.num_blend_layers),
    );
    let token_collector = EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info));
    let mut backend = <TestBlendBackend as BlendBackend<_, _, _, _>>::new(
        settings.clone(),
        overwatch_handle.clone(),
        backend_epoch_info(&public_info),
        BlakeRng::from_entropy(),
    );
    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();

    let secret_info = PolEpochInfo {
        epoch: secret_epoch,
        winning_pol_info_stream: Box::pin(repeat(dummy_pol_private_inputs())),
    };

    // Isolate the `set_epoch_private` calls made by `handle_epoch_event`.
    reset_set_epoch_private_calls();
    let _output = handle_epoch_event(
        EpochEvent::NewEpoch(
            CoreEpochInfo {
                public: CoreEpochPublicInfo {
                    epoch: epoch.strict_add(1.into()),
                    ..public_info.clone()
                },
                core_poq_generator: Some(()),
            }
            .into(),
        ),
        &settings,
        crypto_processor,
        scheduler,
        public_info,
        ServiceState::with_epoch(
            epoch,
            VecDeque::new(),
            token_collector,
            None,
            state_updater.clone(),
        )
        .unwrap(),
        &mut backend,
        &sdp_relay,
        &mut Some(secret_info),
        &mut PendingLocalMessages::new(),
    )
    .await;
    recorded_set_epoch_private_calls()
}

/// On an epoch change, if secret `PoL` info for the *new* epoch is already
/// available (`current_secret_info`), it must be applied to the *new*
/// cryptographic generator via `set_epoch_private`. If the available secret
/// info is for a different epoch, it must not be applied. This is the
/// public-stream side of the public/secret out-of-order coordination.
#[test_log::test(tokio::test)]
async fn test_handle_epoch_event_applies_matching_secret_to_new_generator() {
    // Secret for the new epoch (1) is applied to the new generator.
    assert_eq!(
        transition_to_new_epoch_with_secret(1.into()).await,
        vec![Epoch::new(1)],
        "secret matching the new epoch must be applied to the new generator"
    );

    // Secret for a non-matching epoch (5) must not be applied.
    assert!(
        transition_to_new_epoch_with_secret(5.into())
            .await
            .is_empty(),
        "secret for a non-matching epoch must not be applied to the new generator"
    );
}

/// Handle a `NewEpoch(Empty)` event (empty membership), expecting `Retiring`
/// output. This exercises the `MaybeEmptyCoreEpochInfo::Empty` branch of
/// `handle_epoch_event` directly.
#[test_log::test(tokio::test)]
async fn test_handle_epoch_event_empty_epoch_retires() {
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();

    let epoch = 0.into();
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    let public_info = new_epoch_info(epoch, membership.clone(), &settings);
    let crypto_processor = new_crypto_processor(
        EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
            spent_core_quota: Quota::ZERO,
        },
        &public_info,
        (),
    );
    let scheduler = EpochMessageScheduler::new(
        scheduler_epoch_info(&public_info),
        BlakeRng::from_entropy(),
        scheduler_settings(&settings.time, settings.num_blend_layers),
    );
    let token_collector = EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info));
    let mut backend = <TestBlendBackend as BlendBackend<_, _, _, _>>::new(
        settings.clone(),
        overwatch_handle.clone(),
        backend_epoch_info(&public_info),
        BlakeRng::from_entropy(),
    );
    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();

    // Handle a NewEpoch(Empty) event - empty membership triggers Retiring.
    let empty_epoch = epoch.strict_add(1.into());
    let output = handle_epoch_event(
        EpochEvent::NewEpoch((empty_epoch, ZkHash::from(1)).into()),
        &settings,
        crypto_processor,
        scheduler,
        public_info.clone(),
        ServiceState::with_epoch(
            epoch,
            VecDeque::new(),
            token_collector,
            None,
            state_updater.clone(),
        )
        .unwrap(),
        &mut backend,
        &sdp_relay,
        &mut None,
        &mut PendingLocalMessages::new(),
    )
    .await;
    let HandleEpochEventOutput::Retiring { retiring_epoch } = output else {
        panic!("expected Retiring output for Empty epoch");
    };
    // The old processor/info should be from the epoch we were on before
    // the empty epoch arrived.
    assert_eq!(retiring_epoch.epoch(), epoch);
}

/// Handle a `NewEpoch(NonEmpty)` event where membership exists but the local
/// node is not part of it (`core_poq_generator = None`), expecting `Retiring`
/// output.
#[test_log::test(tokio::test)]
async fn test_handle_epoch_event_non_empty_without_local_core_path_retires() {
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();

    let epoch = 0.into();
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    let public_info = new_epoch_info(epoch, membership.clone(), &settings);
    let crypto_processor = new_crypto_processor(
        EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
            spent_core_quota: Quota::ZERO,
        },
        &public_info,
        (),
    );
    let scheduler = EpochMessageScheduler::new(
        scheduler_epoch_info(&public_info),
        BlakeRng::from_entropy(),
        scheduler_settings(&settings.time, settings.num_blend_layers),
    );
    let token_collector = EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info));
    let mut backend = <TestBlendBackend as BlendBackend<_, _, _, _>>::new(
        settings.clone(),
        overwatch_handle.clone(),
        backend_epoch_info(&public_info),
        BlakeRng::from_entropy(),
    );
    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();

    let output = handle_epoch_event(
        EpochEvent::NewEpoch(
            CoreEpochInfo {
                public: CoreEpochPublicInfo {
                    epoch: epoch.strict_add(1.into()),
                    ..public_info.clone()
                },
                core_poq_generator: None,
            }
            .into(),
        ),
        &settings,
        crypto_processor,
        scheduler,
        public_info.clone(),
        ServiceState::with_epoch(
            epoch,
            VecDeque::new(),
            token_collector,
            None,
            state_updater.clone(),
        )
        .unwrap(),
        &mut backend,
        &sdp_relay,
        &mut None,
        &mut PendingLocalMessages::new(),
    )
    .await;

    let HandleEpochEventOutput::Retiring { retiring_epoch } = output else {
        panic!("expected Retiring output for NonEmpty epoch without local core path");
    };

    assert_eq!(retiring_epoch.epoch(), epoch);
}

/// Check if the service keeps running after it receives a new epoch where
/// it's still core. Also, check if it stops after the epoch transition period
/// if it receives another new epoch that doesn't meet the core node
/// conditions.
#[expect(clippy::too_many_lines, reason = "Test function.")]
#[test_log::test(tokio::test)]
async fn complete_old_epoch_after_main_loop_done() {
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);

    // Create settings.
    let settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );

    // Prepare streams.
    let (inbound_relay, _inbound_message_sender) = new_stream();
    let (mut blend_message_stream, _blend_message_sender) = new_stream();
    let (membership_stream, membership_sender) = new_stream();

    // Send the initial membership info that the service will expect to receive
    // immediately.
    let mut membership_info = MembershipInfo {
        membership: membership.clone(),
        zk: Some(ZkInfo {
            root: ZkHash::ZERO,
            core_and_path_selectors: Some([(ZkHash::ZERO, false); CORE_MERKLE_TREE_HEIGHT]),
        }),
    };
    membership_sender
        .send(test_blend_epoch_state(0, membership_info.clone()))
        .await
        .unwrap();

    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();

    // Prepare dummy Overwatch resources.
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources();

    // Initialize the service.
    let (
        mut remaining_epoch_stream,
        current_public_info,
        crypto_processor,
        current_recovery_checkpoint,
        pending_transactions,
        message_scheduler,
        mut backend,
        mut rng,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestPayloadDispatcher,
        MockCoreAndLeaderProofsGenerator,
        MockProofsVerifier,
        MockKmsAdapter,
        RuntimeServiceId,
    >(
        settings.clone(),
        membership_stream,
        overwatch_handle.clone(),
        MockKmsAdapter,
        &sdp_relay,
        None,
        state_updater,
        seeded_release_delay_rng(),
    )
    .await;
    let mut backend_event_receiver = backend.subscribe_to_events();

    // Run the event loop of the service in a separate task.
    let settings_cloned = settings.clone();
    let join_handle = tokio::spawn(async move {
        let secret_pol_info_stream =
            post_initialize::<OncePolStreamProvider, RuntimeServiceId>(&overwatch_handle).await;

        let retiring_epoch = run_event_loop(
            inbound_relay,
            &mut blend_message_stream,
            secret_pol_info_stream,
            &mut remaining_epoch_stream,
            &settings_cloned,
            &mut backend,
            &TestPayloadDispatcher,
            &sdp_relay,
            &mut rng,
            CurrentEpoch::new(
                crypto_processor,
                message_scheduler.into(),
                current_public_info,
                pending_transactions,
                None,
            ),
            current_recovery_checkpoint,
        )
        .await;

        retire(
            blend_message_stream.map(|(msg, _)| msg),
            remaining_epoch_stream,
            backend,
            TestPayloadDispatcher,
            sdp_relay,
            rng,
            retiring_epoch,
        )
        .await;
    });

    // Send a new epoch with the same membership.

    membership_sender
        .send(test_blend_epoch_state(1, membership_info.clone()))
        .await
        .unwrap();

    // Since the node is still core in the new epoch,
    // the service must keep running even after a epoch transition period.
    wait_for_blend_backend_event(
        &mut backend_event_receiver,
        TestBlendBackendEvent::EpochTransitionCompleted,
    )
    .await;
    assert!(!join_handle.is_finished());

    // Send a new epoch with a new membership smaller than minimal size
    membership_info.membership = new_membership(minimal_network_size.checked_sub(1).unwrap()).0;

    membership_sender
        .send(test_blend_epoch_state(2, membership_info))
        .await
        .unwrap();

    // Since the network is smaller than the minimal size,
    // the service must stop after a epoch transition period.
    wait_for_blend_backend_event(
        &mut backend_event_receiver,
        TestBlendBackendEvent::EpochTransitionCompleted,
    )
    .await;
    join_handle
        .await
        .expect("the service should stop without error");
}

/// Check that the service handles a new epoch with empty providers (zk: None)
/// without panicking. It should retire gracefully.
#[expect(clippy::too_many_lines, reason = "Test function.")]
#[test_log::test(tokio::test)]
async fn stop_on_empty_epoch() {
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);

    // Create settings.
    let settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );

    // Prepare streams.
    let (inbound_relay, _inbound_message_sender) = new_stream();
    let (mut blend_message_stream, _blend_message_sender) = new_stream();
    let (membership_stream, membership_sender) = new_stream();

    // Send the initial membership info that the service will expect to receive
    // immediately.
    let membership_info = MembershipInfo {
        membership: membership.clone(),
        zk: Some(ZkInfo {
            root: ZkHash::ZERO,
            core_and_path_selectors: Some([(ZkHash::ZERO, false); CORE_MERKLE_TREE_HEIGHT]),
        }),
    };
    membership_sender
        .send(test_blend_epoch_state(0, membership_info.clone()))
        .await
        .unwrap();

    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();

    // Prepare dummy Overwatch resources.
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources();

    // Initialize the service.
    let (
        mut remaining_epoch_stream,
        current_public_info,
        crypto_processor,
        current_recovery_checkpoint,
        pending_transactions,
        message_scheduler,
        mut backend,
        mut rng,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestPayloadDispatcher,
        MockCoreAndLeaderProofsGenerator,
        MockProofsVerifier,
        MockKmsAdapter,
        RuntimeServiceId,
    >(
        settings.clone(),
        membership_stream,
        overwatch_handle.clone(),
        MockKmsAdapter,
        &sdp_relay,
        None,
        state_updater,
        seeded_release_delay_rng(),
    )
    .await;

    let mut backend_event_receiver = backend.subscribe_to_events();
    // Run the event loop of the service in a separate task.
    let settings_cloned = settings.clone();
    let join_handle = tokio::spawn(async move {
        let secret_pol_info_stream =
            post_initialize::<OncePolStreamProvider, RuntimeServiceId>(&overwatch_handle).await;

        let retiring_epoch = run_event_loop(
            inbound_relay,
            &mut blend_message_stream,
            secret_pol_info_stream,
            &mut remaining_epoch_stream,
            &settings_cloned,
            &mut backend,
            &TestPayloadDispatcher,
            &sdp_relay,
            &mut rng,
            CurrentEpoch::new(
                crypto_processor,
                message_scheduler.into(),
                current_public_info,
                pending_transactions,
                None,
            ),
            current_recovery_checkpoint,
        )
        .await;

        retire(
            blend_message_stream.map(|(msg, _)| msg),
            remaining_epoch_stream,
            backend,
            TestPayloadDispatcher,
            sdp_relay,
            rng,
            retiring_epoch,
        )
        .await;
    });

    // Send a new epoch with empty providers (zk: None).
    // This simulates an epoch where no providers are available.
    membership_sender
        .send(test_blend_epoch_state(
            1,
            MembershipInfo {
                membership: membership.clone(),
                zk: None,
            },
        ))
        .await
        .unwrap();

    wait_for_blend_backend_event(
        &mut backend_event_receiver,
        TestBlendBackendEvent::EpochTransitionCompleted,
    )
    .await;
    // The service should stop without panicking.
    join_handle
        .await
        .expect("the service should stop without panic on empty epoch");
}

/// Check that the service handles a non-empty new epoch where the local node
/// has no core path (`core_poq_generator = None`) without panicking. It should
/// retire gracefully.
#[expect(clippy::too_many_lines, reason = "Test function.")]
#[test_log::test(tokio::test)]
async fn stop_on_non_empty_epoch_without_local_core_path() {
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);

    // Create settings.
    let settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );

    // Prepare streams.
    let (inbound_relay, _inbound_message_sender) = new_stream();
    let (mut blend_message_stream, _blend_message_sender) = new_stream();
    let (membership_stream, membership_sender) = new_stream();

    // Send the initial membership info that the service will expect to receive
    // immediately.
    let membership_info = MembershipInfo {
        membership: membership.clone(),
        zk: Some(ZkInfo {
            root: ZkHash::ZERO,
            core_and_path_selectors: Some([(ZkHash::ZERO, false); CORE_MERKLE_TREE_HEIGHT]),
        }),
    };
    membership_sender
        .send(test_blend_epoch_state(0, membership_info.clone()))
        .await
        .unwrap();

    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();

    // Prepare dummy Overwatch resources.
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources();

    // Initialize the service.
    let (
        mut remaining_epoch_stream,
        current_public_info,
        crypto_processor,
        current_recovery_checkpoint,
        pending_transactions,
        message_scheduler,
        mut backend,
        mut rng,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestPayloadDispatcher,
        MockCoreAndLeaderProofsGenerator,
        MockProofsVerifier,
        MockKmsAdapter,
        RuntimeServiceId,
    >(
        settings.clone(),
        membership_stream,
        overwatch_handle.clone(),
        MockKmsAdapter,
        &sdp_relay,
        None,
        state_updater,
        seeded_release_delay_rng(),
    )
    .await;

    let mut backend_event_receiver = backend.subscribe_to_events();
    // Run the event loop of the service in a separate task.
    let settings_cloned = settings.clone();
    let join_handle = tokio::spawn(async move {
        let secret_pol_info_stream =
            post_initialize::<OncePolStreamProvider, RuntimeServiceId>(&overwatch_handle).await;

        let retiring_epoch = run_event_loop(
            inbound_relay,
            &mut blend_message_stream,
            secret_pol_info_stream,
            &mut remaining_epoch_stream,
            &settings_cloned,
            &mut backend,
            &TestPayloadDispatcher,
            &sdp_relay,
            &mut rng,
            CurrentEpoch::new(
                crypto_processor,
                message_scheduler.into(),
                current_public_info,
                pending_transactions,
                None,
            ),
            current_recovery_checkpoint,
        )
        .await;

        retire(
            blend_message_stream.map(|(msg, _)| msg),
            remaining_epoch_stream,
            backend,
            TestPayloadDispatcher,
            sdp_relay,
            rng,
            retiring_epoch,
        )
        .await;
    });

    // Send a new non-empty epoch without local core path.
    membership_sender
        .send(test_blend_epoch_state(
            1,
            MembershipInfo {
                membership,
                zk: Some(ZkInfo {
                    root: ZkHash::ZERO,
                    core_and_path_selectors: None,
                }),
            },
        ))
        .await
        .unwrap();

    wait_for_blend_backend_event(
        &mut backend_event_receiver,
        TestBlendBackendEvent::EpochTransitionCompleted,
    )
    .await;
    // The service should stop without panicking.
    join_handle
        .await
        .expect("the service should stop without panic when local core path is missing");
}

/// Verify that the proof generator produces proofs for the correct epoch,
/// and that those proofs are only accepted by a verifier for the same epoch.
#[expect(clippy::too_many_lines, reason = "Test function.")]
#[test_log::test(tokio::test)]
async fn test_proof_generator_epoch_binding() {
    let epoch_0 = 0.into();
    let epoch_1 = 1.into();
    let minimal_network_size = 1;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );

    // Create proof generators for epoch 0 and epoch 1.
    let public_info_0 = new_epoch_info(epoch_0, membership.clone(), &settings);
    let public_info_1 = new_epoch_info(epoch_1, membership.clone(), &settings);

    let mut generator_0 = new_crypto_processor(
        EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
            spent_core_quota: Quota::ZERO,
        },
        &public_info_0,
        (),
    );

    let mut generator_1 = new_crypto_processor(
        EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
            spent_core_quota: Quota::ZERO,
        },
        &public_info_1,
        (),
    );

    // Build a message with epoch 0 proofs.
    let payload = vec![];
    let msg_0 = generator_0
        .encapsulate_block_proposal_payload(&payload)
        .await
        .expect("encapsulation with epoch 0 must succeed");

    // Build a message with epoch 1 proofs.
    let msg_1 = generator_1
        .encapsulate_block_proposal_payload(&payload)
        .await
        .expect("encapsulation with epoch 1 must succeed");

    // Epoch 0 message should be decapsulable by epoch 0 processor.
    let (_, _, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();
    let scheduler_settings = scheduler_settings(&timing_settings(), settings.num_blend_layers);
    let mut scheduler_0 = EpochMessageScheduler::new(
        scheduler_epoch_info(&public_info_0),
        BlakeRng::from_entropy(),
        scheduler_settings,
    );
    let recovery_checkpoint = ServiceState::with_epoch(
        epoch_0,
        VecDeque::new(),
        EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info_0)),
        None,
        state_updater,
    )
    .unwrap();
    drop(handle_incoming_blend_message(
        (msg_0.clone(), epoch_0),
        &mut scheduler_0,
        None,
        generator_0.receiver(),
        None,
        recovery_checkpoint,
    ));
    assert_eq!(
        scheduler_0.release_delayer().unreleased_messages().len(),
        1,
        "Epoch 0 message must be scheduled by epoch 0 processor"
    );

    // Epoch 1 message should NOT be decapsulable by epoch 0 processor
    // (wrong PoQ proofs for epoch 0).
    let (_, _, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();
    let mut scheduler_0_only = EpochMessageScheduler::new(
        scheduler_epoch_info(&public_info_0),
        BlakeRng::from_entropy(),
        scheduler_settings,
    );
    let recovery_checkpoint = ServiceState::with_epoch(
        epoch_0,
        VecDeque::new(),
        EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info_0)),
        None,
        state_updater,
    )
    .unwrap();
    drop(handle_incoming_blend_message(
        (msg_1.clone(), epoch_0),
        &mut scheduler_0_only,
        None,
        generator_0.receiver(),
        None,
        recovery_checkpoint,
    ));
    assert_eq!(
        scheduler_0_only
            .release_delayer()
            .unreleased_messages()
            .len(),
        0,
        "Epoch 1 message must NOT be scheduled by epoch 0 processor"
    );

    // Epoch 1 message should be decapsulable by epoch 1 processor.
    let (_, _, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();
    let mut scheduler_1 = EpochMessageScheduler::new(
        scheduler_epoch_info(&public_info_1),
        BlakeRng::from_entropy(),
        scheduler_settings,
    );
    let recovery_checkpoint = ServiceState::with_epoch(
        epoch_1,
        VecDeque::new(),
        EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info_1)),
        None,
        state_updater,
    )
    .unwrap();
    drop(handle_incoming_blend_message(
        (msg_1, epoch_1),
        &mut scheduler_1,
        None,
        generator_1.receiver(),
        None,
        recovery_checkpoint,
    ));
    assert_eq!(
        scheduler_1.release_delayer().unreleased_messages().len(),
        1,
        "Epoch 1 message must be scheduled by epoch 1 processor"
    );
}

/// When `initialize` receives a `last_saved_state` whose epoch matches the
/// current membership epoch, the saved state is restored (e.g. `spent_quota`
/// is preserved). When the epoch does not match, a fresh state is created.
#[expect(clippy::too_many_lines, reason = "Test function.")]
#[test_log::test(tokio::test)]
async fn test_initialize_recovers_matching_saved_state() {
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let settings = {
        let mut settings = settings(
            local_private_key.clone(),
            u64::from(minimal_network_size).try_into().unwrap(),
            (),
            0,
        );
        // More than one layer, so the emission slots the recovery state counts and the
        // key indices the generator resumes from cannot be confused for each other.
        settings.num_blend_layers = 3.try_into().unwrap();
        settings
    };

    let initial_epoch = 0.into();

    // Matching epoch: saved state should be restored

    let (membership_stream, membership_sender) = new_stream();
    membership_sender
        .send(test_blend_epoch_state(
            0,
            MembershipInfo {
                membership: membership.clone(),
                zk: Some(ZkInfo {
                    root: ZkHash::ZERO,
                    core_and_path_selectors: Some([(ZkHash::ZERO, false); CORE_MERKLE_TREE_HEIGHT]),
                }),
            },
        ))
        .await
        .unwrap();

    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources();
    let (sdp_relay_1, _sdp_relay_receiver) = sdp_relay();

    // Build a pre-populated saved state with matching epoch and some spent quota.
    let public_info = new_epoch_info(initial_epoch, membership.clone(), &settings);
    let token_collector = EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info));
    let saved_state = ServiceState::with_epoch(
        initial_epoch,
        VecDeque::new(),
        token_collector,
        None,
        state_updater.clone(),
    )
    .unwrap();
    let mut updater = saved_state.start_updating();
    // Five cover messages' worth, at three layers each.
    updater.consume_core_quota(Quota::new::<15>());
    updater.queue_unencapsulated_transaction(b"transaction".to_vec());
    let saved_state = updater.commit_changes();

    reset_starting_core_key_indices();

    let (
        _remaining_epoch_stream,
        _current_public_info,
        _crypto_processor,
        recovered_checkpoint,
        _pending_transactions,
        _message_scheduler,
        _backend,
        _rng,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestPayloadDispatcher,
        MockCoreAndLeaderProofsGenerator,
        MockProofsVerifier,
        MockKmsAdapter,
        RuntimeServiceId,
    >(
        settings.clone(),
        membership_stream,
        overwatch_handle,
        MockKmsAdapter,
        &sdp_relay_1,
        Some(saved_state),
        state_updater,
        seeded_release_delay_rng(),
    )
    .await;

    assert_eq!(
        recovered_checkpoint.spent_quota(),
        Quota::new::<15>(),
        "Matching epoch: spent_quota should be restored from saved state"
    );
    assert_eq!(recovered_checkpoint.last_seen_epoch(), initial_epoch);
    // A transaction still waiting for a `PoW` solution has not been encapsulated
    // and so belongs to no epoch: a restart must not lose it.
    assert_eq!(
        recovered_checkpoint.pending_transactions().front(),
        Some(&b"transaction".to_vec()),
        "Matching epoch: a queued transaction should be restored from saved state"
    );
    // The restored quota is also what tells the core proof generator where to pick
    // up — it is counted in proofs, and the generator hands out one per key index.
    // Re-proving an index re-derives a key nullifier this node already put on the
    // wire, and peers drop that as a duplicate.
    assert_eq!(
        recorded_starting_core_key_indices(),
        vec![Quota::new::<15>()],
        "Matching epoch: the generator should resume from the spent quota"
    );

    // Mismatched epoch: fresh state should be created

    let (membership_stream2, membership_sender2) = new_stream();
    membership_sender2
        .send(test_blend_epoch_state(
            0,
            MembershipInfo {
                membership: membership.clone(),
                zk: Some(ZkInfo {
                    root: ZkHash::ZERO,
                    core_and_path_selectors: Some([(ZkHash::ZERO, false); CORE_MERKLE_TREE_HEIGHT]),
                }),
            },
        ))
        .await
        .unwrap();

    let (overwatch_handle2, _overwatch_cmd_receiver2, state_updater2, _state_receiver2) =
        dummy_overwatch_resources();
    let (sdp_relay2, _sdp_relay_receiver2) = sdp_relay();

    // Build a saved state for a *different* epoch (epoch 99) with spent quota.
    let stale_public_info = new_epoch_info(99.into(), membership.clone(), &settings);
    let stale_token_collector =
        EpochBlendingTokenCollector::new(&reward_epoch_info(&stale_public_info));
    let stale_state = ServiceState::with_epoch(
        99.into(),
        VecDeque::new(),
        stale_token_collector,
        None,
        state_updater2.clone(),
    )
    .unwrap();
    let mut updater = stale_state.start_updating();
    updater.consume_core_quota(Quota::new::<42>());
    updater.queue_unencapsulated_transaction(b"stale epoch transaction".to_vec());
    let stale_state = updater.commit_changes();

    let (
        _remaining_epoch_stream2,
        _current_public_info2,
        _crypto_processor2,
        recovered_checkpoint2,
        pending_messages2,
        _message_scheduler2,
        _backend2,
        _rng2,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestPayloadDispatcher,
        MockCoreAndLeaderProofsGenerator,
        MockProofsVerifier,
        MockKmsAdapter,
        RuntimeServiceId,
    >(
        settings.clone(),
        membership_stream2,
        overwatch_handle2,
        MockKmsAdapter,
        &sdp_relay2,
        Some(stale_state),
        state_updater2,
        seeded_release_delay_rng(),
    )
    .await;

    assert_eq!(
        recovered_checkpoint2.spent_quota(),
        Quota::ZERO,
        "Mismatched epoch: spent_quota should be 0 for fresh state"
    );
    assert_eq!(
        recovered_checkpoint2.last_seen_epoch(),
        initial_epoch,
        "Mismatched epoch: should track the current epoch, not the stale one"
    );
    // The rest of a stale state belongs to the epoch it was saved under, but a
    // transaction still waiting for a `PoW` solution has not been encapsulated
    // and so belongs to none.
    assert_eq!(
        recovered_checkpoint2.pending_transactions().front(),
        Some(&b"stale epoch transaction".to_vec()),
        "Mismatched epoch: a queued transaction should outlive the state that carried it"
    );
    assert_eq!(
        pending_messages2.transactions().next(),
        Some(&b"stale epoch transaction".to_vec()),
        "Mismatched epoch: the queue handed to the event loop should carry it too"
    );
}

/// The tokens collected during an epoch are worth an activity proof, and a
/// restart into the *next* epoch must not throw them away.
#[test_log::test(tokio::test)]
async fn test_initialize_submits_activity_proof_for_the_previous_epoch() {
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let mut settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    // A long epoch makes the core quota, and with it the activity threshold, high
    // enough for a single token to clear it.
    settings.time.rounds_per_epoch = 648_000.try_into().unwrap();

    // Saved under epoch 0; the node comes back up in epoch 1.
    let saved_epoch = 0.into();
    let current_epoch = Epoch::new(1);

    let membership_info = MembershipInfo {
        membership: membership.clone(),
        zk: Some(ZkInfo {
            root: ZkHash::ZERO,
            core_and_path_selectors: Some([(ZkHash::ZERO, false); CORE_MERKLE_TREE_HEIGHT]),
        }),
    };
    let (membership_stream, membership_sender) = new_stream();
    membership_sender
        .send(test_blend_epoch_state(1, membership_info))
        .await
        .unwrap();

    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources();
    let (sdp_relay, mut sdp_relay_receiver) = sdp_relay();

    let saved_public_info = new_epoch_info(saved_epoch, membership.clone(), &settings);
    let saved_state = ServiceState::with_epoch(
        saved_epoch,
        VecDeque::new(),
        EpochBlendingTokenCollector::new(&reward_epoch_info(&saved_public_info)),
        None,
        state_updater.clone(),
    )
    .unwrap();
    let mut updater = saved_state.start_updating();
    updater.collect_current_epoch_tokens(once(BlendingToken::new(
        Ed25519Key::from_bytes(&[0; _]).public_key(),
        VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
        VerifiedProofOfSelection::from_bytes_unchecked([0; _]),
    )));
    let saved_state = updater.commit_changes();

    let (
        _remaining_epoch_stream,
        _current_public_info,
        _crypto_processor,
        recovered_checkpoint,
        _pending_transactions,
        _message_scheduler,
        _backend,
        _rng,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestPayloadDispatcher,
        MockCoreAndLeaderProofsGenerator,
        MockProofsVerifier,
        MockKmsAdapter,
        RuntimeServiceId,
    >(
        settings.clone(),
        membership_stream,
        overwatch_handle,
        MockKmsAdapter,
        &sdp_relay,
        Some(saved_state),
        state_updater,
        seeded_release_delay_rng(),
    )
    .await;

    assert_eq!(
        recovered_checkpoint.last_seen_epoch(),
        current_epoch,
        "the recovered state should track the epoch the node came back up in"
    );
    assert!(
        matches!(
            sdp_relay_receiver.try_recv(),
            Ok(lb_sdp_service::SdpMessage::PostActivity {
                metadata: ActivityMetadata::Blend(_),
            })
        ),
        "the previous epoch's tokens should be submitted as an activity proof, not dropped"
    );
}

/// A state older than the immediately preceding epoch is past submitting for.
///
/// The counterpart to
/// [`test_initialize_submits_activity_proof_for_the_previous_epoch`].
#[test_log::test(tokio::test)]
async fn test_initialize_drops_activity_proof_older_than_one_epoch() {
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let mut settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    settings.time.rounds_per_epoch = 648_000.try_into().unwrap();

    let membership_info = MembershipInfo {
        membership: membership.clone(),
        zk: Some(ZkInfo {
            root: ZkHash::ZERO,
            core_and_path_selectors: Some([(ZkHash::ZERO, false); CORE_MERKLE_TREE_HEIGHT]),
        }),
    };
    // The node comes back up in epoch 3, so a state saved under epoch 1 is
    // genuinely two epochs behind rather than merely different.
    let (membership_stream, membership_sender) = new_stream();
    membership_sender
        .send(test_blend_epoch_state(3, membership_info))
        .await
        .unwrap();

    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources();
    let (sdp_relay, mut sdp_relay_receiver) = sdp_relay();

    let stale_epoch = 1.into();
    let stale_public_info = new_epoch_info(stale_epoch, membership.clone(), &settings);
    let stale_state = ServiceState::with_epoch(
        stale_epoch,
        VecDeque::new(),
        EpochBlendingTokenCollector::new(&reward_epoch_info(&stale_public_info)),
        None,
        state_updater.clone(),
    )
    .unwrap();
    let mut updater = stale_state.start_updating();
    updater.collect_current_epoch_tokens(once(BlendingToken::new(
        Ed25519Key::from_bytes(&[0; _]).public_key(),
        VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
        VerifiedProofOfSelection::from_bytes_unchecked([0; _]),
    )));
    let stale_state = updater.commit_changes();

    let (.., _rng) = initialize::<
        NodeId,
        TestBlendBackend,
        TestPayloadDispatcher,
        MockCoreAndLeaderProofsGenerator,
        MockProofsVerifier,
        MockKmsAdapter,
        RuntimeServiceId,
    >(
        settings.clone(),
        membership_stream,
        overwatch_handle,
        MockKmsAdapter,
        &sdp_relay,
        Some(stale_state),
        state_updater,
        seeded_release_delay_rng(),
    )
    .await;

    assert!(
        sdp_relay_receiver.try_recv().is_err(),
        "a collector more than one epoch old should be dropped, not submitted"
    );
}

/// A block proposal that arrives before this epoch's secret `PoL` info still
/// goes out once it lands.
///
/// Leadership proofs only become possible when the secret `PoL` info reaches
/// the processor, and that regularly happens *after* the first proposal does —
/// most visibly at startup, when the node wins the very first slot it is asked
/// to lead. The proposal used to be encapsulated where it arrived, fail, and be
/// dropped with an error, silently losing a block this node had just produced.
#[test_log::test(tokio::test)]
async fn a_proposal_arriving_before_the_pol_info_is_still_sent() {
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let mut settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    // No cover traffic, so the only message that can come out is the proposal.
    // See the `PoW` liveness test for why this quota silences it.
    settings.num_blend_layers = NonZeroU64::try_from(2).unwrap();
    settings.scheduler.cover.message_frequency_per_round = 0.05.try_into().unwrap();

    let (inbound_relay, inbound_message_sender) = new_stream();
    let (mut blend_message_stream, _blend_message_sender) = new_stream();
    let (membership_stream, membership_sender) = new_stream();

    let membership_info = MembershipInfo {
        membership,
        zk: Some(ZkInfo {
            root: ZkHash::ZERO,
            core_and_path_selectors: Some([(ZkHash::ZERO, false); CORE_MERKLE_TREE_HEIGHT]),
        }),
    };
    membership_sender
        .send(test_blend_epoch_state(0, membership_info))
        .await
        .unwrap();

    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources();

    // Both installed before the service exists, so nothing is missed.
    let pol_gate = PolGate::setup();
    let mut outgoing_messages = outgoing_messages_recorder();

    let (
        mut remaining_epoch_stream,
        current_public_info,
        crypto_processor,
        current_recovery_checkpoint,
        pending_transactions,
        message_scheduler,
        mut backend,
        mut rng,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestPayloadDispatcher,
        PolAwareProofsGenerator,
        MockProofsVerifier,
        MockKmsAdapter,
        RuntimeServiceId,
    >(
        settings.clone(),
        membership_stream,
        overwatch_handle.clone(),
        MockKmsAdapter,
        &sdp_relay,
        None,
        state_updater,
        seeded_release_delay_rng(),
    )
    .await;

    tokio::spawn(async move {
        let secret_pol_info_stream =
            post_initialize::<GatedPolStreamProvider, RuntimeServiceId>(&overwatch_handle).await;
        run_event_loop(
            inbound_relay,
            &mut blend_message_stream,
            secret_pol_info_stream,
            &mut remaining_epoch_stream,
            &settings,
            &mut backend,
            &TestPayloadDispatcher,
            &sdp_relay,
            &mut rng,
            CurrentEpoch::new(
                crypto_processor,
                message_scheduler.into(),
                current_public_info,
                pending_transactions,
                None,
            ),
            current_recovery_checkpoint,
        )
        .await;
    });

    // The gate is shut, so the leadership branch has nothing to give: this is the
    // window the proposal used to die in.
    inbound_message_sender
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
    inbound_message_sender
        .send(ServiceMessage::GetPendingTransactions { reply })
        .await
        .unwrap();
    answered.await.unwrap();

    pol_gate.release();

    expect_outgoing_message(
        &mut outgoing_messages,
        "the proposal should have been held until leadership proofs were possible",
    )
    .await;
}

/// A message queued before an epoch rotation still goes out afterwards, under
/// the epoch it was minted for.
///
/// The previous epoch keeps releasing through its own scheduler for the length
/// of its transition period, and each message it releases is published under
/// that epoch so it reaches the peers still negotiated for it. Publishing it
/// under the new epoch would fail their `PoQ` check and earn this node a
/// `SpamReason::InvalidProofOfQuota`.
///
/// What is pinned here is that the message survives the rotation and is
/// published under the epoch it was minted for. *Which* scheduler releases it
/// is not: both the current epoch before the rotation and the previous epoch
/// after it publish under epoch 0, and which one gets there first depends on
/// how the round clock falls against an epoch arriving over a channel. Seeding
/// the delayer fixes the round but not that race — measured at roughly three
/// runs in eight exercising the previous-epoch path.
///
/// Pinning it needs a test that does not go through the loop at all, on
/// [`CurrentEpochDuringTransition::next_event`] directly.
#[test_log::test(tokio::test)]
#[expect(clippy::too_many_lines, reason = "Test function.")]
async fn the_previous_epoch_keeps_releasing_under_its_own_epoch() {
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let mut settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    // No cover traffic, so the only message that can come out is the queued
    // transaction. See the `PoW` liveness test for why this quota silences it.
    settings.num_blend_layers = NonZeroU64::try_from(2).unwrap();
    settings.scheduler.cover.message_frequency_per_round = 0.05.try_into().unwrap();
    let (inbound_relay, inbound_message_sender) = new_stream();
    let (mut blend_message_stream, _blend_message_sender) = new_stream();
    let (membership_stream, membership_sender) = new_stream();

    let membership_info = MembershipInfo {
        membership,
        zk: Some(ZkInfo {
            root: ZkHash::ZERO,
            core_and_path_selectors: Some([(ZkHash::ZERO, false); CORE_MERKLE_TREE_HEIGHT]),
        }),
    };
    membership_sender
        .send(test_blend_epoch_state(0, membership_info.clone()))
        .await
        .unwrap();

    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources();

    // Both installed before the service exists, so nothing is missed.
    let mut published_epochs = published_epochs_recorder();

    let (
        mut remaining_epoch_stream,
        current_public_info,
        crypto_processor,
        current_recovery_checkpoint,
        pending_transactions,
        message_scheduler,
        mut backend,
        mut rng,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestPayloadDispatcher,
        MockCoreAndLeaderProofsGenerator,
        MockProofsVerifier,
        MockKmsAdapter,
        RuntimeServiceId,
    >(
        settings.clone(),
        membership_stream,
        overwatch_handle.clone(),
        MockKmsAdapter,
        &sdp_relay,
        None,
        state_updater,
        seeded_release_delay_rng(),
    )
    .await;

    tokio::spawn(async move {
        let secret_pol_info_stream =
            post_initialize::<OncePolStreamProvider, RuntimeServiceId>(&overwatch_handle).await;
        run_event_loop(
            inbound_relay,
            &mut blend_message_stream,
            secret_pol_info_stream,
            &mut remaining_epoch_stream,
            &settings,
            &mut backend,
            &TestPayloadDispatcher,
            &sdp_relay,
            &mut rng,
            CurrentEpoch::new(
                crypto_processor,
                message_scheduler.into(),
                current_public_info,
                pending_transactions,
                None,
            ),
            current_recovery_checkpoint,
        )
        .await;
    });

    // Queued under epoch 0, and released on a round that has not come yet.
    inbound_message_sender
        .send(ServiceMessage::Blend(BlendPayload::Transaction(
            b"transaction".to_vec(),
        )))
        .await
        .unwrap();

    // Answering a request proves the service went round the inbound arm again,
    // so the transaction ahead of it is already queued when the epoch turns.
    let (reply, answered) = oneshot::channel();
    inbound_message_sender
        .send(ServiceMessage::GetPendingTransactions { reply })
        .await
        .unwrap();
    answered.await.unwrap();

    membership_sender
        .send(test_blend_epoch_state(1, membership_info))
        .await
        .unwrap();

    // The rotation has to be observed before anything is asserted: a release
    // that happened while epoch 0 was still current is also published under
    // epoch 0, and would satisfy the assertion without the previous epoch
    // having released anything at all.
    // The first message out is the one the previous epoch still had queued: the
    // rotation lands before it (measured at ~440µs against ~580µs), and nothing
    // else is due, since this test's quota leaves no room for cover traffic.
    //
    // Generous next to the release round the message has to wait for, and only
    // ever reached when the assertion has already failed.
    let published_under = tokio::time::timeout(Duration::from_secs(10), published_epochs.recv())
        .await
        .expect("timed out: the previous epoch should still release what it had queued")
        .expect("service stopped publishing");
    assert_eq!(
        published_under,
        Epoch::from(0),
        "a message minted under the previous epoch must be published under it, not the new one"
    );
}

/// A block proposal that arrives before this epoch's secret `PoL` info still
/// goes out once it lands.
///
/// Leadership proofs only become possible when the secret `PoL` info reaches
/// the processor, and that regularly happens *after* the first proposal does —
/// most visibly at startup, when the node wins the very first slot it is asked
/// to lead. The proposal used to be encapsulated where it arrived, fail, and be
/// dropped with an error, silently losing a block this node had just produced.
#[test_log::test(tokio::test)]
async fn a_message_that_can_never_be_sent_does_not_block_the_rest() {
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let mut settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    // No cover traffic, so the only message that can come out is the one this
    // test expects. See the `PoW` liveness test for why this quota silences it.
    settings.num_blend_layers = NonZeroU64::try_from(2).unwrap();
    settings.scheduler.cover.message_frequency_per_round = 0.05.try_into().unwrap();

    let (inbound_relay, inbound_message_sender) = new_stream();
    let (mut blend_message_stream, _blend_message_sender) = new_stream();
    let (membership_stream, membership_sender) = new_stream();

    let membership_info = MembershipInfo {
        membership,
        zk: Some(ZkInfo {
            root: ZkHash::ZERO,
            core_and_path_selectors: Some([(ZkHash::ZERO, false); CORE_MERKLE_TREE_HEIGHT]),
        }),
    };
    membership_sender
        .send(test_blend_epoch_state(0, membership_info))
        .await
        .unwrap();

    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources();

    // Installed before the service exists, so nothing is missed.
    let mut outgoing_messages = outgoing_messages_recorder();

    let (
        mut remaining_epoch_stream,
        current_public_info,
        crypto_processor,
        current_recovery_checkpoint,
        pending_transactions,
        message_scheduler,
        mut backend,
        mut rng,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestPayloadDispatcher,
        MockCoreAndLeaderProofsGenerator,
        MockProofsVerifier,
        MockKmsAdapter,
        RuntimeServiceId,
    >(
        settings.clone(),
        membership_stream,
        overwatch_handle.clone(),
        MockKmsAdapter,
        &sdp_relay,
        None,
        state_updater,
        seeded_release_delay_rng(),
    )
    .await;

    tokio::spawn(async move {
        let secret_pol_info_stream =
            post_initialize::<OncePolStreamProvider, RuntimeServiceId>(&overwatch_handle).await;
        run_event_loop(
            inbound_relay,
            &mut blend_message_stream,
            secret_pol_info_stream,
            &mut remaining_epoch_stream,
            &settings,
            &mut backend,
            &TestPayloadDispatcher,
            &sdp_relay,
            &mut rng,
            CurrentEpoch::new(
                crypto_processor,
                message_scheduler.into(),
                current_public_info,
                pending_transactions,
                None,
            ),
            current_recovery_checkpoint,
        )
        .await;
    });

    // One byte over what a payload can hold, so encapsulating it fails the same
    // way however long it waits.
    inbound_message_sender
        .send(ServiceMessage::Blend(BlendPayload::BlockProposal(vec![
            0;
            MAX_PAYLOAD_BODY_SIZE + 1
        ])))
        .await
        .unwrap();
    inbound_message_sender
        .send(ServiceMessage::Blend(BlendPayload::Transaction(
            b"transaction".to_vec(),
        )))
        .await
        .unwrap();

    expect_outgoing_message(
        &mut outgoing_messages,
        "the transaction behind the oversized proposal should still go out",
    )
    .await;
}

/// A transaction waits for a `PoW` solution without holding up anything else.
///
/// The puzzle search behind a transaction's layer proofs takes long enough that
/// awaiting it where the transaction arrives would stop the service dead: no
/// incoming messages, no release rounds, no epoch events, no cover traffic. So
/// the transaction is queued and mined for on its own branch of the event loop,
/// and this test pins that down by getting a block proposal all the way out
/// while the transaction is still waiting for its solution.
#[test_log::test(tokio::test)]
#[expect(clippy::too_many_lines, reason = "Test function.")]
async fn a_transaction_awaiting_a_pow_solution_does_not_stall_the_event_loop() {
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let mut settings = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    // No cover traffic, so every message the service sends is one this test put
    // in and the count below means what it says. The frequency itself must stay
    // positive (`C * ß_c > 0`), so the silence comes from the quota instead:
    // `Q_c = ceil(10 rounds * 0.05 * 2 layers / 2 nodes) = ceil(0.5) = 1`, and
    // the scheduler floors its cover count at `Q_c / num_blend_layers = 0`.
    // Anything in `(0, 0.1]` gives the same quota, so the halfway point keeps
    // float drift well clear of either rounding boundary.
    settings.num_blend_layers = NonZeroU64::try_from(2).unwrap();
    settings.scheduler.cover.message_frequency_per_round = 0.05.try_into().unwrap();

    let (inbound_relay, inbound_message_sender) = new_stream();
    let (mut blend_message_stream, _blend_message_sender) = new_stream();
    let (membership_stream, membership_sender) = new_stream();

    let membership_info = MembershipInfo {
        membership,
        zk: Some(ZkInfo {
            root: ZkHash::ZERO,
            core_and_path_selectors: Some([(ZkHash::ZERO, false); CORE_MERKLE_TREE_HEIGHT]),
        }),
    };
    membership_sender
        .send(test_blend_epoch_state(0, membership_info))
        .await
        .unwrap();

    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources();

    // Both installed before the service exists, so nothing is missed.
    let pow_gate = PowGate::setup();
    let mut outgoing_messages = outgoing_messages_recorder();

    let (
        mut remaining_epoch_stream,
        current_public_info,
        crypto_processor,
        current_recovery_checkpoint,
        pending_transactions,
        message_scheduler,
        mut backend,
        mut rng,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestPayloadDispatcher,
        GatedPowProofsGenerator,
        MockProofsVerifier,
        MockKmsAdapter,
        RuntimeServiceId,
    >(
        settings.clone(),
        membership_stream,
        overwatch_handle.clone(),
        MockKmsAdapter,
        &sdp_relay,
        None,
        state_updater,
        seeded_release_delay_rng(),
    )
    .await;

    tokio::spawn(async move {
        let secret_pol_info_stream =
            post_initialize::<OncePolStreamProvider, RuntimeServiceId>(&overwatch_handle).await;
        run_event_loop(
            inbound_relay,
            &mut blend_message_stream,
            secret_pol_info_stream,
            &mut remaining_epoch_stream,
            &settings,
            &mut backend,
            &TestPayloadDispatcher,
            &sdp_relay,
            &mut rng,
            CurrentEpoch::new(
                crypto_processor,
                message_scheduler.into(),
                current_public_info,
                pending_transactions,
                None,
            ),
            current_recovery_checkpoint,
        )
        .await;
    });

    // The transaction goes in first, so if the loop blocked on mining, the
    // proposal queued behind it could never get out.
    inbound_message_sender
        .send(ServiceMessage::Blend(BlendPayload::Transaction(
            b"transaction".to_vec(),
        )))
        .await
        .unwrap();
    inbound_message_sender
        .send(ServiceMessage::Blend(BlendPayload::BlockProposal(
            b"proposal".to_vec(),
        )))
        .await
        .unwrap();

    // The proposal gets out while the transaction is still mining. Nothing else
    // can be in flight: cover traffic is off and the gate is shut. A stalled
    // loop shows up as a timeout here rather than as a hung test.
    expect_outgoing_message(
        &mut outgoing_messages,
        "the block proposal should be sent while the transaction is still mining",
    )
    .await;

    // And the transaction follows once its solution lands.
    pow_gate.release();
    expect_outgoing_message(
        &mut outgoing_messages,
        "the transaction should be sent once its PoW solution lands",
    )
    .await;
}

/// Waits for the service to send one message onwards, failing rather than
/// hanging if it never does.
async fn expect_outgoing_message(
    outgoing_messages: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
    expectation: &str,
) {
    // Generous next to the release round the message has to wait for, and only
    // ever reached when the assertion has already failed.
    const GRACE: Duration = Duration::from_secs(10);

    tokio::time::timeout(GRACE, outgoing_messages.recv())
        .await
        .unwrap_or_else(|_| panic!("timed out: {expectation}"))
        .unwrap_or_else(|| panic!("service stopped sending: {expectation}"));
}
