use core::num::NonZeroU64;

use lb_blend_primitives::time::Round;

/// A count of something a node may do in one round, and no more.
///
/// The share is refreshed by the round rather than counted down from a total,
/// and the round it was refreshed for is kept alongside it, so that a tick the
/// holder never saw — a task can go unpolled while the behaviour is busy —
/// neither grants extra share nor withholds it.
///
/// Both of the protocol's per-round limits are one of these: `r₁`, what a core
/// connection may carry in a round in one direction, and `r_E`, the edge
/// connections a node accepts in a round.
pub struct RoundShare {
    message_limit: NonZeroU64,
    remaining_message_count: u64,
    current: Round,
}

impl RoundShare {
    #[must_use]
    pub const fn new(message_limit: NonZeroU64, current: Round) -> Self {
        Self {
            message_limit,
            remaining_message_count: message_limit.get(),
            current,
        }
    }

    /// Refresh the share if `round` is a later round than the one it was last
    /// refreshed for.
    pub const fn refresh(&mut self, round: Round) {
        if round.rounds_since(self.current) > 0 {
            self.current = round;
            self.remaining_message_count = self.message_limit.get();
        }
    }

    /// Spends one message of the share, reporting whether there was any left.
    pub const fn try_spend(&mut self) -> bool {
        if self.remaining_message_count == 0 {
            return false;
        }
        self.remaining_message_count -= 1;
        true
    }
}
