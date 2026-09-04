use core::{hash::Hash, num::NonZeroU64};

use lb_blend::scheduling::membership::Membership;
use lb_core::crypto::ZkHash;
use tracing::info;

use crate::membership::{MembershipInfo, ZkInfo};

/// Which of the three ways of participating a membership puts this node in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Core,
    Edge,
    Broadcast,
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

pub enum ModeMembership<NodeId> {
    Core(CoreMembership<NodeId>),
    Edge(EdgeMembership<NodeId>),
    Broadcast,
}

/// What core mode needs: the membership it blends through, and its own path
/// into the core Merkle tree, without which it could not mint a `PoQ`.
pub struct CoreMembership<NodeId> {
    pub membership: Membership<NodeId>,
    pub zk_root: ZkHash,
}

/// What edge mode needs: the core nodes to dial, and the tree root its proofs
/// are checked against.
pub struct EdgeMembership<NodeId> {
    pub membership: Membership<NodeId>,
    pub zk_root: ZkHash,
}

impl<NodeId> ModeMembership<NodeId> {
    pub fn resolve(
        MembershipInfo { membership, zk }: MembershipInfo<NodeId>,
        minimum_network_size: NonZeroU64,
    ) -> Self
    where
        NodeId: Eq + Hash,
    {
        let local_is_member = membership.contains_local();
        let membership_count = membership.size();

        let mode = if membership_count < minimum_network_size.get() as usize {
            Self::Broadcast
        } else {
            match zk {
                // Only an empty membership has no `zk` info, and an
                // empty one is below any minimum, so this is
                // unreachable in practice.
                None => Self::Broadcast,
                Some(ZkInfo { root, .. }) if local_is_member => Self::Core(CoreMembership {
                    membership,
                    zk_root: root,
                }),
                Some(ZkInfo { root, .. }) => Self::Edge(EdgeMembership {
                    membership,
                    zk_root: root,
                }),
            }
        };

        let chosen = mode.mode();
        info!(
            target: crate::LOG_TARGET,
            diagnostic = "blend_tsi_outage",
            event = "blend_mode_chosen",
            mode = chosen.as_ref(),
            membership_count,
            local_is_member,
            "Selected Blend mode from latched membership"
        );
        mode
    }

    #[must_use]
    pub const fn mode(&self) -> Mode {
        match self {
            Self::Core(_) => Mode::Core,
            Self::Edge(_) => Mode::Edge,
            Self::Broadcast => Mode::Broadcast,
        }
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::AdditiveGroup as _;
    use lb_poq::CORE_MERKLE_TREE_HEIGHT;

    use super::*;
    use crate::test_utils::membership::membership;

    const LOCAL: [u8; 32] = [99; 32];
    const OTHER: [u8; 32] = [1; 32];
    const ANOTHER: [u8; 32] = [2; 32];

    fn minimum(n: u64) -> NonZeroU64 {
        NonZeroU64::new(n).expect("test minimum is non-zero")
    }

    fn info(members: &[[u8; 32]], has_path: bool) -> MembershipInfo<[u8; 32]> {
        MembershipInfo {
            membership: membership(members, LOCAL),
            zk: (!members.is_empty()).then(|| ZkInfo {
                root: ZkHash::ZERO,
                core_and_path_selectors: has_path
                    .then_some([(ZkHash::ZERO, false); CORE_MERKLE_TREE_HEIGHT]),
            }),
        }
    }

    /// Successor to the three `try_new_with_core_condition_check` tests and to
    /// the edge's two error variants: all five were checking clauses of this
    /// one rule from inside whichever service happened to own a copy of it.
    #[test]
    fn membership_decides_the_mode() {
        for (members, min, has_path, expected, why) in [
            (
                &[][..],
                1,
                false,
                Mode::Broadcast,
                "an empty membership is below any minimum: nothing to blend through",
            ),
            (
                &[OTHER, ANOTHER][..],
                3,
                false,
                Mode::Broadcast,
                "below the minimum, and this node is not in it either",
            ),
            (
                &[LOCAL, OTHER][..],
                3,
                true,
                Mode::Broadcast,
                "below the minimum wins even when this node is a member with a path",
            ),
            (
                &[OTHER, ANOTHER, [3; 32]][..],
                3,
                false,
                Mode::Edge,
                "at the minimum but not a member: dial in from outside",
            ),
            (
                &[LOCAL, OTHER, ANOTHER][..],
                3,
                true,
                Mode::Core,
                "at the minimum and a member: the boundary is `<`, not `<=`",
            ),
            (
                &[LOCAL, OTHER, ANOTHER, [3; 32]][..],
                3,
                true,
                Mode::Core,
                "above the minimum and a member",
            ),
            (
                &[OTHER, ANOTHER, [3; 32], [4; 32]][..],
                3,
                false,
                Mode::Edge,
                "above the minimum and not a member",
            ),
            (
                &[LOCAL, OTHER, ANOTHER][..],
                3,
                false,
                Mode::Core,
                "the Merkle path is not part of this rule: membership is by \
                 Ed25519 `provider_id`, and core applies the path requirement \
                 itself, where it has the key to look one up",
            ),
        ] {
            assert_eq!(
                ModeMembership::resolve(info(members, has_path), minimum(min)).mode(),
                expected,
                "{why} (members: {}, minimum: {min}, has_path: {has_path})",
                members.len()
            );
        }
    }

    /// The mode does not just name itself, it hands over what it runs on.
    #[test]
    fn each_mode_carries_what_it_needs() {
        let ModeMembership::Core(core) =
            ModeMembership::resolve(info(&[LOCAL, OTHER, ANOTHER], true), minimum(3))
        else {
            panic!("a member with a path is a core node");
        };
        assert_eq!(core.membership.size(), 3);
        assert_eq!(core.zk_root, ZkHash::ZERO);

        let ModeMembership::Edge(edge) =
            ModeMembership::resolve(info(&[OTHER, ANOTHER, [3; 32]], false), minimum(3))
        else {
            panic!("a non-member of a big enough network is an edge node");
        };
        assert_eq!(edge.membership.size(), 3);
        assert_eq!(edge.zk_root, ZkHash::ZERO);
    }
}
