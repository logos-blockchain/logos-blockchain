#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::{error::TryRecvError, unbounded_channel};

    use crate::behaviour::nat::state_machine::{
        Command, StateMachine,
        states::{Public, TestIfPublic, TryMapAddress},
        transitions::fixtures::{
            ADDR, ADDR_1, all_events, autonat_failed, autonat_failed_address_mismatch, autonat_ok,
            autonat_ok_address_mismatch, default_gateway_changed,
            default_gateway_changed_no_local_address, external_address_confirmed,
            external_address_confirmed_address_mismatch,
        },
    };

    #[test]
    fn external_address_confirmed_retargets_and_schedules_probe() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfPublic::for_test(ADDR.clone()));
        state_machine.on_test_event(external_address_confirmed());
        // Stays in TestIfPublic, re-targeted to ADDR
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TestIfPublic::for_test(ADDR.clone())
        );
        assert_eq!(
            rx.try_recv(),
            Ok(Command::ScheduleAutonatClientTest(ADDR.clone()))
        );
    }

    #[test]
    fn external_address_confirmed_mismatch_retargets_and_schedules_probe() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfPublic::for_test(ADDR.clone()));
        state_machine.on_test_event(external_address_confirmed_address_mismatch());
        // Stays in TestIfPublic, re-targeted to ADDR_1
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TestIfPublic::for_test(ADDR_1.clone())
        );
        assert_eq!(
            rx.try_recv(),
            Ok(Command::ScheduleAutonatClientTest(ADDR_1.clone()))
        );
    }

    #[test]
    fn autonat_ok_after_retarget_transitions_to_public() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfPublic::for_test(ADDR.clone()));

        // Step 1: ExternalAddressConfirmed retargets to ADDR_1 and schedules probe
        state_machine.on_test_event(external_address_confirmed_address_mismatch());
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TestIfPublic::for_test(ADDR_1.clone())
        );
        assert_eq!(
            rx.try_recv(),
            Ok(Command::ScheduleAutonatClientTest(ADDR_1.clone()))
        );

        // Step 2: Autonat probe succeeds — transitions to Public
        state_machine.on_test_event(autonat_ok_address_mismatch());
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &Public::for_test(ADDR_1.clone())
        );
        assert_eq!(
            rx.try_recv(),
            Ok(Command::ScheduleAutonatClientTest(ADDR_1.clone()))
        );
    }

    #[test]
    fn autonat_failed_after_retarget_transitions_to_try_map_address() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfPublic::for_test(ADDR.clone()));

        // Step 1: ExternalAddressConfirmed retargets to ADDR_1
        state_machine.on_test_event(external_address_confirmed_address_mismatch());
        let _ = rx.try_recv(); // consume ScheduleAutonatClientTest

        // Step 2: Autonat probe fails — transitions to TryMapAddress
        state_machine.on_test_event(autonat_failed_address_mismatch());
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TryMapAddress::for_test(ADDR_1.clone())
        );
        assert_eq!(rx.try_recv(), Ok(Command::MapAddress(ADDR_1.clone())));
    }

    #[test]
    fn autonat_ok_causes_transition_to_public() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfPublic::for_test(ADDR.clone()));
        state_machine.on_test_event(autonat_ok());
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &Public::for_test(ADDR.clone())
        );
        assert_eq!(
            rx.try_recv(),
            Ok(Command::ScheduleAutonatClientTest(ADDR.clone()))
        );
    }

    #[test]
    fn autonat_ok_address_mismatch_is_ignored() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfPublic::for_test(ADDR.clone()));
        state_machine.on_test_event(autonat_ok_address_mismatch());
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TestIfPublic::for_test(ADDR.clone())
        );
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn autonat_client_failed_causes_transition_to_try_map_address() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfPublic::for_test(ADDR.clone()));
        let event = autonat_failed();
        state_machine.on_test_event(event);
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TryMapAddress::for_test(ADDR.clone())
        );
        assert_eq!(rx.try_recv(), Ok(Command::MapAddress(ADDR.clone())));
    }

    #[test]
    fn autonat_failed_address_mismatch_is_ignored() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfPublic::for_test(ADDR.clone()));
        let event = autonat_failed_address_mismatch();
        state_machine.on_test_event(event);
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TestIfPublic::for_test(ADDR.clone())
        );
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn default_gateway_changed_event_causes_transition_to_try_map_address() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfPublic::for_test(ADDR.clone()));
        state_machine.on_test_event(default_gateway_changed());
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TryMapAddress::for_test(ADDR.clone())
        );
        assert_eq!(rx.try_recv(), Ok(Command::MapAddress(ADDR.clone())));
    }

    #[test]
    fn other_events_are_ignored() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfPublic::for_test(ADDR.clone()));

        let mut other_events = all_events();
        other_events.remove(&external_address_confirmed());
        other_events.remove(&external_address_confirmed_address_mismatch());
        other_events.remove(&autonat_ok());
        other_events.remove(&autonat_ok_address_mismatch());
        other_events.remove(&autonat_failed());
        other_events.remove(&autonat_failed_address_mismatch());
        other_events.remove(&default_gateway_changed());
        other_events.remove(&default_gateway_changed_no_local_address());

        for event in other_events {
            state_machine.on_test_event(event);
            assert_eq!(
                state_machine.inner.as_ref().unwrap(),
                &TestIfPublic::for_test(ADDR.clone())
            );
            assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        }
    }
}
