use tracing::debug;

use crate::behaviour::nat::state_machine::{
    Command, CommandTx, OnEvent, State, event::Event, states::TryMapAddress,
};

/// The `TryMapAddress` state is responsible for attempting to map the address
/// to a public-facing address on the NAT-box. If the mapping is successful, it
/// transitions to the `TestIfMappedPublic` state to verify if the mapped
/// address is indeed public. If the mapping fails, it transitions to the
/// `Private` state. If the address mapper resolves a different local address
/// than the one currently tracked, the state machine adopts that mapper result
/// so it can move forward instead of panicking.
impl OnEvent for State<TryMapAddress> {
    fn on_event(self: Box<Self>, event: Event, command_tx: &CommandTx) -> Box<dyn OnEvent> {
        match event {
            Event::NewExternalMappedAddress {
                local_address,
                external_address,
            } => {
                debug!(
                    "State<TryMapAddress>: Mapping succeeded for {local_address} (was tracking {}), transitioning to TestIfMappedPublic with {external_address}.",
                    self.state.addr_to_map(),
                );
                command_tx.force_send(Command::NewExternalAddrCandidate(external_address.clone()));
                self.boxed(|state| {
                    state
                        .retarget(local_address)
                        .into_test_if_mapped_public(external_address)
                })
            }
            Event::AddressMappingFailed(addr) => {
                debug!(
                    "State<TryMapAddress>: Mapping failed for {addr} (was tracking {}), transitioning to Private.",
                    self.state.addr_to_map(),
                );
                self.boxed(|state| state.retarget(addr).into_private())
            }
            Event::DefaultGatewayChanged { local_address, .. } => {
                if let Some(addr) = local_address {
                    command_tx.force_send(Command::MapAddress(addr));
                }
                self
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
            ADDR, ADDR_1, all_events, default_gateway_changed, mapping_failed,
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
            &TestIfMappedPublic::for_test(ADDR.clone(), ADDR.clone())
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

    #[test]
    fn address_mapping_failed_address_mismatch_transitions_to_private_for_failed_address() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TryMapAddress::for_test(ADDR.clone()));
        let event = mapping_failed_address_mismatch();
        state_machine.on_test_event(event);
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &Private::for_test(ADDR_1.clone())
        );
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn mapping_success_address_mismatch_transitions_to_test_if_mapped_public() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TryMapAddress::for_test(ADDR.clone()));
        let event = mapping_ok_address_mismatch();
        state_machine.on_test_event(event);
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TestIfMappedPublic::for_test(ADDR_1.clone(), ADDR.clone())
        );
        assert_eq!(
            rx.try_recv(),
            Ok(Command::NewExternalAddrCandidate(ADDR.clone()))
        );
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
