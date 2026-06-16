mod utils;

use futures::StreamExt as _;
use lb_blend::{
    message::reward::{ActivityProof, BlendingToken, EpochBlendingTokenCollector},
    proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection},
    scheduling::{
        EpochMessageScheduler, epoch::EpochEvent,
        message_blend::crypto::EpochCryptographicProcessorSettings,
    },
};
use lb_chain_service::Epoch;
use lb_core::{codec::SerializeOp as _, crypto::ZkHash, sdp::ActivityMetadata};
use lb_groth16::AdditiveGroup as _;
use lb_key_management_system_service::keys::Ed25519Key;
use lb_poq::CORE_MERKLE_TREE_HEIGHT;
use lb_utils::blake_rng::BlakeRng;
use rand::SeedableRng as _;

use crate::{
    core::{
        HandleEpochEventOutput,
        backends::BlendBackend,
        handle_epoch_event, handle_epoch_transition_expired, handle_incoming_blend_message,
        initialize, post_initialize, retire, run_event_loop,
        state::ServiceState,
        tests::utils::{
            MockKmsAdapter, MockProofsVerifier, NodeId, TestBlendBackend, TestBlendBackendEvent,
            TestNetworkAdapter, dummy_overwatch_resources, dummy_pol_private_inputs,
            new_crypto_processor, new_epoch_info, new_membership, new_stream,
            recorded_set_epoch_private_calls, reset_set_epoch_private_calls, reward_epoch_info,
            scheduler_epoch_info, scheduler_settings, sdp_relay, settings, timing_settings,
            wait_for_blend_backend_event,
        },
    },
    epoch::{CoreEpochInfo, CoreEpochPublicInfo},
    epoch_info::PolEpochInfo,
    membership::{MembershipInfo, ZkInfo, chain::BlendEpochState},
    message::NetworkMessage,
    test_utils::{crypto::MockCoreAndLeaderProofsGenerator, epoch::OncePolStreamProvider},
};

type RuntimeServiceId = ();

fn test_blend_epoch_state(
    epoch: u32,
    membership_info: MembershipInfo<NodeId>,
) -> BlendEpochState<NodeId> {
    BlendEpochState {
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
    let (settings, _recovery_file) = settings(
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
        },
        &public_info,
        (),
    );
    let payload = NetworkMessage {
        message: vec![],
        broadcast_settings: (),
    }
    .to_bytes()
    .expect("NetworkMessage serialization must succeed");
    let msg = processor
        .encapsulate_data_payload(&payload)
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
        EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info)),
        None,
        state_updater,
    )
    .unwrap();
    let recovery_checkpoint = handle_incoming_blend_message(
        (msg.clone().into(), 0.into()),
        &mut scheduler,
        None,
        &processor,
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
    // number.
    epoch = epoch.strict_add(1.into());
    let public_info = new_epoch_info(epoch, membership.clone(), &settings);
    let mut new_processor = new_crypto_processor(
        EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
        },
        &public_info,
        (),
    );
    let (mut new_scheduler, mut scheduler) =
        scheduler.rotate_epoch(scheduler_epoch_info(&public_info), scheduler_settings);
    let (_, _, _, _, current_token_collector, _, state_updater) =
        recovery_checkpoint.into_components();
    let (new_token_collector, old_token_collector) =
        current_token_collector.rotate_epoch(&reward_epoch_info(&public_info));

    // Check that decapsulating the same message fails with the new processor
    // but succeeds with the old one. Also, it should be scheduled in the old
    // scheduler.
    let recovery_checkpoint = ServiceState::with_epoch(
        epoch,
        new_token_collector,
        Some(old_token_collector),
        state_updater,
    )
    .unwrap();
    let recovery_checkpoint = handle_incoming_blend_message(
        (msg.clone().into(), 0.into()),
        &mut new_scheduler,
        Some(&mut scheduler),
        &new_processor,
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
        .encapsulate_data_payload(&payload)
        .await
        .expect("encapsulation must succeed");
    let recovery_checkpoint = handle_incoming_blend_message(
        (msg.into(), 1.into()),
        &mut new_scheduler,
        Some(&mut scheduler),
        &new_processor,
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
        },
        &new_epoch_info(epoch, membership, &settings),
        (),
    );
    let msg = future_processor
        .encapsulate_data_payload(&payload)
        .await
        .expect("encapsulation must succeed");
    let recovery_checkpoint = handle_incoming_blend_message(
        (msg.into(), 2.into()),
        &mut new_scheduler,
        Some(&mut scheduler),
        &new_processor,
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
    let (settings, _recovery_file) = settings(
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
        },
        &public_info,
        (),
    );

    // One logical data message, serialized once...
    let payload = NetworkMessage {
        message: vec![],
        broadcast_settings: (),
    }
    .to_bytes()
    .expect("NetworkMessage serialization must succeed");

    // ...encapsulated twice. Each call draws fresh randomness, so these are two
    // distinct encapsulated messages (different identifiers) that the swarm
    // would forward independently — exactly the replicas the service produces.
    let replica_a = processor
        .encapsulate_data_payload(&payload)
        .await
        .expect("encapsulation must succeed");
    let replica_b = processor
        .encapsulate_data_payload(&payload)
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
        EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info)),
        None,
        state_updater,
    )
    .unwrap();

    // First replica: decapsulated and recorded as an unsent processed message.
    let recovery_checkpoint = handle_incoming_blend_message(
        (replica_a.into(), epoch),
        &mut scheduler,
        None,
        &processor,
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
        (replica_b.into(), epoch),
        &mut scheduler,
        None,
        &processor,
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
    let (settings, _recovery_file) = settings(
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
        },
        &public_info_0,
        (),
    );

    let payload = NetworkMessage {
        message: vec![],
        broadcast_settings: (),
    }
    .to_bytes()
    .expect("NetworkMessage serialization must succeed");
    let msg = processor_0
        .encapsulate_data_payload(&payload)
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
        EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info_1)),
        None,
        state_updater,
    )
    .unwrap();

    // Send epoch 0 message claiming to be for epoch 1.
    // Signature is valid (built correctly) but PoQ will fail because the
    // MockProofsVerifier for epoch 1 expects epoch 1 proofs.
    drop(handle_incoming_blend_message(
        (msg.into(), epoch_1),
        &mut scheduler,
        None,
        &processor_1,
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
    let (mut settings, _recovery_file) = settings(
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
    let mut backend = <TestBlendBackend as BlendBackend<_, _, _>>::new(
        settings.clone(),
        overwatch_handle.clone(),
        (public_info.membership.clone(), public_info.epoch),
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
    handle_epoch_transition_expired::<_, NodeId, BlakeRng, _>(
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

#[test_log::test(tokio::test)]
#[expect(clippy::too_many_lines, reason = "Test function.")]
async fn test_handle_epoch_event() {
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();

    // Prepare components for epoch event handling.
    let epoch = 0.into();
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let (settings, _recovery_file) = settings(
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
    let mut backend = <TestBlendBackend as BlendBackend<_, _, _>>::new(
        settings.clone(),
        overwatch_handle.clone(),
        (public_info.membership.clone(), public_info.epoch),
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
        ServiceState::with_epoch(epoch, token_collector, None, state_updater.clone()).unwrap(),
        &mut backend,
        &sdp_relay,
        None,
    )
    .await;
    let HandleEpochEventOutput::Transitioning {
        new_crypto_processor,
        new_scheduler,
        old_scheduler,
        new_epoch_info,
        new_recovery_checkpoint,
        old_crypto_processor,
    } = output
    else {
        panic!("expected Transitioning output");
    };
    assert_eq!(new_crypto_processor.epoch(), epoch.strict_add(1.into()));
    assert_eq!(old_crypto_processor.epoch(), epoch);
    assert_eq!(
        new_scheduler.release_delayer().unreleased_messages().len(),
        0
    );
    assert_eq!(
        old_scheduler.release_delayer().unreleased_messages().len(),
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
        None,
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
        None,
    )
    .await;
    let HandleEpochEventOutput::Retiring {
        old_crypto_processor,
        ..
    } = output
    else {
        panic!("expected Retiring output");
    };
    assert_eq!(old_crypto_processor.epoch(), epoch.strict_add(1.into()));
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
    let (settings, _recovery_file) = settings(
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
    let mut backend = <TestBlendBackend as BlendBackend<_, _, _>>::new(
        settings.clone(),
        overwatch_handle.clone(),
        (public_info.membership.clone(), public_info.epoch),
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
        ServiceState::with_epoch(epoch, token_collector, None, state_updater.clone()).unwrap(),
        &mut backend,
        &sdp_relay,
        None,
    )
    .await;

    let HandleEpochEventOutput::Transitioning {
        new_crypto_processor,
        old_crypto_processor,
        new_epoch_info: returned_epoch_info,
        ..
    } = output
    else {
        panic!("expected Transitioning output");
    };

    // A fresh generator is built for the new epoch, and the previous one is
    // retained for the old epoch.
    assert_eq!(new_crypto_processor.epoch(), new_epoch);
    assert_eq!(old_crypto_processor.epoch(), epoch);
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
    let (settings, _recovery_file) = settings(
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
    let mut backend = <TestBlendBackend as BlendBackend<_, _, _>>::new(
        settings.clone(),
        overwatch_handle.clone(),
        (public_info.membership.clone(), public_info.epoch),
        BlakeRng::from_entropy(),
    );
    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();

    let secret_info = PolEpochInfo {
        epoch: secret_epoch,
        poq_private_inputs: dummy_pol_private_inputs(),
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
        ServiceState::with_epoch(epoch, token_collector, None, state_updater.clone()).unwrap(),
        &mut backend,
        &sdp_relay,
        Some(&secret_info),
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
    let (settings, _recovery_file) = settings(
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
    let mut backend = <TestBlendBackend as BlendBackend<_, _, _>>::new(
        settings.clone(),
        overwatch_handle.clone(),
        (public_info.membership.clone(), public_info.epoch),
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
        ServiceState::with_epoch(epoch, token_collector, None, state_updater.clone()).unwrap(),
        &mut backend,
        &sdp_relay,
        None,
    )
    .await;
    let HandleEpochEventOutput::Retiring {
        old_crypto_processor,
        ..
    } = output
    else {
        panic!("expected Retiring output for Empty epoch");
    };
    // The old processor/info should be from the epoch we were on before
    // the empty epoch arrived.
    assert_eq!(old_crypto_processor.epoch(), epoch);
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
    let (settings, _recovery_file) = settings(
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
    let mut backend = <TestBlendBackend as BlendBackend<_, _, _>>::new(
        settings.clone(),
        overwatch_handle.clone(),
        (public_info.membership.clone(), public_info.epoch),
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
        ServiceState::with_epoch(epoch, token_collector, None, state_updater.clone()).unwrap(),
        &mut backend,
        &sdp_relay,
        None,
    )
    .await;

    let HandleEpochEventOutput::Retiring {
        old_crypto_processor,
        ..
    } = output
    else {
        panic!("expected Retiring output for NonEmpty epoch without local core path");
    };

    assert_eq!(old_crypto_processor.epoch(), epoch);
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
    let (settings, _recovery_file) = settings(
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
        message_scheduler,
        mut backend,
        mut rng,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestNetworkAdapter,
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
    )
    .await;
    let mut backend_event_receiver = backend.subscribe_to_events();

    // Run the event loop of the service in a separate task.
    let settings_cloned = settings.clone();
    let join_handle = tokio::spawn(async move {
        let secret_pol_info_stream =
            post_initialize::<OncePolStreamProvider, RuntimeServiceId>(&overwatch_handle).await;

        let (
            old_epoch_crypto_processor,
            old_epoch_message_scheduler,
            old_epoch_blending_token_collector,
        ) = run_event_loop(
            inbound_relay,
            &mut blend_message_stream,
            secret_pol_info_stream,
            &mut remaining_epoch_stream,
            &settings_cloned,
            &mut backend,
            &TestNetworkAdapter,
            &sdp_relay,
            message_scheduler.into(),
            &mut rng,
            crypto_processor,
            current_public_info,
            current_recovery_checkpoint,
        )
        .await;

        retire(
            blend_message_stream.map(|(msg, _)| msg),
            remaining_epoch_stream,
            backend,
            TestNetworkAdapter,
            sdp_relay,
            old_epoch_message_scheduler,
            rng,
            old_epoch_blending_token_collector,
            old_epoch_crypto_processor,
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
    let (settings, _recovery_file) = settings(
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
        message_scheduler,
        mut backend,
        mut rng,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestNetworkAdapter,
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
    )
    .await;

    let mut backend_event_receiver = backend.subscribe_to_events();
    // Run the event loop of the service in a separate task.
    let settings_cloned = settings.clone();
    let join_handle = tokio::spawn(async move {
        let secret_pol_info_stream =
            post_initialize::<OncePolStreamProvider, RuntimeServiceId>(&overwatch_handle).await;

        let (
            old_epoch_crypto_processor,
            old_epoch_message_scheduler,
            old_epoch_blending_token_collector,
        ) = run_event_loop(
            inbound_relay,
            &mut blend_message_stream,
            secret_pol_info_stream,
            &mut remaining_epoch_stream,
            &settings_cloned,
            &mut backend,
            &TestNetworkAdapter,
            &sdp_relay,
            message_scheduler.into(),
            &mut rng,
            crypto_processor,
            current_public_info,
            current_recovery_checkpoint,
        )
        .await;

        retire(
            blend_message_stream.map(|(msg, _)| msg),
            remaining_epoch_stream,
            backend,
            TestNetworkAdapter,
            sdp_relay,
            old_epoch_message_scheduler,
            rng,
            old_epoch_blending_token_collector,
            old_epoch_crypto_processor,
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
    let (settings, _recovery_file) = settings(
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
        message_scheduler,
        mut backend,
        mut rng,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestNetworkAdapter,
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
    )
    .await;

    let mut backend_event_receiver = backend.subscribe_to_events();
    // Run the event loop of the service in a separate task.
    let settings_cloned = settings.clone();
    let join_handle = tokio::spawn(async move {
        let secret_pol_info_stream =
            post_initialize::<OncePolStreamProvider, RuntimeServiceId>(&overwatch_handle).await;

        let (
            old_epoch_crypto_processor,
            old_epoch_message_scheduler,
            old_epoch_blending_token_collector,
        ) = run_event_loop(
            inbound_relay,
            &mut blend_message_stream,
            secret_pol_info_stream,
            &mut remaining_epoch_stream,
            &settings_cloned,
            &mut backend,
            &TestNetworkAdapter,
            &sdp_relay,
            message_scheduler.into(),
            &mut rng,
            crypto_processor,
            current_public_info,
            current_recovery_checkpoint,
        )
        .await;

        retire(
            blend_message_stream.map(|(msg, _)| msg),
            remaining_epoch_stream,
            backend,
            TestNetworkAdapter,
            sdp_relay,
            old_epoch_message_scheduler,
            rng,
            old_epoch_blending_token_collector,
            old_epoch_crypto_processor,
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
    let (settings, _recovery_file) = settings(
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
        },
        &public_info_0,
        (),
    );

    let mut generator_1 = new_crypto_processor(
        EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
        },
        &public_info_1,
        (),
    );

    // Build a message with epoch 0 proofs.
    let payload = NetworkMessage {
        message: vec![],
        broadcast_settings: (),
    }
    .to_bytes()
    .expect("NetworkMessage serialization must succeed");
    let msg_0 = generator_0
        .encapsulate_data_payload(&payload)
        .await
        .expect("encapsulation with epoch 0 must succeed");

    // Build a message with epoch 1 proofs.
    let msg_1 = generator_1
        .encapsulate_data_payload(&payload)
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
        EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info_0)),
        None,
        state_updater,
    )
    .unwrap();
    drop(handle_incoming_blend_message(
        (msg_0.clone().into(), epoch_0),
        &mut scheduler_0,
        None,
        &generator_0,
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
        EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info_0)),
        None,
        state_updater,
    )
    .unwrap();
    drop(handle_incoming_blend_message(
        (msg_1.clone().into(), epoch_0),
        &mut scheduler_0_only,
        None,
        &generator_0,
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
        EpochBlendingTokenCollector::new(&reward_epoch_info(&public_info_1)),
        None,
        state_updater,
    )
    .unwrap();
    drop(handle_incoming_blend_message(
        (msg_1.into(), epoch_1),
        &mut scheduler_1,
        None,
        &generator_1,
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
    let (settings, _recovery_file) = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );

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
    let saved_state =
        ServiceState::with_epoch(initial_epoch, token_collector, None, state_updater.clone())
            .unwrap();
    let mut updater = saved_state.start_updating();
    updater.consume_core_quota(5);
    let saved_state = updater.commit_changes();

    let (
        _remaining_epoch_stream,
        _current_public_info,
        _crypto_processor,
        recovered_checkpoint,
        _message_scheduler,
        _backend,
        _rng,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestNetworkAdapter,
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
    )
    .await;

    assert_eq!(
        recovered_checkpoint.spent_quota(),
        5,
        "Matching epoch: spent_quota should be restored from saved state"
    );
    assert_eq!(recovered_checkpoint.last_seen_epoch(), initial_epoch);

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
        stale_token_collector,
        None,
        state_updater2.clone(),
    )
    .unwrap();
    let mut updater = stale_state.start_updating();
    updater.consume_core_quota(42);
    let stale_state = updater.commit_changes();

    let (
        _remaining_epoch_stream2,
        _current_public_info2,
        _crypto_processor2,
        recovered_checkpoint2,
        _message_scheduler2,
        _backend2,
        _rng2,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestNetworkAdapter,
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
    )
    .await;

    assert_eq!(
        recovered_checkpoint2.spent_quota(),
        0,
        "Mismatched epoch: spent_quota should be 0 for fresh state"
    );
    assert_eq!(
        recovered_checkpoint2.last_seen_epoch(),
        initial_epoch,
        "Mismatched epoch: should track the current epoch, not the stale one"
    );
}
