//! Type-state tokens enforcing the LIB-update protocol at compile time.
//!
//! Updating the LIB is a three-phase mutation:
//!
//! 1. stale forks are pruned first
//! 2. immutable blocks are pruned next
//! 3. the new LIB block is written last.
//!
//! Each phase consumes the previous token by value, so the phases cannot be
//! reordered, repeated or skipped without a compile error.
//!
//! Once started, ignoring any step of the protocol, i.e. by not using a
//! returned value, is linted against by `must_use`.

use core::{fmt::Debug, hash::Hash};
use std::collections::{BTreeMap, HashSet};

use crate::{
    Branch, Cryptarchia, PrunedBlocks, Slot,
    block::{Block, Role},
};

#[must_use = "a started LIB update must be committed"]
pub struct LibUpdate<Id> {
    new_lib: Branch<Id>,
}

#[must_use = "a started LIB update must be committed"]
pub struct StalePruned<Id> {
    new_lib: Branch<Id>,
    stale_blocks: HashSet<Id>,
}

#[must_use = "a started LIB update must be committed"]
pub struct ImmutablePruned<Id> {
    new_lib: Branch<Id>,
    stale_blocks: HashSet<Id>,
    immutable_blocks: BTreeMap<Slot, Id>,
}

impl<Id: Eq + Hash + Copy + Debug> LibUpdate<Id> {
    pub const fn new(new_lib: Branch<Id>) -> Self {
        Self { new_lib }
    }

    pub fn prune_stale_forks(self, tree: &mut Cryptarchia<Id>) -> StalePruned<Id> {
        let stale_blocks = tree.prune_stale_forks(self.new_lib.length());
        StalePruned {
            new_lib: self.new_lib,
            stale_blocks,
        }
    }
}

impl<Id: Eq + Hash + Copy + Debug> StalePruned<Id> {
    pub fn prune_immutable_blocks(self, tree: &mut Cryptarchia<Id>) -> ImmutablePruned<Id> {
        let immutable_blocks = tree.prune_immutable_blocks(self.new_lib.id());
        ImmutablePruned {
            new_lib: self.new_lib,
            stale_blocks: self.stale_blocks,
            immutable_blocks,
        }
    }
}

impl<Id: Eq + Hash + Copy + Debug> ImmutablePruned<Id> {
    pub fn commit(self, tree: &mut Cryptarchia<Id>) -> PrunedBlocks<Id> {
        tree.branches.lib = self.new_lib.id();
        tree.branches.insert_mut(Block {
            branch: self.new_lib,
            role: Role::Lib,
        });
        PrunedBlocks {
            stale_blocks: self.stale_blocks,
            immutable_blocks: self.immutable_blocks,
        }
    }
}
