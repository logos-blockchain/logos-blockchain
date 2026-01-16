use std::fmt::Debug;

use libp2p::Multiaddr;
use tokio::sync::mpsc::UnboundedSender;

mod event;
mod states;
// #[cfg(test)]
// mod tests;
mod transitions;

use event::Event;
use states::Uninitialized;

/// Commands that can be issued by the state machine to `NatBehaviour`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    ScheduleAutonatClientTest(Multiaddr),
    MapAddress(Multiaddr),
    NewExternalAddrCandidate(Multiaddr),
}

#[derive(Debug)]
pub struct StateMachine {
    inner: Option<Box<dyn OnEvent>>,
    command_tx: CommandTx,
}

impl StateMachine {
    pub fn new(command_tx: UnboundedSender<Command>) -> Self {
        Self {
            inner: Some(Box::new(State::<Uninitialized>::new())),
            command_tx: command_tx.into(),
        }
    }

    pub fn on_event<E>(&mut self, event: E)
    where
        E: TryInto<Event, Error = ()>,
    {
        let current_state = self.inner.take().expect("State to be Some");

        match event.try_into() {
            Err(()) => {
                // Ignore unrecognized events
                self.inner = Some(current_state);
            }
            Ok(event) => {
                self.inner = Some(current_state.on_event(event, &self.command_tx));
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct State<S> {
    pub state: S,
}

#[derive(Debug, Clone)]
struct CommandTx {
    tx: UnboundedSender<Command>,
}

impl From<UnboundedSender<Command>> for CommandTx {
    fn from(tx: UnboundedSender<Command>) -> Self {
        Self { tx }
    }
}

impl CommandTx {
    fn force_send(&self, command: Command) {
        self.tx.send(command).expect("Channel not to be closed");
    }
}

trait OnEvent: Debug + Send {
    fn on_event(self: Box<Self>, event: Event, command_tx: &CommandTx) -> Box<dyn OnEvent>;
}

impl<S> State<S> {
    fn boxed<T, C>(self, next_state_ctor: C) -> Box<State<T>>
    where
        C: FnOnce(S) -> T,
    {
        let Self { state, .. } = self;
        Box::new(State {
            state: next_state_ctor(state),
        })
    }
}
