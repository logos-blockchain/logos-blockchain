use lb_log_targets::libp2p as lb_log_targets_libp2p;

use crate::behaviour::nat::state_machine::{
    Command, CommandTx, OnEvent, State, event::Event, states::TestIfPublic,
};

const LOG_TARGET: &str = lb_log_targets_libp2p::behaviour::nat::state_machine::TEST_IF_PUBLIC;

/// The `TestIfPublic` state is responsible for testing if the node is publicly
/// reachable. If `AutoNAT` confirms an address, it transitions to the `Public`
/// state. If not, it transitions to the `TryMapAddress` state to attempt
/// mapping the local address to some public-facing address on the NAT-box.
///
/// `AutoNAT` tests the addresses at which other peers observe the node, which
/// behind a NAT are the NAT's and not the local address, and no address is
/// confirmed in this state. So any `ExternalAddressConfirmed` event causes a
/// direct transition to `Public`, and any `AutonatClientTestFailed` event means
/// that the node is not reachable as it is. The swarm confirms an external
/// address only once, so there is no second confirmation to wait for. The
/// `Public` state will verify reachability via periodic autonat probes and
/// demote back if the address turns out to be unreachable.
impl OnEvent for State<TestIfPublic> {
    fn on_event(self: Box<Self>, event: Event, command_tx: &CommandTx) -> Box<dyn OnEvent> {
        match event {
            Event::ExternalAddressConfirmed(addr) => {
                tracing::debug!(
                    target: LOG_TARGET,
                    "State<TestIfPublic>: External address {addr} confirmed, promoting to Public.",
                );
                command_tx.force_send(Command::ScheduleAutonatClientTest(addr.clone()));
                self.boxed(|state| state.into_public(addr))
            }
            Event::AutonatClientTestFailed(addr) => {
                tracing::debug!(
                    target: LOG_TARGET,
                    "State<TestIfPublic>: AutoNAT could not reach {addr}, mapping {}.",
                    self.state.local_address(),
                );
                command_tx.force_send(Command::MapAddress(self.state.local_address().clone()));
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
    fn external_address_confirmed_transitions_to_public() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfPublic::for_test(ADDR.clone()));
        state_machine.on_test_event(external_address_confirmed());
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
    fn external_address_confirmed_mismatch_transitions_to_public_with_new_addr() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfPublic::for_test(ADDR.clone()));
        state_machine.on_test_event(external_address_confirmed_address_mismatch());
        // Transitions to Public with the confirmed address (ADDR_1), keeping
        // the local address (ADDR)
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &Public::for_test_with_addrs(ADDR.clone(), ADDR_1.clone())
        );
        assert_eq!(
            rx.try_recv(),
            Ok(Command::ScheduleAutonatClientTest(ADDR_1.clone()))
        );
    }

    #[test]
    fn autonat_client_failed_causes_transition_to_try_map_address() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfPublic::for_test(ADDR.clone()));
        state_machine.on_test_event(autonat_failed());
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TryMapAddress::for_test(ADDR.clone())
        );
        assert_eq!(rx.try_recv(), Ok(Command::MapAddress(ADDR.clone())));
    }

    #[test]
    fn autonat_failed_for_observed_address_maps_local_address() {
        let (tx, mut rx) = unbounded_channel();
        let mut state_machine = StateMachine::new(tx);
        state_machine.inner = Some(TestIfPublic::for_test(ADDR.clone()));
        // AutoNAT tested the address a peer observed (ADDR_1), not the local
        // one.
        state_machine.on_test_event(autonat_failed_address_mismatch());
        assert_eq!(
            state_machine.inner.as_ref().unwrap(),
            &TryMapAddress::for_test(ADDR.clone())
        );
        assert_eq!(rx.try_recv(), Ok(Command::MapAddress(ADDR.clone())));
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
