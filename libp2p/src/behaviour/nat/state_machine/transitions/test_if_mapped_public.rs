use tracing::info;

use crate::behaviour::nat::state_machine::{
    Command, CommandTx, OnEvent, State, event::Event, states::TestIfMappedPublic,
};

/// The `TestIfMappedPublic` state is responsible for testing if the mapped
/// address on the NAT-box is public. If the address is confirmed as public, it
/// transitions to the `MappedPublic` state. If the address is not public, it
/// transitions to the `Private` state.
///
/// This is one of two states that accept a different address than expected. If
/// the swarm confirms a different external address, the state machine stays in
/// `TestIfMappedPublic` with the new address.
impl OnEvent for State<TestIfMappedPublic> {
    fn on_event(self: Box<Self>, event: Event, command_tx: &CommandTx) -> Box<dyn OnEvent> {
        match event {
            Event::ExternalAddressConfirmed(addr) if self.state.addr_to_test() == &addr => {
                command_tx.force_send(Command::ScheduleAutonatClientTest(addr));
                self.boxed(TestIfMappedPublic::into_mapped_public)
            }
            Event::AutonatClientTestFailed(addr) if self.state.addr_to_test() == &addr => {
                self.boxed(TestIfMappedPublic::into_private)
            }
            Event::DefaultGatewayChanged { local_address, .. } => {
                if let Some(addr) = local_address {
                    command_tx.force_send(Command::MapAddress(addr));
                    self.boxed(TestIfMappedPublic::into_try_map_address)
                } else {
                    self.boxed(TestIfMappedPublic::into_private)
                }
            }
            // A different address was confirmed externally. Stay in
            // TestIfMappedPublic and re-target to test the new address.
            Event::ExternalAddressConfirmed(addr) => {
                info!(
                    "State<TestIfMappedPublic>: External address {addr} confirmed (was testing {}), re-targeting.",
                    self.state.addr_to_test(),
                );
                self.boxed(|state| state.into_retarget(addr))
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
        states::{MappedPublic, Private, TestIfMappedPublic, TryMapAddress},
        transitions::fixtures::{
            ADDR, ADDR_1, all_events, autonat_failed, autonat_failed_address_mismatch,
            default_gateway_changed, default_gateway_changed_no_local_address,
            external_address_confirmed, external_address_confirmed_address_mismatch,
        },
    };

    #[test]
    fn external_address_confirmed_event_causes_transition_to_mapped_public() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfMappedPublic::for_test(ADDR.clone(), ADDR.clone()));
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
    fn external_address_confirmed_mismatch_stays_in_test_if_mapped_public_with_new_addr() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfMappedPublic::for_test(ADDR.clone(), ADDR.clone()));
        let event = external_address_confirmed_address_mismatch();
        state_machine.on_test_event(event);
        // local_address stays ADDR, addr_to_test becomes ADDR_1
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TestIfMappedPublic::for_test(ADDR.clone(), ADDR_1.clone())
        );
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn autonat_client_failed_causes_transition_to_private() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfMappedPublic::for_test(ADDR.clone(), ADDR.clone()));
        let event = autonat_failed();
        state_machine.on_test_event(event);
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &Private::for_test(ADDR.clone())
        );
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn autonat_failed_address_mismatch_is_ignored() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfMappedPublic::for_test(ADDR.clone(), ADDR.clone()));
        let event = autonat_failed_address_mismatch();
        state_machine.on_test_event(event);
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TestIfMappedPublic::for_test(ADDR.clone(), ADDR.clone())
        );
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn default_gateway_changed_event_causes_transition_to_try_map_address() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfMappedPublic::for_test(ADDR.clone(), ADDR.clone()));
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
        state_machine.inner = Some(TestIfMappedPublic::for_test(ADDR.clone(), ADDR.clone()));
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
        state_machine.inner = Some(TestIfMappedPublic::for_test(ADDR.clone(), ADDR.clone()));

        let mut other_events = all_events();
        other_events.remove(&external_address_confirmed());
        other_events.remove(&external_address_confirmed_address_mismatch());
        other_events.remove(&autonat_failed());
        other_events.remove(&autonat_failed_address_mismatch());
        other_events.remove(&default_gateway_changed());
        other_events.remove(&default_gateway_changed_no_local_address());

        for event in other_events {
            state_machine.on_test_event(event);
            assert_eq!(
                state_machine.inner.as_ref().unwrap(),
                &TestIfMappedPublic::for_test(ADDR.clone(), ADDR.clone())
            );
            assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        }
    }
}
