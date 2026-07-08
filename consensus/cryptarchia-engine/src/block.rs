use core::hash::Hash;
use std::num::NonZero;

use rpds::HashTrieMapSync;

use crate::time::Slot;

/// Holds the immutable facts about a block: its identity, parentage, slot
/// and chain length.
///
/// Values of this type are plain data, safe to copy and hold across engine
/// mutations. A block's *role* in the tree (LIB, fork point, tip) changes as
/// the tree evolves, so it is deliberately not part of this type: role
/// questions are only answerable by the live block tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Branch<Id> {
    id: Id,
    parent: Id,
    slot: Slot,
    length: u64,
}

impl<Id: Copy> Branch<Id> {
    pub(crate) const fn new(id: Id, parent: Id, slot: Slot, length: u64) -> Self {
        Self {
            id,
            parent,
            slot,
            length,
        }
    }

    /// The block id.
    pub const fn id(&self) -> Id {
        self.id
    }

    /// The parent block id.
    ///
    /// Tree-root convention: the block the tree was created from is its own
    /// parent. Code that walks parent links must therefore either stop when
    /// `parent() == id()` or treat a failed lookup of the parent as the end
    /// of the in-memory lineage (the parent of an advanced LIB is real but
    /// pruned from memory).
    ///
    /// Instead of manually walking parent links, use the `LineageIterator`
    /// which walks back the chain starting from a provided initial block
    /// and naturally stops at the LIB block.
    pub const fn parent(&self) -> Id {
        self.parent
    }

    /// The slot the block belongs to.
    pub const fn slot(&self) -> Slot {
        self.slot
    }

    /// The chain length up to and including this block.
    pub const fn length(&self) -> u64 {
        self.length
    }
}

/// A block's current role in the block tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The root of the in-memory tree: the latest immutable block.
    /// Its ancestors have been pruned from memory.
    Lib,
    /// An interior block with at least one child.
    ///
    /// The count lets fork pruning turn a block back into a `Role::Tip`
    /// when its last remaining child is removed.
    Internal { children_count: NonZero<usize> },
    /// A leaf: a chain tip with no children.
    Tip,
}

/// A block-tree map entry: the immutable block data plus its current role.
///
/// `Block` values must not escape the tree, the role is only true for as
/// long as the tree is not mutated. Public APIs hand out `Branch` copies
/// instead, which cannot go stale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block<Id> {
    pub(crate) branch: Branch<Id>,
    pub(crate) role: Role,
}

impl<Id: Copy> Block<Id> {
    pub(crate) const fn is_lib(&self) -> bool {
        matches!(self.role, Role::Lib)
    }

    pub(crate) const fn is_tip(&self) -> bool {
        matches!(self.role, Role::Tip)
    }

    pub(crate) const fn has_single_child(&self) -> bool {
        matches!(self.role, Role::Internal{children_count} if children_count.get() == 1)
    }

    pub(crate) fn with_child_added(self) -> Self {
        let role = match self.role {
            Role::Lib => Role::Lib,
            Role::Tip => Role::Internal {
                children_count: NonZero::<usize>::MIN,
            },
            Role::Internal { children_count: n } => Role::Internal {
                // SOUNDNESS: n >= 1, so n + 1 >= 2.
                children_count: (n.get() + 1).try_into().unwrap(),
            },
        };
        Self { role, ..self }
    }

    pub(crate) fn with_child_removed(self) -> Self {
        let role = match self.role {
            Role::Lib => Role::Lib,
            Role::Tip => unreachable!("a childless block cannot lose a child"),
            Role::Internal { children_count } => match children_count.get() {
                1 => Role::Tip,
                n => Role::Internal {
                    // SOUNDNESS: this arm only matches n >= 2, so n - 1 >= 1.
                    children_count: (n - 1).try_into().unwrap(),
                },
            },
        };
        Self { role, ..self }
    }
}

/// Iterates a lineage from the given block (inclusive) up to the LIB
/// (inclusive), following parent links.
///
/// Every element is fetched from the map by id, so the yielded blocks
/// always reflect the tree the iterator was created over.
pub struct LineageIterator<'a, Id> {
    cursor: Option<Id>,
    blocks: &'a HashTrieMapSync<Id, Block<Id>>,
}

impl<'a, Id> LineageIterator<'a, Id> {
    pub(crate) const fn new(cursor: Id, blocks: &'a HashTrieMapSync<Id, Block<Id>>) -> Self {
        Self {
            cursor: Some(cursor),
            blocks,
        }
    }
}

impl<'a, Id: Copy + Eq + Hash> Iterator for LineageIterator<'a, Id> {
    type Item = &'a Block<Id>;

    fn next(&mut self) -> Option<Self::Item> {
        let id = self.cursor?;
        // SOUNDNESS: a lineage walk only follows parent links of in-tree
        // blocks, pruning removes children before their parents, and the
        // walk stops at the LIB before reaching its (pruned) parent.
        let block = self.blocks.get(&id).unwrap();
        self.cursor = if block.is_lib() {
            None
        } else {
            Some(block.branch.parent())
        };
        Some(block)
    }
}

/// Pairs each element with whether it is the last one, via one-element
/// lookahead.
///
/// The lookahead runs the upstream iterator one step ahead of the consumer,
/// so this is only appropriate over side-effect-free upstreams.
pub struct WithIsLast<I: Iterator> {
    inner: std::iter::Peekable<I>,
}

impl<I: Iterator> Iterator for WithIsLast<I> {
    type Item = (I::Item, bool);

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.inner.next()?;
        Some((item, self.inner.peek().is_none()))
    }
}

/// Extension trait adding [`WithIsLast`] to any iterator.
pub trait WithIsLastExt: Iterator + Sized {
    /// Pairs each element with whether it is the last one.
    fn with_is_last(self) -> WithIsLast<Self> {
        WithIsLast {
            inner: self.peekable(),
        }
    }
}

impl<I: Iterator> WithIsLastExt for I {}

#[cfg(test)]
mod tests {
    use super::WithIsLastExt as _;

    #[test]
    fn with_is_last_flags_only_the_final_element() {
        let empty: Vec<(u8, bool)> = std::iter::empty().with_is_last().collect();
        assert_eq!(empty, vec![]);

        let single: Vec<_> = std::iter::once(7).with_is_last().collect();
        assert_eq!(single, vec![(7, true)]);

        let multi: Vec<_> = [1, 2, 3].into_iter().with_is_last().collect();
        assert_eq!(multi, vec![(1, false), (2, false), (3, true)]);
    }
}
