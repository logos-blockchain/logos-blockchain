use tracing::info;

use crate::behaviour::nat::state_machine::{
    Command, CommandTx, OnEvent, State, event::Event, states::TestIfPublic,
};

/// The `TestIfPublic` state is responsible for testing if the provided address
/// is public. If the address is confirmed as public, it transitions to the
/// `Public` state. If the address is not public, it transitions to the
/// `TryMapAddress` state to attempt mapping the address to some public-facing
/// address on the NAT-box.
///
/// This is one of two states that accept a different address than expected. If
/// the swarm confirms a different external address (e.g. the public IP
/// discovered by identify/Kademlia differs from the local listen address), the
/// state machine stays in `TestIfPublic` with the new address.
impl OnEvent for State<TestIfPublic> {
    fn on_event(self: Box<Self>, event: Event, command_tx: &CommandTx) -> Box<dyn OnEvent> {
        match event {
            Event::ExternalAddressConfirmed(addr) if self.state.addr_to_test() == &addr => {
                command_tx.force_send(Command::ScheduleAutonatClientTest(addr));
                self.boxed(TestIfPublic::into_public)
            }
            Event::AutonatClientTestFailed(addr) if self.state.addr_to_test() == &addr => {
                command_tx.force_send(Command::MapAddress(addr));
                self.boxed(TestIfPublic::into_try_map_address)
            }
            Event::DefaultGatewayChanged { local_address, .. } => {
                if let Some(addr) = local_address {
                    command_tx.force_send(Command::MapAddress(addr));
                    self.boxed(TestIfPublic::into_try_map_address)
                } else {
                    self
                }
            }
            // A different address was confirmed externally. Stay in TestIfPublic
            // and re-target to test the new address before promoting to Public.
            Event::ExternalAddressConfirmed(addr) => {
                info!(
                    "State<TestIfPublic>: External address {addr} confirmed (was testing {}), re-targeting.",
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
        states::{Public, TestIfPublic, TryMapAddress},
        transitions::fixtures::{
            ADDR, ADDR_1, all_events, autonat_failed, autonat_failed_address_mismatch,
            default_gateway_changed, default_gateway_changed_no_local_address,
            external_address_confirmed, external_address_confirmed_address_mismatch,
        },
    };

    #[test]
    fn external_address_confirmed_event_causes_transition_to_public() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfPublic::for_test(ADDR.clone()));
        let event = external_address_confirmed();
        state_machine.on_test_event(event);
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
    fn external_address_confirmed_mismatch_stays_in_test_if_public_with_new_addr() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfPublic::for_test(ADDR.clone()));
        let event = external_address_confirmed_address_mismatch();
        state_machine.on_test_event(event);
        // Stays in TestIfPublic but now tracking ADDR_1
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TestIfPublic::for_test(ADDR_1.clone())
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
