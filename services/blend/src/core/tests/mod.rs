mod utils;

use futures::StreamExt as _;
use lb_blend::{
    message::reward::{ActivityProof, BlendingToken, SessionBlendingTokenCollector},
    proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection},
    scheduling::{
        SessionMessageScheduler, message_blend::crypto::SessionCryptographicProcessorSettings,
        session::SessionEvent,
    },
};
use lb_chain_service::Epoch;
use lb_core::{codec::SerializeOp as _, crypto::ZkHash, sdp::ActivityMetadata};
use lb_groth16::Field as _;
use lb_key_management_system_service::keys::Ed25519Key;
use lb_poq::CORE_MERKLE_TREE_HEIGHT;
use lb_time_service::SlotTick;
use lb_utils::blake_rng::BlakeRng;
use rand::SeedableRng as _;

use crate::{
    core::{
        HandleSessionEventOutput,
        backends::BlendBackend,
        handle_clock_event, handle_incoming_blend_message, handle_new_secret_epoch_info,
        handle_session_event, handle_session_transition_expired, initialize, post_initialize,
        retire, run_event_loop,
        state::ServiceState,
        tests::utils::{
            MockKmsAdapter, MockProofsVerifier, NodeId, TestBlendBackend, TestBlendBackendEvent,
            TestNetworkAdapter, dummy_overwatch_resources, new_crypto_processor, new_membership,
            new_public_info, new_stream, reward_session_info, scheduler_session_info,
            scheduler_settings, sdp_relay, settings, timing_settings, wait_for_blend_backend_event,
        },
    },
    epoch_info::{EpochHandler, PolEpochInfo},
    membership::{MembershipInfo, ZkInfo},
    message::NetworkMessage,
    session::{CoreSessionInfo, CoreSessionPublicInfo},
    test_utils::{
        crypto::MockCoreAndLeaderProofsGenerator,
        epoch::{OncePolStreamProvider, TestChainService},
    },
};

type RuntimeServiceId = ();

/// Check if incoming encapsulated messages are properly decapsulated and
/// scheduled by [`handle_incoming_blend_message`].
#[test_log::test(tokio::test)]
async fn test_handle_incoming_blend_message() {
    let (_, _, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();

    // Prepare a encapsulated message.
    let mut session = 0;
    let minimal_network_size = 1;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let (settings, _recovery_file) = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    let public_info = new_public_info(session, membership.clone(), &settings);
    let mut processor = new_crypto_processor(
        SessionCryptographicProcessorSettings {
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
    let mut scheduler = SessionMessageScheduler::new(
        scheduler_session_info(&public_info),
        BlakeRng::from_entropy(),
        scheduler_settings,
    );
    let recovery_checkpoint = ServiceState::with_session(
        session,
        SessionBlendingTokenCollector::new(&reward_session_info(&public_info)),
        None,
        state_updater,
    )
    .unwrap();
    let recovery_checkpoint = handle_incoming_blend_message(
        (msg.clone().into(), 0),
        &mut scheduler,
        None,
        &processor,
        None,
        recovery_checkpoint,
    );
    assert_eq!(scheduler.release_delayer().unreleased_messages().len(), 1);
    assert_eq!(
        recovery_checkpoint
            .current_session_token_collector()
            .tokens()
            .len(),
        1
    );

    // Creates a new processor/scheduler/token_collector with the new session
    // number.
    session += 1;
    let public_info = new_public_info(session, membership.clone(), &settings);
    let mut new_processor = new_crypto_processor(
        SessionCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
        },
        &public_info,
        (),
    );
    let (mut new_scheduler, mut scheduler) =
        scheduler.rotate_session(scheduler_session_info(&public_info), scheduler_settings);
    let (_, _, _, _, current_token_collector, _, state_updater) =
        recovery_checkpoint.into_components();
    let (new_token_collector, old_token_collector) =
        current_token_collector.rotate_session(&reward_session_info(&public_info));

    // Check that decapsulating the same message fails with the new processor
    // but succeeds with the old one. Also, it should be scheduled in the old
    // scheduler.
    let recovery_checkpoint = ServiceState::with_session(
        session,
        new_token_collector,
        Some(old_token_collector),
        state_updater,
    )
    .unwrap();
    let recovery_checkpoint = handle_incoming_blend_message(
        (msg.clone().into(), 0),
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
            .current_session_token_collector()
            .tokens()
            .len(),
        0
    );
    // No new token should be collected from the same message.
    assert_eq!(
        recovery_checkpoint
            .clone()
            .start_updating()
            .clear_old_session_token_collector()
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
        (msg.into(), 1),
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
            .current_session_token_collector()
            .tokens()
            .len(),
        1
    );
    assert_eq!(
        recovery_checkpoint
            .clone()
            .start_updating()
            .clear_old_session_token_collector()
            .unwrap()
            .tokens()
            .len(),
        1
    );

    // Check that a message built with a future session cannot be
    // decapsulated by either processor, and thus not scheduled.
    session += 1;
    let mut future_processor = new_crypto_processor(
        SessionCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
        },
        &new_public_info(session, membership, &settings),
        (),
    );
    let msg = future_processor
        .encapsulate_data_payload(&payload)
        .await
        .expect("encapsulation must succeed");
    let recovery_checkpoint = handle_incoming_blend_message(
        (msg.into(), 2),
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
            .current_session_token_collector()
            .tokens()
            .len(),
        1
    );
    assert_eq!(
        recovery_checkpoint
            .start_updating()
            .clear_old_session_token_collector()
            .unwrap()
            .tokens()
            .len(),
        1
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

    // Create session 0 processor and build a message with session 0 proofs.
    let session_0 = 0;
    let public_info_0 = new_public_info(session_0, membership.clone(), &settings);
    let mut processor_0 = new_crypto_processor(
        SessionCryptographicProcessorSettings {
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

    // Create session 1 processor - its MockProofsVerifier expects session 1
    // proofs.
    let session_1 = 1;
    let public_info_1 = new_public_info(session_1, membership, &settings);
    let processor_1 = new_crypto_processor(
        SessionCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
        },
        &public_info_1,
        (),
    );

    let scheduler_settings = scheduler_settings(&timing_settings(), settings.num_blend_layers);
    let mut scheduler = SessionMessageScheduler::new(
        scheduler_session_info(&public_info_1),
        BlakeRng::from_entropy(),
        scheduler_settings,
    );
    let recovery_checkpoint = ServiceState::with_session(
        session_1,
        SessionBlendingTokenCollector::new(&reward_session_info(&public_info_1)),
        None,
        state_updater,
    )
    .unwrap();

    // Send session 0 message claiming to be for session 1.
    // Signature is valid (built correctly) but PoQ will fail because the
    // MockProofsVerifier for session 1 expects session 1 proofs.
    drop(handle_incoming_blend_message(
        (msg.into(), session_1),
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
async fn test_handle_session_transition_expired() {
    let (overwatch_handle, _, _, _) = dummy_overwatch_resources::<(), (), RuntimeServiceId>();

    // Prepare settings.
    let session = 0;
    let minimal_network_size = 1;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let (mut settings, _recovery_file) = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    // Set a long rounds_per_session to make the core quota large enough,
    // since we want the activity threshold to be sufficiently high.
    settings.time.rounds_per_session = 648_000.try_into().unwrap();

    // Create backend.
    let public_info = new_public_info(session, membership.clone(), &settings);
    let mut backend = <TestBlendBackend as BlendBackend<_, _, _>>::new(
        settings.clone(),
        overwatch_handle.clone(),
        public_info.clone(),
        BlakeRng::from_entropy(),
    );
    let mut backend_event_receiver = backend.subscribe_to_events();

    // Create token collector and collect a token.
    let mut token_collector =
        SessionBlendingTokenCollector::new(&reward_session_info(&public_info))
            .rotate_session(&reward_session_info(&new_public_info(
                session + 1,
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

    // Call `handle_session_transition_expired`.
    handle_session_transition_expired::<_, NodeId, BlakeRng, _>(
        &mut backend,
        token_collector,
        &sdp_relay,
    )
    .await;

    // Check that the backend handled the transition completion.
    wait_for_blend_backend_event(
        &mut backend_event_receiver,
        TestBlendBackendEvent::SessionTransitionCompleted,
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
    assert_eq!(
        *activity_proof,
        (&ActivityProof::new(session, token)).into()
    );
}

#[test_log::test(tokio::test)]
async fn test_handle_session_event() {
    use lb_chain_service::Epoch;

    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();

    // Prepare components for session event handling.
    let session = 0;
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let (settings, _recovery_file) = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    let public_info = new_public_info(session, membership.clone(), &settings);
    let crypto_processor = new_crypto_processor(
        SessionCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
        },
        &public_info,
        (),
    );
    let scheduler = SessionMessageScheduler::new(
        scheduler_session_info(&public_info),
        BlakeRng::from_entropy(),
        scheduler_settings(&settings.time, settings.num_blend_layers),
    );
    let token_collector = SessionBlendingTokenCollector::new(&reward_session_info(&public_info));
    let mut backend = <TestBlendBackend as BlendBackend<_, _, _>>::new(
        settings.clone(),
        overwatch_handle.clone(),
        public_info.clone(),
        BlakeRng::from_entropy(),
    );
    let mut backend_event_receiver = backend.subscribe_to_events();
    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();

    // Handle a NewSession event, expecting Transitioning output.
    let output = handle_session_event(
        SessionEvent::NewSession(
            CoreSessionInfo {
                public: CoreSessionPublicInfo {
                    membership: membership.clone(),
                    session: session + 1,
                    poq_core_public_inputs: public_info.session.core_public_inputs,
                },
                core_poq_generator: Some(()),
            }
            .into(),
        ),
        &settings,
        crypto_processor,
        scheduler,
        public_info,
        ServiceState::with_session(session, token_collector, None, state_updater.clone()).unwrap(),
        &mut backend,
        &sdp_relay,
        Epoch::new(0),
        None,
    )
    .await;
    let HandleSessionEventOutput::Transitioning {
        new_crypto_processor,
        old_crypto_processor,
        new_scheduler,
        old_scheduler,
        new_public_info,
        new_recovery_checkpoint,
    } = output
    else {
        panic!("expected Transitioning output");
    };
    assert_eq!(
        new_crypto_processor.verifier().session_number(),
        session + 1
    );
    assert_eq!(old_crypto_processor.verifier().session_number(), session);
    assert_eq!(
        new_scheduler.release_delayer().unreleased_messages().len(),
        0
    );
    assert_eq!(
        old_scheduler.release_delayer().unreleased_messages().len(),
        0
    );
    assert_eq!(new_public_info.session.session_number, session + 1);
    assert!(
        new_recovery_checkpoint
            .clone()
            .start_updating()
            .clear_old_session_token_collector()
            .is_some()
    );

    // Handle a TransitionExpired event, expecting TransitionCompleted output.
    let output = handle_session_event(
        SessionEvent::TransitionPeriodExpired,
        &settings,
        new_crypto_processor,
        new_scheduler,
        new_public_info,
        new_recovery_checkpoint,
        &mut backend,
        &sdp_relay,
        Epoch::new(0),
        None,
    )
    .await;
    let HandleSessionEventOutput::TransitionCompleted {
        current_crypto_processor,
        current_scheduler,
        current_public_info,
        new_recovery_checkpoint,
    } = output
    else {
        panic!("expected TransitionCompleted output");
    };
    assert_eq!(
        current_crypto_processor.verifier().session_number(),
        session + 1
    );
    assert_eq!(current_public_info.session.session_number, session + 1);
    assert!(
        new_recovery_checkpoint
            .clone()
            .start_updating()
            .clear_old_session_token_collector()
            .is_none()
    );
    wait_for_blend_backend_event(
        &mut backend_event_receiver,
        TestBlendBackendEvent::SessionTransitionCompleted,
    )
    .await;

    // Handle a NewSession event with a new too small membership,
    // expecting Retiring output.
    let output = handle_session_event(
        SessionEvent::NewSession(
            CoreSessionInfo {
                public: CoreSessionPublicInfo {
                    membership: new_membership(minimal_network_size - 1).0,
                    session: session + 2,
                    poq_core_public_inputs: current_public_info.session.core_public_inputs,
                },
                core_poq_generator: Some(()),
            }
            .into(),
        ),
        &settings,
        current_crypto_processor,
        current_scheduler,
        current_public_info,
        new_recovery_checkpoint,
        &mut backend,
        &sdp_relay,
        Epoch::new(0),
        None,
    )
    .await;
    let HandleSessionEventOutput::Retiring {
        old_crypto_processor,
        old_public_info,
        ..
    } = output
    else {
        panic!("expected Retiring output");
    };
    assert_eq!(
        old_crypto_processor.verifier().session_number(),
        session + 1
    );
    assert_eq!(old_public_info.session.session_number, session + 1);
}

/// Handle a `NewSession(Empty)` event (empty membership), expecting `Retiring`
/// output. This exercises the `MaybeEmptyCoreSessionInfo::Empty` branch of
/// `handle_session_event` directly.
#[test_log::test(tokio::test)]
async fn test_handle_session_event_empty_session_retires() {
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();

    let session = 0;
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let (settings, _recovery_file) = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    let public_info = new_public_info(session, membership.clone(), &settings);
    let crypto_processor = new_crypto_processor(
        SessionCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
        },
        &public_info,
        (),
    );
    let scheduler = SessionMessageScheduler::new(
        scheduler_session_info(&public_info),
        BlakeRng::from_entropy(),
        scheduler_settings(&settings.time, settings.num_blend_layers),
    );
    let token_collector = SessionBlendingTokenCollector::new(&reward_session_info(&public_info));
    let mut backend = <TestBlendBackend as BlendBackend<_, _, _>>::new(
        settings.clone(),
        overwatch_handle.clone(),
        public_info.clone(),
        BlakeRng::from_entropy(),
    );
    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();

    // Handle a NewSession(Empty) event - empty membership triggers Retiring.
    let empty_session: u64 = session + 1;
    let output = handle_session_event(
        SessionEvent::NewSession(empty_session.into()),
        &settings,
        crypto_processor,
        scheduler,
        public_info.clone(),
        ServiceState::with_session(session, token_collector, None, state_updater.clone()).unwrap(),
        &mut backend,
        &sdp_relay,
        Epoch::new(0),
        None,
    )
    .await;
    let HandleSessionEventOutput::Retiring {
        old_crypto_processor,
        old_public_info,
        ..
    } = output
    else {
        panic!("expected Retiring output for Empty session");
    };
    // The old processor/info should be from the session we were on before
    // the empty session arrived.
    assert_eq!(old_crypto_processor.verifier().session_number(), session);
    assert_eq!(old_public_info.session.session_number, session);
}

/// Handle a `NewSession(NonEmpty)` event where membership exists but the local
/// node is not part of it (`core_poq_generator = None`), expecting `Retiring`
/// output.
#[test_log::test(tokio::test)]
async fn test_handle_session_event_non_empty_without_local_core_path_retires() {
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();

    let session = 0;
    let minimal_network_size = 2;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let (settings, _recovery_file) = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    let public_info = new_public_info(session, membership.clone(), &settings);
    let crypto_processor = new_crypto_processor(
        SessionCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
        },
        &public_info,
        (),
    );
    let scheduler = SessionMessageScheduler::new(
        scheduler_session_info(&public_info),
        BlakeRng::from_entropy(),
        scheduler_settings(&settings.time, settings.num_blend_layers),
    );
    let token_collector = SessionBlendingTokenCollector::new(&reward_session_info(&public_info));
    let mut backend = <TestBlendBackend as BlendBackend<_, _, _>>::new(
        settings.clone(),
        overwatch_handle.clone(),
        public_info.clone(),
        BlakeRng::from_entropy(),
    );
    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();

    let output = handle_session_event(
        SessionEvent::NewSession(
            CoreSessionInfo {
                public: CoreSessionPublicInfo {
                    membership,
                    session: session + 1,
                    poq_core_public_inputs: public_info.session.core_public_inputs,
                },
                core_poq_generator: None,
            }
            .into(),
        ),
        &settings,
        crypto_processor,
        scheduler,
        public_info.clone(),
        ServiceState::with_session(session, token_collector, None, state_updater.clone()).unwrap(),
        &mut backend,
        &sdp_relay,
        Epoch::new(0),
        None,
    )
    .await;

    let HandleSessionEventOutput::Retiring {
        old_crypto_processor,
        old_public_info,
        ..
    } = output
    else {
        panic!("expected Retiring output for NonEmpty session without local core path");
    };

    assert_eq!(old_crypto_processor.verifier().session_number(), session);
    assert_eq!(old_public_info.session.session_number, session);
}

/// Check if the service keeps running after it receives a new session where
/// it's still core. Also, check if it stops after the session transition period
/// if it receives another new session that doesn't meet the core node
/// conditions.
#[test_log::test(tokio::test)]
async fn complete_old_session_after_main_loop_done() {
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
    let (clock_stream, clock_sender) = new_stream();

    // Send the initial membership info that the service will expect to receive
    // immediately.
    let initial_session = 0;
    let mut membership_info = MembershipInfo {
        membership: membership.clone(),
        zk: Some(ZkInfo {
            root: ZkHash::ZERO,
            core_and_path_selectors: Some([(ZkHash::ZERO, false); CORE_MERKLE_TREE_HEIGHT]),
        }),
        session_number: initial_session,
    };
    membership_sender
        .send(membership_info.clone())
        .await
        .unwrap();

    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();

    // Send the initial slot tick that the service will expect to receive
    // immediately.
    clock_sender
        .send(SlotTick {
            epoch: 0.into(),
            slot: 0.into(),
        })
        .await
        .unwrap();

    // Prepare an epoch handler with the mock chain service that always returns the
    // same epoch state.
    let mut epoch_handler = EpochHandler::new(
        TestChainService,
        settings.time.epoch_transition_period_in_slots,
    );

    // Prepare dummy Overwatch resources.
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources();

    // Initialize the service.
    let (
        mut remaining_session_stream,
        mut remaining_clock_stream,
        current_public_info,
        _,
        crypto_processor,
        current_recovery_checkpoint,
        message_scheduler,
        mut backend,
        mut rng,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestNetworkAdapter,
        TestChainService,
        MockCoreAndLeaderProofsGenerator,
        MockProofsVerifier,
        MockKmsAdapter,
        RuntimeServiceId,
    >(
        settings.clone(),
        membership_stream,
        clock_stream,
        &mut epoch_handler,
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
            old_session_crypto_processor,
            old_session_message_scheduler,
            old_session_blending_token_collector,
            old_session_public_info,
            _,
        ) = run_event_loop(
            inbound_relay,
            &mut blend_message_stream,
            &mut remaining_clock_stream,
            secret_pol_info_stream,
            &mut remaining_session_stream,
            &settings_cloned,
            &mut backend,
            &TestNetworkAdapter,
            &sdp_relay,
            &mut epoch_handler,
            message_scheduler.into(),
            &mut rng,
            crypto_processor,
            current_public_info,
            Epoch::new(0),
            current_recovery_checkpoint,
        )
        .await;

        retire(
            blend_message_stream.map(|(msg, _)| msg),
            remaining_clock_stream,
            remaining_session_stream,
            &settings_cloned,
            backend,
            TestNetworkAdapter,
            sdp_relay,
            epoch_handler,
            old_session_message_scheduler,
            rng,
            old_session_blending_token_collector,
            old_session_crypto_processor,
            old_session_public_info,
            Epoch::new(0),
        )
        .await;
    });

    // Send a new session with the same membership.
    membership_info.session_number += 1;
    membership_sender
        .send(membership_info.clone())
        .await
        .unwrap();

    // Since the node is still core in the new session,
    // the service must keep running even after a session transition period.
    wait_for_blend_backend_event(
        &mut backend_event_receiver,
        TestBlendBackendEvent::SessionTransitionCompleted,
    )
    .await;
    assert!(!join_handle.is_finished());

    // Send a new session with a new membership smaller than minimal size
    membership_info.membership = new_membership(minimal_network_size.checked_sub(1).unwrap()).0;
    membership_info.session_number += 1;
    membership_sender.send(membership_info).await.unwrap();

    // Since the network is smaller than the minimal size,
    // the service must stop after a session transition period.
    wait_for_blend_backend_event(
        &mut backend_event_receiver,
        TestBlendBackendEvent::SessionTransitionCompleted,
    )
    .await;
    join_handle
        .await
        .expect("the service should stop without error");
}

/// Check that the service handles a new session with empty providers (zk: None)
/// without panicking. It should retire gracefully.
#[test_log::test(tokio::test)]
async fn stop_on_empty_session() {
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
    let (clock_stream, clock_sender) = new_stream();

    // Send the initial membership info that the service will expect to receive
    // immediately.
    let initial_session = 0;
    let membership_info = MembershipInfo {
        membership: membership.clone(),
        zk: Some(ZkInfo {
            root: ZkHash::ZERO,
            core_and_path_selectors: Some([(ZkHash::ZERO, false); CORE_MERKLE_TREE_HEIGHT]),
        }),
        session_number: initial_session,
    };
    membership_sender
        .send(membership_info.clone())
        .await
        .unwrap();

    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();

    // Send the initial slot tick that the service will expect to receive
    // immediately.
    clock_sender
        .send(SlotTick {
            epoch: 0.into(),
            slot: 0.into(),
        })
        .await
        .unwrap();

    // Prepare an epoch handler with the mock chain service that always returns the
    // same epoch state.
    let mut epoch_handler = EpochHandler::new(
        TestChainService,
        settings.time.epoch_transition_period_in_slots,
    );

    // Prepare dummy Overwatch resources.
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources();

    // Initialize the service.
    let (
        mut remaining_session_stream,
        mut remaining_clock_stream,
        current_public_info,
        _,
        crypto_processor,
        current_recovery_checkpoint,
        message_scheduler,
        mut backend,
        mut rng,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestNetworkAdapter,
        TestChainService,
        MockCoreAndLeaderProofsGenerator,
        MockProofsVerifier,
        MockKmsAdapter,
        RuntimeServiceId,
    >(
        settings.clone(),
        membership_stream,
        clock_stream,
        &mut epoch_handler,
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
            old_session_crypto_processor,
            old_session_message_scheduler,
            old_session_blending_token_collector,
            old_session_public_info,
            _,
        ) = run_event_loop(
            inbound_relay,
            &mut blend_message_stream,
            &mut remaining_clock_stream,
            secret_pol_info_stream,
            &mut remaining_session_stream,
            &settings_cloned,
            &mut backend,
            &TestNetworkAdapter,
            &sdp_relay,
            &mut epoch_handler,
            message_scheduler.into(),
            &mut rng,
            crypto_processor,
            current_public_info,
            Epoch::new(0),
            current_recovery_checkpoint,
        )
        .await;

        retire(
            blend_message_stream.map(|(msg, _)| msg),
            remaining_clock_stream,
            remaining_session_stream,
            &settings_cloned,
            backend,
            TestNetworkAdapter,
            sdp_relay,
            epoch_handler,
            old_session_message_scheduler,
            rng,
            old_session_blending_token_collector,
            old_session_crypto_processor,
            old_session_public_info,
            Epoch::new(0),
        )
        .await;
    });

    // Send a new session with empty providers (zk: None).
    // This simulates a session where no providers are available.
    membership_sender
        .send(MembershipInfo {
            membership: membership.clone(),
            zk: None,
            session_number: initial_session + 1,
        })
        .await
        .unwrap();

    wait_for_blend_backend_event(
        &mut backend_event_receiver,
        TestBlendBackendEvent::SessionTransitionCompleted,
    )
    .await;
    // The service should stop without panicking.
    join_handle
        .await
        .expect("the service should stop without panic on empty session");
}

/// Check that the service handles a non-empty new session where the local node
/// has no core path (`core_poq_generator = None`) without panicking. It should
/// retire gracefully.
#[test_log::test(tokio::test)]
async fn stop_on_non_empty_session_without_local_core_path() {
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
    let (clock_stream, clock_sender) = new_stream();

    // Send the initial membership info that the service will expect to receive
    // immediately.
    let initial_session = 0;
    let membership_info = MembershipInfo {
        membership: membership.clone(),
        zk: Some(ZkInfo {
            root: ZkHash::ZERO,
            core_and_path_selectors: Some([(ZkHash::ZERO, false); CORE_MERKLE_TREE_HEIGHT]),
        }),
        session_number: initial_session,
    };
    membership_sender
        .send(membership_info.clone())
        .await
        .unwrap();

    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();

    // Send the initial slot tick that the service will expect to receive
    // immediately.
    clock_sender
        .send(SlotTick {
            epoch: 0.into(),
            slot: 0.into(),
        })
        .await
        .unwrap();

    // Prepare an epoch handler with the mock chain service that always returns the
    // same epoch state.
    let mut epoch_handler = EpochHandler::new(
        TestChainService,
        settings.time.epoch_transition_period_in_slots,
    );

    // Prepare dummy Overwatch resources.
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources();

    // Initialize the service.
    let (
        mut remaining_session_stream,
        mut remaining_clock_stream,
        current_public_info,
        _,
        crypto_processor,
        current_recovery_checkpoint,
        message_scheduler,
        mut backend,
        mut rng,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestNetworkAdapter,
        TestChainService,
        MockCoreAndLeaderProofsGenerator,
        MockProofsVerifier,
        MockKmsAdapter,
        RuntimeServiceId,
    >(
        settings.clone(),
        membership_stream,
        clock_stream,
        &mut epoch_handler,
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
            old_session_crypto_processor,
            old_session_message_scheduler,
            old_session_blending_token_collector,
            old_session_public_info,
            _,
        ) = run_event_loop(
            inbound_relay,
            &mut blend_message_stream,
            &mut remaining_clock_stream,
            secret_pol_info_stream,
            &mut remaining_session_stream,
            &settings_cloned,
            &mut backend,
            &TestNetworkAdapter,
            &sdp_relay,
            &mut epoch_handler,
            message_scheduler.into(),
            &mut rng,
            crypto_processor,
            current_public_info,
            Epoch::new(0),
            current_recovery_checkpoint,
        )
        .await;

        retire(
            blend_message_stream.map(|(msg, _)| msg),
            remaining_clock_stream,
            remaining_session_stream,
            &settings_cloned,
            backend,
            TestNetworkAdapter,
            sdp_relay,
            epoch_handler,
            old_session_message_scheduler,
            rng,
            old_session_blending_token_collector,
            old_session_crypto_processor,
            old_session_public_info,
            Epoch::new(0),
        )
        .await;
    });

    // Send a new non-empty session without local core path.
    membership_sender
        .send(MembershipInfo {
            membership,
            zk: Some(ZkInfo {
                root: ZkHash::ZERO,
                core_and_path_selectors: None,
            }),
            session_number: initial_session + 1,
        })
        .await
        .unwrap();

    wait_for_blend_backend_event(
        &mut backend_event_receiver,
        TestBlendBackendEvent::SessionTransitionCompleted,
    )
    .await;
    // The service should stop without panicking.
    join_handle
        .await
        .expect("the service should stop without panic when local core path is missing");
}

/// Verify that the proof generator produces proofs for the correct session,
/// and that those proofs are only accepted by a verifier for the same session.
#[test_log::test(tokio::test)]
async fn test_proof_generator_session_binding() {
    let session_0 = 0u64;
    let session_1 = 1u64;
    let minimal_network_size = 1;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let (settings, _recovery_file) = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );

    // Create proof generators for session 0 and session 1.
    let public_info_0 = new_public_info(session_0, membership.clone(), &settings);
    let public_info_1 = new_public_info(session_1, membership.clone(), &settings);

    let mut generator_0 = new_crypto_processor(
        SessionCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
        },
        &public_info_0,
        (),
    );

    let mut generator_1 = new_crypto_processor(
        SessionCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
        },
        &public_info_1,
        (),
    );

    // Build a message with session 0 proofs.
    let payload = NetworkMessage {
        message: vec![],
        broadcast_settings: (),
    }
    .to_bytes()
    .expect("NetworkMessage serialization must succeed");
    let msg_0 = generator_0
        .encapsulate_data_payload(&payload)
        .await
        .expect("encapsulation with session 0 must succeed");

    // Build a message with session 1 proofs.
    let msg_1 = generator_1
        .encapsulate_data_payload(&payload)
        .await
        .expect("encapsulation with session 1 must succeed");

    // Session 0 message should be decapsulable by session 0 processor.
    let (_, _, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();
    let scheduler_settings = scheduler_settings(&timing_settings(), settings.num_blend_layers);
    let mut scheduler_0 = SessionMessageScheduler::new(
        scheduler_session_info(&public_info_0),
        BlakeRng::from_entropy(),
        scheduler_settings,
    );
    let recovery_checkpoint = ServiceState::with_session(
        session_0,
        SessionBlendingTokenCollector::new(&reward_session_info(&public_info_0)),
        None,
        state_updater,
    )
    .unwrap();
    drop(handle_incoming_blend_message(
        (msg_0.clone().into(), session_0),
        &mut scheduler_0,
        None,
        &generator_0,
        None,
        recovery_checkpoint,
    ));
    assert_eq!(
        scheduler_0.release_delayer().unreleased_messages().len(),
        1,
        "Session 0 message must be scheduled by session 0 processor"
    );

    // Session 1 message should NOT be decapsulable by session 0 processor
    // (wrong PoQ proofs for session 0).
    let (_, _, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();
    let mut scheduler_0_only = SessionMessageScheduler::new(
        scheduler_session_info(&public_info_0),
        BlakeRng::from_entropy(),
        scheduler_settings,
    );
    let recovery_checkpoint = ServiceState::with_session(
        session_0,
        SessionBlendingTokenCollector::new(&reward_session_info(&public_info_0)),
        None,
        state_updater,
    )
    .unwrap();
    drop(handle_incoming_blend_message(
        (msg_1.clone().into(), session_0),
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
        "Session 1 message must NOT be scheduled by session 0 processor"
    );

    // Session 1 message should be decapsulable by session 1 processor.
    let (_, _, state_updater, _state_receiver) =
        dummy_overwatch_resources::<(), (), RuntimeServiceId>();
    let mut scheduler_1 = SessionMessageScheduler::new(
        scheduler_session_info(&public_info_1),
        BlakeRng::from_entropy(),
        scheduler_settings,
    );
    let recovery_checkpoint = ServiceState::with_session(
        session_1,
        SessionBlendingTokenCollector::new(&reward_session_info(&public_info_1)),
        None,
        state_updater,
    )
    .unwrap();
    drop(handle_incoming_blend_message(
        (msg_1.into(), session_1),
        &mut scheduler_1,
        None,
        &generator_1,
        None,
        recovery_checkpoint,
    ));
    assert_eq!(
        scheduler_1.release_delayer().unreleased_messages().len(),
        1,
        "Session 1 message must be scheduled by session 1 processor"
    );
}

/// Verify that `handle_clock_event` correctly updates the public info and
/// epoch number when the `EpochHandler` emits a `NewEpoch` event.
#[test_log::test(tokio::test)]
async fn test_handle_clock_event_new_epoch() {
    let minimal_network_size = 1;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let (settings, _recovery_file) = settings(
        local_private_key.clone(),
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    let session = 0;
    let public_info = new_public_info(session, membership.clone(), &settings);
    let mut processor = new_crypto_processor(
        SessionCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
        },
        &public_info,
        (),
    );

    let initial_epoch = Epoch::new(0);

    // Create an EpochHandler with a transition period of 1 slot.
    let mut epoch_handler: EpochHandler<_, RuntimeServiceId> =
        EpochHandler::new(TestChainService, 1.try_into().unwrap());

    // First tick initializes the epoch handler.
    let (updated_info, updated_epoch) = handle_clock_event(
        SlotTick {
            epoch: 1.into(),
            slot: 1.into(),
        },
        &settings,
        &mut epoch_handler,
        &mut processor,
        public_info.clone(),
        initial_epoch,
    )
    .await;
    assert_eq!(
        updated_epoch,
        Epoch::new(1),
        "Epoch must advance to 1 after first tick in epoch 1"
    );
    // Public info should be updated with new leader inputs derived from chain
    // state.
    assert_ne!(
        updated_info.epoch, public_info.epoch,
        "Leader inputs should be updated from chain epoch state"
    );

    // Tick in the same epoch should not change epoch.
    let (unchanged_info, unchanged_epoch) = handle_clock_event(
        SlotTick {
            epoch: 1.into(),
            slot: 2.into(),
        },
        &settings,
        &mut epoch_handler,
        &mut processor,
        updated_info.clone(),
        updated_epoch,
    )
    .await;
    assert_eq!(unchanged_epoch, Epoch::new(1));
    assert_eq!(unchanged_info.epoch, updated_info.epoch);

    // Tick in a new epoch should advance again.
    let (final_info, final_epoch) = handle_clock_event(
        SlotTick {
            epoch: 2.into(),
            slot: 3.into(),
        },
        &settings,
        &mut epoch_handler,
        &mut processor,
        unchanged_info.clone(),
        unchanged_epoch,
    )
    .await;
    assert_eq!(
        final_epoch,
        Epoch::new(2),
        "Epoch must advance to 2 after tick in epoch 2"
    );
    // Since epoch_transition_period is 1 slot and slot 2 was the last in epoch 1,
    // this triggers NewEpochAndOldEpochTransitionExpired which both completes the
    // old transition and rotates to epoch 2.
    assert_eq!(final_info.session.session_number, session);
}

/// Verify that `handle_new_secret_epoch_info` returns updated leader inputs
/// when the `PoL` info epoch is newer than the current epoch, and returns
/// `None` when the epoch has already been processed.
#[test_log::test(tokio::test)]
async fn test_handle_new_secret_epoch_info() {
    let minimal_network_size = 1;
    let (membership, local_private_key) = new_membership(minimal_network_size);
    let (settings, _recovery_file) = settings(
        local_private_key,
        u64::from(minimal_network_size).try_into().unwrap(),
        (),
        0,
    );
    let session = 0;
    let public_info = new_public_info(session, membership, &settings);
    let mut processor = new_crypto_processor(
        SessionCryptographicProcessorSettings {
            non_ephemeral_encryption_key: settings.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: settings.num_blend_layers,
        },
        &public_info,
        (),
    );

    let current_epoch = Epoch::new(0);

    // PoL info for a new epoch (epoch 1 > current 0): should return Some.
    let pol_info = PolEpochInfo {
        epoch: Epoch::new(1),
        poq_public_inputs: lb_core::proofs::leader_proof::LeaderPublic {
            slot: 1,
            latest_root: lb_groth16::Fr::ZERO,
            lottery_0: lb_groth16::Fr::ONE,
            lottery_1: lb_groth16::Fr::ONE,
            epoch_nonce: ZkHash::ONE,
            aged_root: ZkHash::ONE,
        },
        poq_private_inputs:
            lb_blend::proofs::quota::inputs::prove::private::ProofOfLeadershipQuotaInputs {
                slot: 1,
                note_value: 1,
                transaction_hash: ZkHash::ZERO,
                output_number: 1,
                aged_path_and_selectors: [(ZkHash::ZERO, false); _],
                secret_key: ZkHash::ZERO,
            },
    };
    let result = handle_new_secret_epoch_info(&settings, &pol_info, &mut processor, current_epoch);
    assert!(
        result.is_some(),
        "Should return Some(LeaderInputs) when PoL epoch > current epoch"
    );
    let new_leader = result.unwrap();
    assert_eq!(new_leader.pol_epoch_nonce, ZkHash::ONE);
    assert_eq!(new_leader.pol_ledger_aged, ZkHash::ONE);

    // PoL info for the same epoch (epoch 1 == current 1): should return None.
    let already_processed_epoch = Epoch::new(1);
    let result = handle_new_secret_epoch_info(
        &settings,
        &pol_info,
        &mut processor,
        already_processed_epoch,
    );
    assert!(
        result.is_none(),
        "Should return None when PoL epoch <= current epoch"
    );

    // PoL info for an older epoch: should return None.
    let future_epoch = Epoch::new(5);
    let result = handle_new_secret_epoch_info(&settings, &pol_info, &mut processor, future_epoch);
    assert!(
        result.is_none(),
        "Should return None when PoL epoch < current epoch"
    );
}

/// When `initialize` receives a `last_saved_state` whose session matches the
/// current membership session, the saved state is restored (e.g. `spent_quota`
/// is preserved). When the session does not match, a fresh state is created.
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

    let initial_session = 0;

    // Matching session: saved state should be restored

    let (membership_stream, membership_sender) = new_stream();
    let (clock_stream, clock_sender) = new_stream();
    membership_sender
        .send(MembershipInfo {
            membership: membership.clone(),
            zk: Some(ZkInfo {
                root: ZkHash::ZERO,
                core_and_path_selectors: Some([(ZkHash::ZERO, false); CORE_MERKLE_TREE_HEIGHT]),
            }),
            session_number: initial_session,
        })
        .await
        .unwrap();
    clock_sender
        .send(SlotTick {
            epoch: 0.into(),
            slot: 0.into(),
        })
        .await
        .unwrap();

    let mut epoch_handler = EpochHandler::new(
        TestChainService,
        settings.time.epoch_transition_period_in_slots,
    );
    let (overwatch_handle, _overwatch_cmd_receiver, state_updater, _state_receiver) =
        dummy_overwatch_resources();
    let (sdp_relay_1, _sdp_relay_receiver) = sdp_relay();

    // Build a pre-populated saved state with matching session and some spent quota.
    let public_info = new_public_info(initial_session, membership.clone(), &settings);
    let token_collector = SessionBlendingTokenCollector::new(&reward_session_info(&public_info));
    let saved_state = ServiceState::with_session(
        initial_session,
        token_collector,
        None,
        state_updater.clone(),
    )
    .unwrap();
    let mut updater = saved_state.start_updating();
    updater.consume_core_quota(5);
    let saved_state = updater.commit_changes();

    let (
        _remaining_session_stream,
        _remaining_clock_stream,
        _current_public_info,
        _current_epoch,
        _crypto_processor,
        recovered_checkpoint,
        _message_scheduler,
        _backend,
        _rng,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestNetworkAdapter,
        TestChainService,
        MockCoreAndLeaderProofsGenerator,
        MockProofsVerifier,
        MockKmsAdapter,
        RuntimeServiceId,
    >(
        settings.clone(),
        membership_stream,
        clock_stream,
        &mut epoch_handler,
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
        "Matching session: spent_quota should be restored from saved state"
    );
    assert_eq!(recovered_checkpoint.last_seen_session(), initial_session);

    // Mismatched session: fresh state should be created

    let (membership_stream2, membership_sender2) = new_stream();
    let (clock_stream2, clock_sender2) = new_stream();
    membership_sender2
        .send(MembershipInfo {
            membership: membership.clone(),
            zk: Some(ZkInfo {
                root: ZkHash::ZERO,
                core_and_path_selectors: Some([(ZkHash::ZERO, false); CORE_MERKLE_TREE_HEIGHT]),
            }),
            session_number: initial_session,
        })
        .await
        .unwrap();
    clock_sender2
        .send(SlotTick {
            epoch: 0.into(),
            slot: 1.into(),
        })
        .await
        .unwrap();

    let mut epoch_handler2 = EpochHandler::new(
        TestChainService,
        settings.time.epoch_transition_period_in_slots,
    );
    let (overwatch_handle2, _overwatch_cmd_receiver2, state_updater2, _state_receiver2) =
        dummy_overwatch_resources();
    let (sdp_relay2, _sdp_relay_receiver2) = sdp_relay();

    // Build a saved state for a *different* session (session 99) with spent quota.
    let stale_public_info = new_public_info(99, membership.clone(), &settings);
    let stale_token_collector =
        SessionBlendingTokenCollector::new(&reward_session_info(&stale_public_info));
    let stale_state =
        ServiceState::with_session(99, stale_token_collector, None, state_updater2.clone())
            .unwrap();
    let mut updater = stale_state.start_updating();
    updater.consume_core_quota(42);
    let stale_state = updater.commit_changes();

    let (
        _remaining_session_stream2,
        _remaining_clock_stream2,
        _current_public_info2,
        _current_epoch2,
        _crypto_processor2,
        recovered_checkpoint2,
        _message_scheduler2,
        _backend2,
        _rng2,
    ) = initialize::<
        NodeId,
        TestBlendBackend,
        TestNetworkAdapter,
        TestChainService,
        MockCoreAndLeaderProofsGenerator,
        MockProofsVerifier,
        MockKmsAdapter,
        RuntimeServiceId,
    >(
        settings.clone(),
        membership_stream2,
        clock_stream2,
        &mut epoch_handler2,
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
        "Mismatched session: spent_quota should be 0 for fresh state"
    );
    assert_eq!(
        recovered_checkpoint2.last_seen_session(),
        initial_session,
        "Mismatched session: should track the current session, not the stale one"
    );
}
