//! One event per operation the world exposes.
//!
//! A scenario is a list of events whatever produces it: a catalogue chain,
//! a hand-written story or the random generator. `World::apply_event`
//! resolves what the current state cannot do, the way the network would: an
//! impossible fork becomes a plain block, an action on a sequencer that is
//! not there is skipped.

use crate::{
    chains::{Condition, Shape},
    world::Posted,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The chain moves: a block of `shape` taking `posted`.
    Chain { shape: Shape, posted: Posted },
    /// Something happens on the client's side.
    Client(Condition),
    /// Sequencer `index` publishes `payload`.
    Publish { index: usize, payload: String },
    /// The user deposits one of its pre-minted notes into the channel.
    Deposit,
    /// Sequencer `index` withdraws `amount` to the user, from finalized
    /// notes or, with `unfinalized`, from any the SDK picks.
    Withdraw {
        index: usize,
        amount: u64,
        unfinalized: bool,
    },
    /// Sequencer `index` pins a deposit whose notes its wallet holds.
    Pin { index: usize },
    /// Sequencer `index` submits a config accrediting itself and `names`.
    Config {
        index: usize,
        names: Vec<String>,
        timeframe: u32,
        timeout: u32,
    },
    /// Sequencer `index` submits a hand-built tx chaining `payloads`.
    Custom { index: usize, payloads: Vec<String> },
    /// Sequencer `index` lets its next orphan die.
    Abandon { index: usize },
    /// Sequencer `index` publishes `payloads` at once on its next turn.
    Burst { index: usize, payloads: Vec<String> },
    /// Sequencer `index` starts or stops republishing others' orphans.
    Adopt { index: usize, on: bool },
    /// Sequencer `index` stops; its checkpoint is kept.
    Stop { index: usize },
    /// Sequencer `index` starts again from its last checkpoint.
    Start { index: usize },
}
