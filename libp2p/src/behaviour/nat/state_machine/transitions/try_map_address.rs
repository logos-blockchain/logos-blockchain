use crate::behaviour::nat::state_machine::{
    Command, CommandTx, OnEvent, State, event::Event, states::TryMapAddress,
};

/// The `TryMapAddress` state is responsible for attempting to map the address
/// to a public-facing address on the NAT-box. If the mapping is successful, it
/// transitions to the `TestIfMappedPublic` state to verify if the mapped
/// address is indeed public. If the mapping fails, it transitions to the
/// `Private` state.
///
/// ### Panics
///
/// This state will panic if it receives a mapping event (success or failure)
/// that does not match the expected address to map.
impl OnEvent for State<TryMapAddress> {
    fn on_event(self: Box<Self>, event: Event, command_tx: &CommandTx) -> Box<dyn OnEvent> {
        match event {
            Event::NewExternalMappedAddress {
                local_address,
                external_address,
            } if &local_address == self.state.addr_to_map() => {
                command_tx.force_send(Command::NewExternalAddrCandidate(external_address.clone()));
                self.boxed(|state| state.into_test_if_mapped_public(external_address))
            }
            Event::AddressMappingFailed(addr) if self.state.addr_to_map() == &addr => {
                self.boxed(TryMapAddress::into_private)
            }
            Event::DefaultGatewayChanged { local_address, .. } => {
                if let Some(addr) = local_address {
                    command_tx.force_send(Command::MapAddress(addr));
                }
                self
            }
            Event::AddressMappingFailed(addr) => {
                panic!(
                    "State<TryMapAddress>: Address mapper reported failure for address {}, but {} was expected",
                    addr,
                    self.state.addr_to_map(),
                );
            }
            Event::NewExternalMappedAddress { local_address, .. } => {
                panic!(
                    "State<TryMapAddress>: Address mapper reported success for address {}, but {} was expected",
                    local_address,
                    self.state.addr_to_map(),
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
        states::{Private, TestIfMappedPublic, TryMapAddress},
        transitions::fixtures::{
            ADDR, all_events, default_gateway_changed, mapping_failed,
            mapping_failed_address_mismatch, mapping_ok, mapping_ok_address_mismatch,
        },
    };

    #[test]
    fn new_external_mapped_address_event_causes_transition_to_test_if_mapped_public() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TryMapAddress::for_test(ADDR.clone()));
        let event = mapping_ok();
        state_machine.on_test_event(event);
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TestIfMappedPublic::for_test(ADDR.clone())
        );
        assert_eq!(
            rx.try_recv(),
            Ok(Command::NewExternalAddrCandidate(ADDR.clone()))
        );
    }

    #[test]
    fn address_mapping_failed_causes_transition_to_private() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TryMapAddress::for_test(ADDR.clone()));
        let event = mapping_failed();
        state_machine.on_test_event(event);
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &Private::for_test(ADDR.clone())
        );
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[should_panic = "State<TryMapAddress>: Address mapper reported failure for address /memory/1, but /memory/0 was expected"]
    #[test]
    fn address_mismatch_causes_panic() {
        let (tx, _) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TryMapAddress::for_test(ADDR.clone()));
        let event = mapping_failed_address_mismatch();
        state_machine.on_test_event(event);
    }

    #[should_panic = "State<TryMapAddress>: Address mapper reported success for address /memory/1, but /memory/0 was expected"]
    #[test]
    fn mapping_success_address_mismatch_causes_panic() {
        let (tx, _) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TryMapAddress::for_test(ADDR.clone()));
        let event = mapping_ok_address_mismatch();
        state_machine.on_test_event(event);
    }

    #[should_panic = "State<TryMapAddress>: Address mapper reported success for address /memory/1, but /memory/0 was expected"]
    #[test]
    fn mapping_ok_address_mismatch_causes_panic() {
        let (tx, _) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TryMapAddress::for_test(ADDR.clone()));
        let event = mapping_ok_address_mismatch();
        state_machine.on_test_event(event);
    }

    #[test]
    fn default_gateway_changed_event_stays_in_try_map_address() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TryMapAddress::for_test(ADDR.clone()));
        let event = default_gateway_changed();
        state_machine.on_test_event(event);
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
        state_machine.inner = Some(TryMapAddress::for_test(ADDR.clone()));

        let mut other_events = all_events();
        other_events.remove(&mapping_ok());
        other_events.remove(&mapping_ok_address_mismatch());
        other_events.remove(&mapping_failed());
        other_events.remove(&mapping_failed_address_mismatch());
        other_events.remove(&default_gateway_changed());
        other_events.remove(&mapping_failed_address_mismatch());

        for event in other_events {
            state_machine.on_test_event(event);
            assert_eq!(
                state_machine.inner.as_ref().unwrap(),
                &TryMapAddress::for_test(ADDR.clone())
            );
            assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        }
    }
}
