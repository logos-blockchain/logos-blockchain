use crate::behaviour::nat::state_machine::{
    Command, CommandTx, OnEvent, State, event::Event, states::MappedPublic,
};

/// The `MappedPublic` state represents a state where the node's address is
/// known and confirmed to be mapped to a publicly reachable address on the
/// NAT-box. In this state, the address is periodically tested by the `autonat`
/// client to ensure it remains valid. If the address is found to be
/// unreachable, the state machine transitions to the `TestIfMappedPublic` state
/// to re-evaluate the address.
///
/// ### Panics
///
/// This state will panic if it receives an event that does not match the
/// expected address to test.
impl OnEvent for State<MappedPublic> {
    fn on_event(self: Box<Self>, event: Event, command_tx: &CommandTx) -> Box<dyn OnEvent> {
        match event {
            Event::ExternalAddressConfirmed(addr) | Event::AutonatClientTestOk(addr)
                if self.state.external_address() == &addr =>
            {
                command_tx.force_send(Command::ScheduleAutonatClientTest(addr));
                self
            }
            Event::AutonatClientTestFailed(addr) if self.state.external_address() == &addr => {
                self.boxed(MappedPublic::into_test_if_public)
            }
            Event::AddressMappingFailed(addr) if self.state.external_address() == &addr => {
                self.boxed(MappedPublic::into_private)
            }
            Event::DefaultGatewayChanged { local_address, .. } => {
                if let Some(addr) = local_address {
                    command_tx.force_send(Command::MapAddress(addr));
                    self.boxed(MappedPublic::into_try_map_address)
                } else {
                    self.boxed(MappedPublic::into_private)
                }
            }
            Event::ExternalAddressConfirmed(addr) => {
                panic!(
                    "State<MappedPublic>: Swarm confirmed external address {}, but {} was expected",
                    addr,
                    self.state.external_address(),
                );
            }
            Event::AutonatClientTestOk(addr) | Event::AutonatClientTestFailed(addr) => {
                panic!(
                    "State<MappedPublic>: Autonat client reported address {}, but {} was expected",
                    addr,
                    self.state.external_address(),
                );
            }
            _ => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::{error::TryRecvError, unbounded_channel};

    use super::Command;
    use crate::behaviour::nat::state_machine::{
        StateMachine,
        states::{MappedPublic, Private, TestIfPublic, TryMapAddress},
        transitions::fixtures::{
            ADDR, all_events, autonat_failed, autonat_failed_address_mismatch, autonat_ok,
            autonat_ok_address_mismatch, default_gateway_changed,
            default_gateway_changed_no_local_address, external_address_confirmed,
            external_address_confirmed_address_mismatch, mapping_failed,
        },
    };

    #[test]
    fn external_address_confirmed_event_causes_scheduling_new_test() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(MappedPublic::for_test(ADDR.clone()));
        let event = external_address_confirmed();
        state_machine.on_test_event(event);
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &MappedPublic::for_test(ADDR.clone())
        );
        assert_eq!(
            rx.try_recv(),
            Ok(Command::ScheduleAutonatClientTest(ADDR.clone()))
        );
    }

    #[test]
    fn autonat_ok_event_causes_scheduling_new_test() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(MappedPublic::for_test(ADDR.clone()));
        let event = autonat_ok();
        state_machine.on_test_event(event);
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &MappedPublic::for_test(ADDR.clone())
        );
        assert_eq!(
            rx.try_recv(),
            Ok(Command::ScheduleAutonatClientTest(ADDR.clone()))
        );
    }

    #[test]
    fn autonat_client_failed_causes_transition_to_test_if_public() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(MappedPublic::for_test(ADDR.clone()));
        let event = autonat_failed();
        state_machine.on_test_event(event);
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TestIfPublic::for_test(ADDR.clone())
        );
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn address_mapping_failed_causes_transition_to_private() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(MappedPublic::for_test(ADDR.clone()));
        state_machine.on_test_event(mapping_failed());
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &Private::for_test(ADDR.clone())
        );
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[should_panic = "State<MappedPublic>: Swarm confirmed external address /memory/1, but /memory/0 was expected"]
    #[test]
    fn address_mismatch_in_external_address_confirmed_event_causes_panic() {
        let (tx, _) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(MappedPublic::for_test(ADDR.clone()));
        let event = external_address_confirmed_address_mismatch();
        state_machine.on_test_event(event);
    }

    #[should_panic = "State<MappedPublic>: Autonat client reported address /memory/1, but /memory/0 was expected"]
    #[test]
    fn address_mismatch_in_autonat_ok_event_causes_panic() {
        let (tx, _) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(MappedPublic::for_test(ADDR.clone()));
        let event = autonat_ok_address_mismatch();
        state_machine.on_test_event(event);
    }

    #[should_panic = "State<MappedPublic>: Autonat client reported address /memory/1, but /memory/0 was expected"]
    #[test]
    fn address_mismatch_in_autonat_failed_event_causes_panic() {
        let (tx, _) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(MappedPublic::for_test(ADDR.clone()));
        let event = autonat_failed_address_mismatch();
        state_machine.on_test_event(event);
    }

    #[test]
    fn default_gateway_changed_event_causes_transition_to_try_map_address() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(MappedPublic::for_test(ADDR.clone()));
        state_machine.on_test_event(default_gateway_changed());
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TryMapAddress::for_test(ADDR.clone())
        );
        assert_eq!(rx.try_recv(), Ok(Command::MapAddress(ADDR.clone())));
    }

    #[test]
    fn default_gateway_changed_event_without_local_address_causes_transition_to_private() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(MappedPublic::for_test(ADDR.clone()));
        state_machine.on_test_event(default_gateway_changed_no_local_address());
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &Private::for_test(ADDR.clone())
        );
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn other_events_are_ignored() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(MappedPublic::for_test(ADDR.clone()));

        let mut other_events = all_events();
        other_events.remove(&external_address_confirmed());
        other_events.remove(&autonat_ok());
        other_events.remove(&autonat_failed());
        other_events.remove(&mapping_failed());
        other_events.remove(&default_gateway_changed());
        other_events.remove(&default_gateway_changed_no_local_address());

        for event in other_events {
            state_machine.on_test_event(event);
            assert_eq!(
                state_machine.inner.as_ref().unwrap(),
                &MappedPublic::for_test(ADDR.clone())
            );
            assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        }
    }
}
