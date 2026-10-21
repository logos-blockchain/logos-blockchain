//! The catalogue: every chain history the SDK is held to.
//!
//! The SDK sees one thing, an ordered stream of block events plus the
//! answers to its own calls, so a network condition is a shape of that
//! stream, and the shapes that matter are a list. Each entry here is a
//! named, deterministic sequence of what the network does per step, and
//! every story runs on every entry.

use std::borrow::Cow;

use crate::world::{FINALITY_DEPTH, Posted};

/// The finality depth as a fork depth.
const K: usize = FINALITY_DEPTH as usize;

/// How the chain moves in one step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// One block on the tip, `gap` slots later; a gap above one leaves
    /// empty slots.
    Extend { gap: u64 },
    /// A branch from `depth` blocks below the tip with `length` blocks:
    /// longer than `depth` it wins, equal it ties and loses, shorter it
    /// stays dormant. Extends the tip instead when the chain is too short.
    Fork { depth: usize, length: usize },
    /// Extend the dormant branch by `blocks`, maybe overtaking the tip;
    /// extends the tip instead without a dormant branch.
    Revive { blocks: usize },
    /// `silent` blocks the node never announces, then one it does.
    Lagging { silent: usize },
    /// The tip moves silently and a competing block is announced instead.
    SplitTip,
    /// The tip's event delivered again.
    Duplicate,
    /// LIB stops advancing, as under the bootstrap fork-choice rule; the
    /// block that comes with it carries the last LIB that moved.
    FreezeLib,
    /// LIB catches up with the tip; the block that comes with it carries
    /// the jump.
    ThawLib,
    /// Forks below LIB are forgotten; their blocks can no longer be fetched.
    Prune,
}

/// What happens on the client's side of the step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition {
    Quiet,
    /// Every block stream closes; the sequencers reconnect.
    Drop,
    /// The primary node goes down for a step: its mempool and funding
    /// reservations are lost, then it comes back.
    NodeDown,
    /// Restart sequencer `index`, from its checkpoint or, with nothing of
    /// its own pending, from scratch. Nothing happens without that
    /// sequencer.
    Restart {
        index: usize,
        fresh: bool,
    },
    /// The next `count` block fetches fail.
    FetchFail {
        count: usize,
    },
    /// Gas prices move to `price`, in force from the next block.
    GasPrice {
        price: u64,
    },
}

/// One step of a chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Move {
    pub shape: Shape,
    pub posted: Posted,
    pub condition: Condition,
}

/// What a chain promises to have done by the end of a story long enough
/// to see it, checked by [`World::finish`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    Nothing,
    /// At least one reorg.
    Reorg,
    /// A reorg reaching the finality depth.
    DeepReorg,
    /// A stream drop and a sequencer that re-emitted `Ready`.
    Reconnect,
    /// The primary reorged onto the peer's branch and what the peer posted
    /// reached its chain.
    Merge,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    /// The moves, applied cyclically.
    Pattern(Cow<'static, [Move]>),
    /// Two nodes that gossip and periodically split; see
    /// `World::partition_step`.
    Partitions,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chain {
    pub name: Cow<'static, str>,
    kind: Kind,
    pub expect: Expect,
}

impl Chain {
    /// The move for step `n`, `None` on the two-node chain.
    #[must_use]
    pub fn step(&self, n: usize) -> Option<Move> {
        match &self.kind {
            Kind::Pattern(moves) => Some(moves[n % moves.len()]),
            Kind::Partitions => None,
        }
    }

    #[must_use]
    pub const fn is_partitions(&self) -> bool {
        matches!(self.kind, Kind::Partitions)
    }

    /// Steps a story must run for the chain to have shown what it
    /// promises: three cycles of the pattern.
    #[must_use]
    pub fn steps_to_expect(&self) -> usize {
        match &self.kind {
            Kind::Pattern(moves) => 3 * moves.len(),
            Kind::Partitions => 22,
        }
    }
}

const fn mv(shape: Shape, posted: Posted, condition: Condition) -> Move {
    Move {
        shape,
        posted,
        condition,
    }
}

const fn block(posted: Posted, condition: Condition) -> Move {
    mv(Shape::Extend { gap: 1 }, posted, condition)
}

const BLOCK: Move = block(Posted::All, Condition::Quiet);

const fn pattern(name: &'static str, moves: &'static [Move], expect: Expect) -> Chain {
    Chain {
        name: Cow::Borrowed(name),
        kind: Kind::Pattern(Cow::Borrowed(moves)),
        expect,
    }
}

/// The single conditions the catalogue is built from, each as one move.
#[must_use]
pub fn base_conditions() -> Vec<(&'static str, Move)> {
    vec![
        (
            "empty_slots",
            mv(Shape::Extend { gap: 3 }, Posted::All, Condition::Quiet),
        ),
        ("half_posted", block(Posted::Half, Condition::Quiet)),
        ("reversed_posted", block(Posted::Reversed, Condition::Quiet)),
        ("nothing_posted", block(Posted::None, Condition::Quiet)),
        ("shallow_fork", fork(1, 2)),
        ("deep_fork", fork(K, K + 1)),
        ("fork_tie", fork(1, 1)),
        ("dormant_fork", fork(2, 1)),
        (
            "revive",
            mv(Shape::Revive { blocks: 3 }, Posted::All, Condition::Quiet),
        ),
        (
            "split_tip",
            mv(Shape::SplitTip, Posted::All, Condition::Quiet),
        ),
        (
            "lagging",
            mv(Shape::Lagging { silent: 2 }, Posted::All, Condition::Quiet),
        ),
        (
            "duplicate",
            mv(Shape::Duplicate, Posted::All, Condition::Quiet),
        ),
        (
            "freeze_lib",
            mv(Shape::FreezeLib, Posted::All, Condition::Quiet),
        ),
        (
            "thaw_lib",
            mv(Shape::ThawLib, Posted::All, Condition::Quiet),
        ),
        ("prune", mv(Shape::Prune, Posted::All, Condition::Quiet)),
        ("drop", block(Posted::All, Condition::Drop)),
        ("node_down", block(Posted::All, Condition::NodeDown)),
        ("restart", restart(0, false)),
        ("fresh_restart", restart(0, true)),
        (
            "fetch_fail",
            block(Posted::All, Condition::FetchFail { count: 1 }),
        ),
        ("price_up", price(2)),
        ("price_down", price(1)),
    ]
}

/// The ordered pair of base conditions `first` and `second`, two blocks
/// apart, with a finality depth of plain blocks after.
///
/// The second tier of the catalogue, where order matters. A frozen LIB is
/// thawed and a price move undone before the cycle repeats, and the plain
/// blocks let what the pair moved finalize, so the chain can settle.
#[must_use]
pub fn pair(first: &str, second: &str) -> Chain {
    let base = base_conditions();
    let find = |name: &str| {
        base.iter()
            .find(|(n, _)| *n == name)
            .map_or_else(|| panic!("no base condition {name}"), |(_, mv)| *mv)
    };
    let (first_move, second_move) = (find(first), find(second));
    let thaw = mv(Shape::ThawLib, Posted::All, Condition::Quiet);
    let mut moves = vec![
        BLOCK,
        BLOCK,
        first_move,
        BLOCK,
        second_move,
        BLOCK,
        thaw,
        price(1),
    ];
    moves.extend(std::iter::repeat_n(BLOCK, K));
    Chain {
        name: Cow::Owned(format!("{first}_then_{second}")),
        kind: Kind::Pattern(Cow::Owned(moves)),
        expect: Expect::Nothing,
    }
}

/// Invoke `$m!($args; a, b, ...)` with every base condition name, so a
/// macro can emit one test per ordered pair.
#[macro_export]
macro_rules! for_each_base_condition {
    ($m:ident, $($args:tt)*) => {
        $m! { $($args)*;
            empty_slots, half_posted, reversed_posted, nothing_posted, shallow_fork,
            deep_fork, fork_tie, dormant_fork, revive, split_tip, lagging, duplicate,
            freeze_lib, thaw_lib, prune, drop, node_down, restart, fresh_restart,
            fetch_fail, price_up, price_down
        }
    };
}

/// Invoke `$m!($args; chain, chain, ...)` with every catalogue entry, so a
/// macro can emit one test per chain.
#[macro_export]
macro_rules! for_each_chain {
    ($m:ident, $($args:tt)*) => {
        $m! { $($args)*;
            linear, empty_slots, half_posted, reversed_posted, delayed_inclusion,
            shallow_forks, deep_forks, fork_ties, dormant_revived, split_tips, lagging,
            duplicates, lib_frozen, forks_pruned, stream_drops, node_outages, restarts,
            fresh_restarts, fetch_failures, price_moves, fork_after_price_rise,
            restart_under_fork, drop_under_fork, forks_under_frozen_lib, partitions
        }
    };
}

/// Every entry of the catalogue.
#[must_use]
pub fn all() -> Vec<Chain> {
    macro_rules! collect {
        (; $($chain:ident),* $(,)?) => { vec![$($chain()),*] };
    }
    for_each_chain!(collect,)
}

#[must_use]
pub const fn linear() -> Chain {
    pattern("linear", &[BLOCK], Expect::Nothing)
}

/// Blocks with slot gaps between them.
#[must_use]
pub const fn empty_slots() -> Chain {
    const MOVES: [Move; 4] = [
        BLOCK,
        mv(Shape::Extend { gap: 3 }, Posted::All, Condition::Quiet),
        BLOCK,
        mv(Shape::Extend { gap: 2 }, Posted::All, Condition::Quiet),
    ];
    pattern("empty_slots", &MOVES, Expect::Nothing)
}

/// Every block takes every other posted transaction.
#[must_use]
pub const fn half_posted() -> Chain {
    const MOVES: [Move; 1] = [block(Posted::Half, Condition::Quiet)];
    pattern("half_posted", &MOVES, Expect::Nothing)
}

/// Every third block orders the posted transactions in reverse, like a
/// leader ordering differently.
#[must_use]
pub const fn reversed_posted() -> Chain {
    const MOVES: [Move; 3] = [BLOCK, BLOCK, block(Posted::Reversed, Condition::Quiet)];
    pattern("reversed_posted", &MOVES, Expect::Nothing)
}

/// Every fifth block takes nothing, so transactions wait a block.
#[must_use]
pub const fn delayed_inclusion() -> Chain {
    const MOVES: [Move; 5] = [
        BLOCK,
        BLOCK,
        block(Posted::None, Condition::Quiet),
        BLOCK,
        BLOCK,
    ];
    pattern("delayed_inclusion", &MOVES, Expect::Nothing)
}

const fn fork(depth: usize, length: usize) -> Move {
    mv(Shape::Fork { depth, length }, Posted::All, Condition::Quiet)
}

/// Every seventh step the tip block is orphaned by a two-block branch; its
/// transactions return to the mempool.
#[must_use]
pub const fn shallow_forks() -> Chain {
    const MOVES: [Move; 7] = [BLOCK, BLOCK, fork(1, 2), BLOCK, BLOCK, BLOCK, BLOCK];
    pattern("shallow_forks", &MOVES, Expect::Reorg)
}

/// A branch from the last finalized block overtakes the tip, orphaning
/// `FINALITY_DEPTH` blocks at once.
///
/// The chain then runs a finality depth of plain blocks: a reorg that deep
/// is a rare event, and nothing finalizes between two of them otherwise.
#[must_use]
pub fn deep_forks() -> Chain {
    let mut moves = vec![BLOCK; K + 4];
    moves.push(fork(K, K + 1));
    moves.extend([BLOCK, BLOCK]);
    Chain {
        name: Cow::Borrowed("deep_forks"),
        kind: Kind::Pattern(Cow::Owned(moves)),
        expect: Expect::DeepReorg,
    }
}

/// Every fifth step a competing block of the same height takes the mempool
/// and loses.
#[must_use]
pub const fn fork_ties() -> Chain {
    const MOVES: [Move; 5] = [BLOCK, BLOCK, fork(1, 1), BLOCK, BLOCK];
    pattern("fork_ties", &MOVES, Expect::Nothing)
}

/// A short branch stays dormant for a block, then grows past the tip; the
/// chain then runs long enough to finalize what the reorg moved.
#[must_use]
pub const fn dormant_revived() -> Chain {
    const MOVES: [Move; 10] = [
        BLOCK,
        BLOCK,
        fork(2, 1),
        BLOCK,
        mv(Shape::Revive { blocks: 3 }, Posted::All, Condition::Quiet),
        BLOCK,
        BLOCK,
        BLOCK,
        BLOCK,
        BLOCK,
    ];
    pattern("dormant_revived", &MOVES, Expect::Reorg)
}

/// Every fourth step the tip moves silently behind an announced sibling.
#[must_use]
pub const fn split_tips() -> Chain {
    const MOVES: [Move; 4] = [
        BLOCK,
        BLOCK,
        BLOCK,
        mv(Shape::SplitTip, Posted::All, Condition::Quiet),
    ];
    pattern("split_tips", &MOVES, Expect::Nothing)
}

/// Every fifth step two blocks go unannounced before one that is.
#[must_use]
pub const fn lagging() -> Chain {
    const MOVES: [Move; 5] = [
        BLOCK,
        BLOCK,
        BLOCK,
        BLOCK,
        mv(Shape::Lagging { silent: 2 }, Posted::All, Condition::Quiet),
    ];
    pattern("lagging", &MOVES, Expect::Nothing)
}

/// Every third step the tip's event is delivered twice.
#[must_use]
pub const fn duplicates() -> Chain {
    const MOVES: [Move; 3] = [
        BLOCK,
        BLOCK,
        mv(Shape::Duplicate, Posted::All, Condition::Quiet),
    ];
    pattern("duplicates", &MOVES, Expect::Nothing)
}

/// LIB freezes for four blocks, then jumps to the tip.
#[must_use]
pub const fn lib_frozen() -> Chain {
    const MOVES: [Move; 10] = [
        mv(Shape::FreezeLib, Posted::All, Condition::Quiet),
        BLOCK,
        BLOCK,
        BLOCK,
        mv(Shape::ThawLib, Posted::All, Condition::Quiet),
        BLOCK,
        BLOCK,
        BLOCK,
        BLOCK,
        BLOCK,
    ];
    pattern("lib_frozen", &MOVES, Expect::Nothing)
}

/// A losing fork is pruned once LIB passes it, so its blocks can no longer
/// be fetched.
#[must_use]
pub const fn forks_pruned() -> Chain {
    const MOVES: [Move; 6] = [
        fork(1, 1),
        BLOCK,
        BLOCK,
        BLOCK,
        mv(Shape::Prune, Posted::All, Condition::Quiet),
        BLOCK,
    ];
    pattern("forks_pruned", &MOVES, Expect::Nothing)
}

/// Every seventh step the block streams close before the block.
#[must_use]
pub const fn stream_drops() -> Chain {
    const MOVES: [Move; 7] = [
        BLOCK,
        block(Posted::All, Condition::Drop),
        BLOCK,
        BLOCK,
        BLOCK,
        BLOCK,
        BLOCK,
    ];
    pattern("stream_drops", &MOVES, Expect::Reconnect)
}

/// Every seventh step the node is down for a step.
#[must_use]
pub const fn node_outages() -> Chain {
    const MOVES: [Move; 7] = [
        BLOCK,
        BLOCK,
        BLOCK,
        block(Posted::All, Condition::NodeDown),
        BLOCK,
        BLOCK,
        BLOCK,
    ];
    pattern("node_outages", &MOVES, Expect::Nothing)
}

const fn restart(index: usize, fresh: bool) -> Move {
    block(Posted::All, Condition::Restart { index, fresh })
}

/// The first two sequencers restart from their checkpoints in turn.
#[must_use]
pub const fn restarts() -> Chain {
    const MOVES: [Move; 12] = [
        BLOCK,
        BLOCK,
        BLOCK,
        BLOCK,
        BLOCK,
        restart(0, false),
        BLOCK,
        BLOCK,
        BLOCK,
        BLOCK,
        BLOCK,
        restart(1, false),
    ];
    pattern("restarts", &MOVES, Expect::Nothing)
}

/// The first sequencer restarts from scratch whenever it has nothing of its
/// own pending.
#[must_use]
pub const fn fresh_restarts() -> Chain {
    const MOVES: [Move; 6] = [BLOCK, BLOCK, BLOCK, BLOCK, BLOCK, restart(0, true)];
    pattern("fresh_restarts", &MOVES, Expect::Nothing)
}

/// Every fifth step the next block fetch fails.
#[must_use]
pub const fn fetch_failures() -> Chain {
    const MOVES: [Move; 5] = [
        BLOCK,
        BLOCK,
        block(Posted::All, Condition::FetchFail { count: 1 }),
        BLOCK,
        BLOCK,
    ];
    pattern("fetch_failures", &MOVES, Expect::Nothing)
}

const fn price(price: u64) -> Move {
    block(Posted::All, Condition::GasPrice { price })
}

/// Gas prices double for two blocks, then fall back: what was funded at
/// the old price is evicted and has to come back.
#[must_use]
pub const fn price_moves() -> Chain {
    const MOVES: [Move; 10] = [
        BLOCK,
        BLOCK,
        BLOCK,
        BLOCK,
        price(2),
        BLOCK,
        price(1),
        BLOCK,
        BLOCK,
        BLOCK,
    ];
    pattern("price_moves", &MOVES, Expect::Nothing)
}

/// Prices double, then a fork orphans what was funded at the old price.
#[must_use]
pub const fn fork_after_price_rise() -> Chain {
    const MOVES: [Move; 8] = [
        BLOCK,
        BLOCK,
        BLOCK,
        price(2),
        fork(2, 3),
        price(1),
        BLOCK,
        BLOCK,
    ];
    pattern("fork_after_price_rise", &MOVES, Expect::Reorg)
}

/// A sequencer restarts while a dormant branch waits, which then wins.
#[must_use]
pub const fn restart_under_fork() -> Chain {
    const MOVES: [Move; 10] = [
        BLOCK,
        BLOCK,
        fork(2, 1),
        restart(0, false),
        mv(Shape::Revive { blocks: 3 }, Posted::All, Condition::Quiet),
        BLOCK,
        BLOCK,
        BLOCK,
        BLOCK,
        BLOCK,
    ];
    pattern("restart_under_fork", &MOVES, Expect::Reorg)
}

/// The streams drop while a dormant branch waits, which then wins.
#[must_use]
pub const fn drop_under_fork() -> Chain {
    const MOVES: [Move; 10] = [
        BLOCK,
        BLOCK,
        fork(2, 1),
        block(Posted::All, Condition::Drop),
        mv(Shape::Revive { blocks: 3 }, Posted::All, Condition::Quiet),
        BLOCK,
        BLOCK,
        BLOCK,
        BLOCK,
        BLOCK,
    ];
    pattern("drop_under_fork", &MOVES, Expect::Reconnect)
}

/// Forks of both depths while LIB is frozen, then LIB jumps.
#[must_use]
pub const fn forks_under_frozen_lib() -> Chain {
    const MOVES: [Move; 10] = [
        mv(Shape::FreezeLib, Posted::All, Condition::Quiet),
        fork(1, 2),
        BLOCK,
        fork(2, 3),
        mv(Shape::ThawLib, Posted::All, Condition::Quiet),
        BLOCK,
        BLOCK,
        BLOCK,
        BLOCK,
        BLOCK,
    ];
    pattern("forks_under_frozen_lib", &MOVES, Expect::Reorg)
}

/// Two nodes that gossip transactions and blocks; every other sequencer
/// talks to the peer. Every eleventh step the nodes split for three
/// blocks and the peer gets ahead, so the merge reorgs the primary.
#[must_use]
pub const fn partitions() -> Chain {
    Chain {
        name: Cow::Borrowed("partitions"),
        kind: Kind::Partitions,
        expect: Expect::Merge,
    }
}
