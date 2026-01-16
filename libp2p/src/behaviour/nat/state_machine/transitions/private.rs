use crate::behaviour::nat::state_machine::{
    Command, CommandTx, OnEvent, State, event::Event, states::Private,
};

/// The `Private` state represents a state where the node's address is known,
/// but it is not publicly reachable, and it has not been successfully mapped to
/// a publicly reachable address on the NAT-box. In this state, the state
/// machine waits for a change in the local address or the default gateway to
/// re-evaluate the address in the `TestIfPublic` state.
impl OnEvent for State<Private> {
    fn on_event(self: Box<Self>, event: Event, command_tx: &CommandTx) -> Box<dyn OnEvent> {
        match event {
            Event::LocalAddressChanged(addr) if self.state.local_address() != &addr => {
                self.boxed(|state| state.into_test_if_public(addr))
            }
            Event::DefaultGatewayChanged { local_address, .. } => {
                if let Some(addr) = local_address {
                    command_tx.force_send(Command::MapAddress(addr.clone()));
                    self.boxed(|state| state.into_try_map_address(addr))
                } else {
                    self
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
        states::{Private, TestIfPublic, TryMapAddress},
        transitions::fixtures::{
            ADDR, ADDR_1, all_events, default_gateway_changed,
            default_gateway_changed_no_local_address, local_address_changed,
        },
    };

    #[test]
    fn local_address_changed_event_causes_transition_to_test_if_public() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(Private::for_test(ADDR.clone()));
        let event = local_address_changed();
        state_machine.on_test_event(event);
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TestIfPublic::for_test(ADDR_1.clone())
        );
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn default_gateway_changed_event_causes_transition_to_try_map_address() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(Private::for_test(ADDR.clone()));
        state_machine.on_test_event(default_gateway_changed());
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TryMapAddress::for_test(ADDR.clone())
        );
        assert_eq!(rx.try_recv(), Ok(Command::MapAddress(ADDR.clone())));
    }

    #[test]
    fn default_gateway_changed_event_without_local_address_stays_private() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(Private::for_test(ADDR.clone()));
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
        state_machine.inner = Some(Private::for_test(ADDR.clone()));

        let mut other_events = all_events();
        other_events.remove(&local_address_changed());
        other_events.remove(&default_gateway_changed());
        other_events.remove(&default_gateway_changed_no_local_address());

        for event in other_events {
            state_machine.on_test_event(event);
            assert_eq!(
                state_machine.inner.as_ref().unwrap(),
                &Private::for_test(ADDR.clone())
            );
            assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        }
    }
}
