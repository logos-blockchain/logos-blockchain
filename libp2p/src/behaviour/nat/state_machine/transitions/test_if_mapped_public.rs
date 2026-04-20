use tracing::debug;

use crate::behaviour::nat::state_machine::{
    Command, CommandTx, OnEvent, State, event::Event, states::TestIfMappedPublic,
};

/// The `TestIfMappedPublic` state is responsible for testing if the mapped
/// address on the NAT-box is public. If the address is confirmed as public, it
/// transitions to the `MappedPublic` state. If the address is not public, it
/// transitions to the `Private` state.
///
/// Any `ExternalAddressConfirmed` event — regardless of whether it matches the
/// current `addr_to_test` — causes a direct transition to `MappedPublic`. The
/// swarm confirms an external address only once, so there is no second
/// confirmation to wait for. The `MappedPublic` state will verify reachability
/// via periodic autonat probes and demote back if the address turns out to be
/// unreachable.
impl OnEvent for State<TestIfMappedPublic> {
    fn on_event(self: Box<Self>, event: Event, command_tx: &CommandTx) -> Box<dyn OnEvent> {
        match event {
            Event::ExternalAddressConfirmed(addr) => {
                debug!(
                    "State<TestIfMappedPublic>: External address {addr} confirmed (was testing {}), promoting to MappedPublic.",
                    self.state.addr_to_test(),
                );
                command_tx.force_send(Command::ScheduleAutonatClientTest(addr.clone()));
                self.boxed(|state| state.retarget(addr).into_mapped_public())
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
    fn external_address_confirmed_transitions_to_mapped_public() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfMappedPublic::for_test(ADDR.clone(), ADDR.clone()));
        state_machine.on_test_event(external_address_confirmed());
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
    fn external_address_confirmed_mismatch_transitions_to_mapped_public_with_new_addr() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfMappedPublic::for_test(ADDR.clone(), ADDR.clone()));
        state_machine.on_test_event(external_address_confirmed_address_mismatch());
        // Transitions to MappedPublic with local_address=ADDR, external_address=ADDR_1
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &MappedPublic::for_test_with_addrs(ADDR.clone(), ADDR_1.clone())
        );
        assert_eq!(
            rx.try_recv(),
            Ok(Command::ScheduleAutonatClientTest(ADDR_1.clone()))
        );
    }

    #[test]
    fn autonat_client_failed_causes_transition_to_private() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfMappedPublic::for_test(ADDR.clone(), ADDR.clone()));
        state_machine.on_test_event(autonat_failed());
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
        state_machine.on_test_event(autonat_failed_address_mismatch());
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
