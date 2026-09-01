use core::{hash::Hash, num::NonZeroU64};

use lb_blend::scheduling::membership::Membership;
use tracing::info;

/// Which of the three ways of participating a membership puts this node in.
///
/// **The single statement of the rule.** It used to be written in four places —
/// the orchestrator's choice of which service to start, and one "am I still
/// allowed to run?" check inside each of core and edge — which is three chances
/// for the node to disagree with itself about what it should be doing. A node
/// whose orchestrator says `Edge` while its edge service says "not me" either
/// runs nothing or runs two things.
///
/// So the orchestrator asks this to decide what to start, and a running mode
/// asks the same question to decide whether to stop: `Mode::choose(..) !=
/// Self::MODE`. One rule, one answer, no way for the two to drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Blend through the network as a member of it.
    Core,
    /// Dial into the network from outside it.
    Edge,
    /// Do not blend at all: put payloads straight on the wire.
    Broadcast,
}

impl Mode {
    /// The mode this membership calls for.
    ///
    /// Order matters: a membership below the minimum leaves nothing to blend
    /// *through*, so it means broadcast even for a node that is itself a
    /// member.
    #[must_use]
    pub fn choose<NodeId>(membership: &Membership<NodeId>, minimum_network_size: NonZeroU64) -> Self
    where
        NodeId: Eq + Hash,
    {
        let mode = if membership.size() < minimum_network_size.get() as usize {
            Self::Broadcast
        } else if membership.contains_local() {
            Self::Core
        } else {
            Self::Edge
        };
        info!(
            target: crate::LOG_TARGET,
            diagnostic = "blend_tsi_outage",
            event = "blend_mode_chosen",
            mode = mode.as_ref(),
            membership_count = membership.size(),
            local_is_member = membership.contains_local(),
            "Selected Blend mode from latched membership"
        );
        mode
    }
}

impl AsRef<str> for Mode {
    fn as_ref(&self) -> &str {
        match self {
            Self::Core => "core",
            Self::Edge => "edge",
            Self::Broadcast => "broadcast",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::membership::membership;

    const LOCAL: [u8; 32] = [99; 32];
    const OTHER: [u8; 32] = [1; 32];
    const ANOTHER: [u8; 32] = [2; 32];

    fn minimum(n: u64) -> NonZeroU64 {
        NonZeroU64::new(n).expect("test minimum is non-zero")
    }

    /// Successor to the three `try_new_with_core_condition_check` tests and to
    /// the edge's two error variants: all five were checking clauses of this
    /// one rule from inside whichever service happened to own a copy of it.
    #[test]
    fn membership_decides_the_mode() {
        for (members, min, expected, why) in [
            (
                &[][..],
                1,
                Mode::Broadcast,
                "an empty membership is below any minimum: nothing to blend through",
            ),
            (
                &[OTHER, ANOTHER][..],
                3,
                Mode::Broadcast,
                "below the minimum, and this node is not in it either",
            ),
            (
                &[LOCAL, OTHER][..],
                3,
                Mode::Broadcast,
                "below the minimum wins even when this node is a member",
            ),
            (
                &[OTHER, ANOTHER, [3; 32]][..],
                3,
                Mode::Edge,
                "at the minimum but not a member: dial in from outside",
            ),
            (
                &[LOCAL, OTHER, ANOTHER][..],
                3,
                Mode::Core,
                "at the minimum and a member: the boundary is `<`, not `<=`",
            ),
            (
                &[LOCAL, OTHER, ANOTHER, [3; 32]][..],
                3,
                Mode::Core,
                "above the minimum and a member",
            ),
            (
                &[OTHER, ANOTHER, [3; 32], [4; 32]][..],
                3,
                Mode::Edge,
                "above the minimum and not a member",
            ),
        ] {
            assert_eq!(
                Mode::choose(&membership(members, LOCAL), minimum(min)),
                expected,
                "{why} (members: {}, minimum: {min})",
                members.len()
            );
        }
    }
}
