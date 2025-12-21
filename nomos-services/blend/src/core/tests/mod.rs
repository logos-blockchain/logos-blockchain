mod utils;

use std::num::NonZeroU64;

use groth16::Field as _;
use key_management_system_service::keys::Ed25519Key;
use nomos_blend::{
    message::reward::{ActivityProof, BlendingToken, SessionBlendingTokenCollector},
    proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection},
    scheduling::{
        SessionMessageScheduler, message_blend::crypto::SessionCryptographicProcessorSettings,
        session::SessionEvent,
    },
};
use nomos_core::{codec::SerializeOp as _, crypto::ZkHash, sdp::ActivityMetadata};
use nomos_time::SlotTick;
use nomos_utils::blake_rng::BlakeRng;
use poq::CORE_MERKLE_TREE_HEIGHT;
use rand::SeedableRng as _;

use crate::{
    core::{
        HandleSessionEventOutput,
        backends::BlendBackend,
        handle_incoming_blend_message, handle_session_event, handle_session_transition_expired,
        initialize, post_initialize, retire, run_event_loop,
        state::ServiceState,
        tests::utils::{
            MockKmsAdapter, MockProofsVerifier, NodeId, TestBlendBackend, TestBlendBackendEvent,
            TestNetworkAdapter, dummy_overwatch_resources, new_crypto_processor, new_membership,
            new_public_info, new_stream, reward_session_info, scheduler_session_info,
            scheduler_settings, sdp_relay, settings, timing_settings, wait_for_blend_backend_event,
        },
    },
    epoch_info::EpochHandler,
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
    );
    let public_info = new_public_info(session, membership.clone(), &settings);
    let mut processor = new_crypto_processor(&settings.crypto, &public_info, ());
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
    let scheduler_settings =
        scheduler_settings(&timing_settings(), settings.crypto.num_blend_layers);
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
        msg.clone(),
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
    let mut new_processor = new_crypto_processor(&settings.crypto, &public_info, ());
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
        msg,
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
        msg,
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
        &settings.crypto,
        &new_public_info(session, membership, &settings),
        (),
    );
    let msg = future_processor
        .encapsulate_data_payload(&payload)
        .await
        .expect("encapsulation must succeed");
    let recovery_checkpoint = handle_incoming_blend_message(
        msg,
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
    );
    // Set a long rounds_per_session to make the core quota large enough,
    // since we want the activity threshold to be sufficiently high.
    settings.time.rounds_per_session = 648_000.try_into().unwrap();

    // Create backend.
    let public_info = new_public_info(session, membership.clone(), &settings);
    let mut backend = <TestBlendBackend as BlendBackend<_, _, MockProofsVerifier, _>>::new(
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
    handle_session_transition_expired::<_, NodeId, BlakeRng, MockProofsVerifier, _>(
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
    let nomos_sdp::SdpMessage::PostActivity {
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
#[cfg(test)]
async fn test_handle_session_event() {
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
    );
    let public_info = new_public_info(session, membership.clone(), &settings);
    let crypto_processor = new_crypto_processor(
        &SessionCryptographicProcessorSettings {
            non_ephemeral_signing_key: local_private_key,
            num_blend_layers: NonZeroU64::try_from(1).unwrap(),
        },
        &public_info,
        (),
    );
    let scheduler = SessionMessageScheduler::new(
        scheduler_session_info(&public_info),
        BlakeRng::from_entropy(),
        scheduler_settings(&settings.time, settings.crypto.num_blend_layers),
    );
    let token_collector = SessionBlendingTokenCollector::new(&reward_session_info(&public_info));
    let mut backend = <TestBlendBackend as BlendBackend<_, _, MockProofsVerifier, _>>::new(
        settings.clone(),
        overwatch_handle.clone(),
        public_info.clone(),
        BlakeRng::from_entropy(),
    );
    let mut backend_event_receiver = backend.subscribe_to_events();
    let (sdp_relay, _sdp_relay_receiver) = sdp_relay();

    // Handle a NewSession event, expecting Transitioning output.
    let output = handle_session_event(
        SessionEvent::NewSession(CoreSessionInfo {
            public: CoreSessionPublicInfo {
                membership: membership.clone(),
                session: session + 1,
                poq_core_public_inputs: public_info.session.core_public_inputs,
            },
            core_poq_generator: (),
        }),
        &settings,
        crypto_processor,
        scheduler,
        public_info,
        ServiceState::with_session(session, token_collector, None, state_updater.clone()).unwrap(),
        &mut backend,
        &sdp_relay,
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
        SessionEvent::NewSession(CoreSessionInfo {
            public: CoreSessionPublicInfo {
                membership: new_membership(minimal_network_size - 1).0,
                session: session + 2,
                poq_core_public_inputs: current_public_info.session.core_public_inputs,
            },
            core_poq_generator: (),
        }),
        &settings,
        current_crypto_processor,
        current_scheduler,
        current_public_info,
        new_recovery_checkpoint,
        &mut backend,
        &sdp_relay,
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
        zk: ZkInfo {
            root: ZkHash::ZERO,
            core_and_path_selectors: Some([(ZkHash::ZERO, false); CORE_MERKLE_TREE_HEIGHT]),
        },
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
            current_recovery_checkpoint,
        )
        .await;

        retire(
            blend_message_stream,
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
