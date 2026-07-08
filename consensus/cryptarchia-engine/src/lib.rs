//! The Cryptarchia consensus engine: an in-memory block tree with
//! Ouroboros-style fork choice, Genesis while bootstrapping, Praos when
//! online, and pruning driven by advances of the LIB, the latest
//! immutable block.

mod block;
/// Consensus configuration: the security parameter, slot activation
/// coefficient and lottery constants.
pub mod config;
/// Slots, epochs and slot timing.
pub mod time;
mod typestate;

use core::{fmt::Debug, hash::Hash};
use std::{
    collections::{BTreeMap, HashSet},
    num::NonZero,
};

use crate::block::{Block, LineageIterator, Role, WithIsLastExt as _};
pub use block::Branch;
pub use config::*;

use rpds::{HashTrieMapSync, HashTrieSetSync};
use thiserror::Error;
pub use time::{Epoch, EpochConfig, Slot};

pub(crate) const LOG_TARGET: &str = "cryptarchia::engine";

/// The engine's operating mode, which selects the fork choice rule and the
/// LIB update policy.
#[derive(Clone, Debug, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum State {
    /// Catching up with the network: Ouroboros Genesis fork choice, LIB
    /// frozen at the block the engine was created from.
    Bootstrapping,
    /// Caught up: Ouroboros Praos fork choice, LIB trailing the local chain
    /// tip by the security parameter.
    Online,
}

impl State {
    /// Whether the engine is bootstrapping.
    #[must_use]
    pub const fn is_bootstrapping(&self) -> bool {
        matches!(self, Self::Bootstrapping)
    }

    /// Whether the engine is online.
    #[must_use]
    pub const fn is_online(&self) -> bool {
        matches!(self, Self::Online)
    }
}

/// Implementation of the fork choice rule as defined in the Ouroboros Genesis
/// paper k defines the forking depth of chain we accept without more
/// analysis s defines the length of time (unit of slots) after the fork
/// happened we will inspect for chain density
fn maxvalid_bg<Id>(
    local_chain: Branch<Id>,
    branches: &Branches<Id>,
    k: u64,
    s_gen: NonZero<u64>,
) -> Branch<Id>
where
    Id: Eq + Hash + Copy,
{
    let mut cmax = local_chain;

    let forks = branches.branches();
    for chain in forks {
        let lowest_common_ancestor = branches.lca(&cmax, &chain);

        // SOUNDNESS: LCA is found on `cmax`'s own lineage, where lengths
        // strictly decrease walking parent-ward, so its length is at most
        // `cmax.length()` so the subtraction cannot underflow.
        let m = cmax.length() - lowest_common_ancestor.length();

        if m <= k {
            // Classic longest chain rule with parameter k
            if cmax.length() < chain.length() {
                cmax = chain;
            }
        } else {
            // The chain is forking too much, we need to pay a bit more attention
            // In particular, select the chain that is the densest after the fork
            let density_slot = lowest_common_ancestor.slot().strict_add(s_gen.get().into());

            // SOUNDNESS:
            //            a0 - a1 - a2 - a3 - a4 - a5 - a6 - cmax - a6 - a7 - a8 - ...
            //           /     ^ density_slot maybe here       ^ or here      ^ or here
            // ... b0 - lca
            //           \
            //           c0 - c1 - c2 - c3 - chain - c5 - ...
            //
            //  Given density_slot is past LCA, at LCA+s_gen, then
            //  - if cmax lies after the density_slot then density_slot is returned
            //  - if cmax lies before or at the density_slot then cmax is returned
            //  So since in all cases the walk finds an entry, it's safe to unwrap.
            let cmax_density = branches
                .walk_back_before(&cmax, density_slot)
                .unwrap()
                .length();

            // SOUNDNESS: same argument as for cmax.
            let candidate_density = branches
                .walk_back_before(&chain, density_slot)
                .unwrap()
                .length();

            if cmax_density < candidate_density {
                cmax = chain;
            }
        }
    }
    cmax
}

/// Implementation of the fork choice rule as defined in the Ouroboros Praos
/// paper k defines the forking depth of chain we can accept.
fn maxvalid_mc<Id>(local_chain: Branch<Id>, branches: &Branches<Id>, k: u64) -> Branch<Id>
where
    Id: Eq + Hash + Copy,
{
    let mut cmax = local_chain;

    let forks = branches.branches();
    for chain in forks {
        let lowest_common_ancestor = branches.lca(&cmax, &chain);

        // SOUNDNESS: same argument as in `maxvalid_bg`.
        let m = cmax.length() - lowest_common_ancestor.length();

        if m <= k && cmax.length() < chain.length() {
            // Classic longest chain rule with parameter k
            cmax = chain;
        }
    }
    cmax
}

/// The consensus engine: a block tree plus the configuration and operating
/// mode that drive fork choice and LIB updates.
#[derive(Clone, Debug, PartialEq)]
pub struct Cryptarchia<Id>
where
    Id: Eq + Hash,
{
    local_chain: Branch<Id>,
    branches: Branches<Id>,
    config: Config,
    state: State,
}

/// The in-memory block tree, rooted at the LIB: every known block, the set
/// of chain tips, and the LIB pointer.
#[derive(Clone, Debug)]
pub struct Branches<Id>
where
    Id: Eq + Hash,
{
    blocks: HashTrieMapSync<Id, Block<Id>>,
    tips: HashTrieSetSync<Id>,
    lib: Id,
}

impl<Id> PartialEq for Branches<Id>
where
    Id: Eq + Hash,
{
    fn eq(&self, other: &Self) -> bool {
        self.blocks == other.blocks && self.tips == other.tips && self.lib == other.lib
    }
}

impl<Id> Branches<Id>
where
    Id: Eq + Hash + Copy,
{
    /// Creates a block tree with `lib` as its only block and root.
    pub fn from_lib(lib: Id, slot: Slot, length: u64) -> Self {
        let mut blocks = HashTrieMapSync::new_sync();
        blocks.insert_mut(
            lib,
            Block {
                // tree-root convention: set the root to be its own parent.
                branch: Branch::new(lib, lib, slot, length),
                role: Role::Lib,
            },
        );
        let mut tips = HashTrieSetSync::new_sync();
        tips.insert_mut(lib);
        Self { blocks, tips, lib }
    }

    /// Apply a new header.
    ///
    /// Re-applying a header that is already in the tree is a no-op: it is
    /// logged and `Ok` is returned without modifying `self`.
    ///
    /// On error, `self` is not modified.
    fn apply_header(&mut self, id: Id, parent_id: Id, slot: Slot) -> Result<(), Error<Id>>
    where
        Id: Debug,
    {
        let parent = *self
            .blocks
            .get(&parent_id)
            .ok_or(Error::ParentMissing(parent_id))?;

        if parent.branch.slot() > slot {
            return Err(Error::InvalidSlot(parent_id));
        }

        // Re-inserting an existing block must be refused:
        // - inflates its parent's children_count causing leaks in `prune_stale_forks`
        // - erases existing block's children_count. Pruning may then remove this block leaving
        // its children no way to walk back to it which results in a panic
        // - the tips set regains a block that may have children, feeding
        //   fork choice and fork pruning a candidate that is not a leaf.
        if let Some(branch) = self.get(&id) {
            tracing::debug!(
                target: LOG_TARGET,
                "Header {branch:?} already in the tree. Re-insertion attempted with parent_id: {parent_id:?} and slot: {slot:?}"
            );
            return Ok(());
        }

        let length = parent
            .branch
            .length()
            .checked_add(1)
            .expect("New branch height overflows.");

        self.insert_mut(parent.with_child_added());

        self.tips.remove_mut(&parent_id);
        self.tips.insert_mut(id);

        self.insert_mut(Block {
            branch: Branch::new(id, parent_id, slot, length),
            role: Role::Tip,
        });

        Ok(())
    }

    /// Iterates over the blocks at the current chain tips.
    pub fn branches(&self) -> impl Iterator<Item = Branch<Id>> + '_ {
        // SOUNDNESS: the tips set only contains ids of blocks in the map
        // both are updated together on insert and prune.
        self.tips.iter().map(|id| self.blocks[id].branch)
    }

    /// Find the lowest common ancestor of two branches.
    pub fn lca(&self, b1: &Branch<Id>, b2: &Branch<Id>) -> Branch<Id> {
        assert!(
            self.blocks.contains_key(&b1.id()) && self.blocks.contains_key(&b2.id()),
            "lca() requires branches that are currently in the tree"
        );

        let mut it1 = LineageIterator::new(b1.id(), &self.blocks);
        let mut it2 = LineageIterator::new(b2.id(), &self.blocks);

        // first reduce branches to the same length
        // SOUNDNESS: each `+ 1` is guarded by its arm's strict inequality, so
        // the result is at most the longer branch's length and cannot overflow.
        if b1.length() > b2.length() {
            let _ = it1.find(|block| block.branch.length() == b2.length() + 1);
        } else if b2.length() > b1.length() {
            let _ = it2.find(|block| block.branch.length() == b1.length() + 1);
        }

        // then walk up the chain until we find the common ancestor
        // SOUNDNESS: b1 and b2 have LIB in common if nothing else.
        it1.zip(it2)
            .find(|(n1, n2)| n1.branch.id() == n2.branch.id())
            .unwrap()
            .0
            .branch
    }

    /// Returns the block with the given id, if it is in the tree.
    pub fn get(&self, id: &Id) -> Option<&Branch<Id>> {
        self.blocks.get(id).map(|block| &block.branch)
    }

    /// Returns the chain length of the given block, if it is in the tree.
    pub fn get_length_for_header(&self, header_id: &Id) -> Option<u64> {
        self.get(header_id).map(Branch::length)
    }

    /// Walk back the chain until the target slot.
    fn walk_back_before(&self, branch: &Branch<Id>, slot: Slot) -> Option<Branch<Id>> {
        LineageIterator::new(branch.id(), &self.blocks)
            .find(|block| block.branch.slot() <= slot)
            .map(|block| block.branch)
    }

    /// Walk back the chain and return all blocks in the range
    /// `[branch, target_exclusive)`.
    fn walk_back_to_block(&self, branch: Id, target_exclusive: Id) -> impl Iterator<Item = Id> {
        LineageIterator::new(branch, &self.blocks)
            .map(|block| block.branch.id())
            .take_while(move |id| id != &target_exclusive)
    }

    /// Returns the min(n, A)-th ancestor of the provided block, where A is the
    /// number of ancestors of this block.
    ///
    /// # Example
    ///
    /// Each block is labelled with the `n` that reaches it from `a4`:
    ///
    /// ```text
    ///       LIB --- a1 --- a2 --- a3 --- a4
    /// n:    >=4     3      2      1      0
    /// ```
    ///
    /// `nth_ancestor(a4, 0)` returns `a4` itself,
    /// `nth_ancestor(a4, 2)` returns `a2`,
    /// `nth_ancestor(a4, 99)` returns `LIB`.
    fn nth_ancestor(&self, id: Id, n: usize) -> Branch<Id> {
        let m = n.checked_add(1).expect("security_param < usize::MAX");
        LineageIterator::new(id, &self.blocks)
            .take(m)
            .last()
            // SOUNDNESS: a lineage yields at least its start block
            // `take` requests n + 1 >= 1 elements, so `last()` always finds one.
            .unwrap()
            .branch
    }

    fn insert_mut(&mut self, block: Block<Id>) {
        self.blocks.insert_mut(block.branch.id(), block);
    }
}

/// Errors returned when applying a block to the tree.
#[derive(Debug, Clone, Error)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub enum Error<Id> {
    /// The block's parent is not in the tree.
    #[error("Parent block: {0:?} is not know to this node")]
    ParentMissing(Id),
    /// An orphan proof was not found in the ledger.
    #[error("Orphan proof has was not found in the ledger: {0:?}, can't import it")]
    OrphanMissing(Id),
    /// The block's slot precedes its parent's slot.
    #[error("Invalid slot for block {0:?}, parent slot is greater than child slot")]
    InvalidSlot(Id),
}

impl<Id> Cryptarchia<Id>
where
    Id: Eq + Hash + Copy + Debug,
{
    /// Creates an engine whose block tree contains only the given LIB block,
    /// which starts as the local chain tip.
    pub fn from_lib(id: Id, config: Config, state: State, slot: Slot, length: u64) -> Self {
        Self {
            branches: Branches::from_lib(id, slot, length),
            local_chain: Branch::new(id, id, slot, length),
            config,
            state,
        }
    }

    /// Apply the given block.
    ///
    /// On success, returns the pruned/reorged blocks resulting from the update.
    /// Re-receiving a block that is already in the tree is a no-op.
    /// On error, `self` is not modified.
    #[must_use = "Returns a new instance with the updated state, without modifying the original."]
    pub fn receive_block(
        &mut self,
        id: Id,
        parent: Id,
        slot: Slot,
    ) -> Result<(PrunedBlocks<Id>, ReorgedBlocks<Id>), Error<Id>> {
        let old_local_chain = self.local_chain;

        self.branches.apply_header(id, parent, slot)?;
        let new_local_chain = self.fork_choice();
        self.local_chain = new_local_chain;

        // Before `update_lib` which may prune blocks,
        // collect the reorged blocks in the old local chain.
        let reorged_blocks = if self.local_chain.id() == old_local_chain.id() {
            ReorgedBlocks::new()
        } else {
            // It's safer to compute LCA here, not in `fork_choice`,
            // because `fork_choice` may walk through multiple candidates
            // whose pairwise LCAs don't lie on `old_local_chain`'s parent chain.
            let lca = self.branches.lca(&old_local_chain, &new_local_chain);
            ReorgedBlocks(
                self.branches
                    .walk_back_to_block(old_local_chain.id(), lca.id())
                    .collect(),
            )
        };

        let pruned_blocks = self.update_lib();

        Ok((pruned_blocks, reorged_blocks))
    }

    /// Attempts to update the LIB.
    /// Whether the LIB is actually updated or not depends on the
    /// current state.
    ///
    /// If the LIB is updated, forks that diverged before the new LIB are
    /// pruned, and so are the new LIB's ancestors, which have become
    /// immutable; both prunes are returned as [`PrunedBlocks`].
    /// Otherwise, an empty [`PrunedBlocks`] is returned.
    fn update_lib(&mut self) -> PrunedBlocks<Id> {
        let new_lib = self.new_lib();
        // Trigger pruning only if the LIB has changed.
        if self.branches.lib == new_lib.id() {
            return PrunedBlocks::new();
        }

        // - `prune_stale_forks` must happen before `prune_immutable_blocks`
        // otherwise blocks would be left dangling
        // - `prune_immutable_blocks` must happen before committing to the new LIB
        // since the old LIB marks where to stop removal.
        //
        // Any other order of operations corrupts the state.
        typestate::LibUpdate::new(new_lib)
            .prune_stale_forks(self)
            .prune_immutable_blocks(self)
            .commit(self)
    }

    /// The block the LIB should point at under the current operating mode.
    fn new_lib(&self) -> Branch<Id> {
        let k = self
            .config
            .security_param()
            .get()
            .try_into()
            .expect("usize must be at least as wide as the security parameter");

        match self.state {
            State::Bootstrapping => self.get_unwrap(self.branches.lib).branch,
            State::Online => self.branches.nth_ancestor(self.local_chain.id(), k),
        }
    }

    /// Runs the fork choice rule and returns the selected new local chain tip.
    pub fn fork_choice(&self) -> Branch<Id> {
        match self.state {
            State::Bootstrapping => {
                let k = self.config.security_param().get().into();
                let s_gen = self.config.s_gen();
                maxvalid_bg(self.local_chain, &self.branches, k, s_gen)
            }
            State::Online => {
                let k = self.config.security_param().get().into();
                maxvalid_mc(self.local_chain, &self.branches, k)
            }
        }
    }

    /// The id of the local canonical chain's tip.
    pub const fn tip(&self) -> Id {
        self.local_chain.id()
    }

    /// The block at the local canonical chain's tip.
    pub const fn tip_branch(&self) -> &Branch<Id> {
        &self.local_chain
    }

    /// Removes the blocks of all forks that diverged before the new LIB and
    /// returns their ids. A fork is prunable if its LCA with the local chain
    /// is below the new LIB; forks diverging at or after the new LIB may
    /// still become canonical and are kept.
    ///
    /// # Example
    ///
    /// With `new_lib_length = 3` (the new LIB is `a3`).
    ///
    /// ```text
    ///      b1                  <- prunable: diverged below the new LIB
    ///     /
    /// G - a1 - a2 - a3 - a4    <- local chain, a3 = new LIB
    ///               \
    ///                c2        <- kept: diverged at the new LIB
    /// ```
    ///
    /// `b1` is pruned because its LCA `a1` lies below `a3`,
    /// while `c2` survives because its LCA is `a3` itself.
    ///
    /// ```text
    /// G - a1 - a2 - a3 - a4    <- local chain, a3 = new LIB
    ///               \
    ///                c2        <- kept: diverged at the new LIB
    /// ```
    fn prune_stale_forks(&mut self, new_lib_length: u64) -> HashSet<Id> {
        self.clone()
            .prunable_forks(new_lib_length)
            .flat_map(|tip| self.prune_fork(tip.id()))
            .collect()
    }

    /// Iterates over the fork tips whose divergence from the local chain
    /// lies strictly below the given LIB length.
    fn prunable_forks(&self, lib_length: u64) -> impl Iterator<Item = Branch<Id>> + '_ {
        self.non_canonical_forks().filter(move |non_canonical_tip| {
            let lca = self.branches.lca(&self.local_chain, non_canonical_tip);
            // non-canonical forks at or past LIB may still become canonical so none should be pruned.
            lca.length() < lib_length
        })
    }

    /// Iterates over the tips of all forks other than the local canonical
    /// chain, prunable or not.
    pub fn non_canonical_forks(&self) -> impl Iterator<Item = Branch<Id>> + '_ {
        let canonical_id = self.local_chain.id();
        self.branches
            .branches()
            .filter(move |tip| tip.id() != canonical_id)
    }

    /// Removes the blocks of the stale fork ending in `tip_id` and returns
    /// their ids. A block is removable if it is a tip or has only one child.
    ///
    /// # Example
    ///
    /// ```text
    ///              c1 - c2    <- stale tip C
    ///             /
    ///       b1 - b2 - b3      <- stale tip B
    ///      /
    /// G - a1 - a2 - a3 - a4   <- local chain
    /// ```
    ///
    /// First pass, `prune_fork(b3)`: only `b3` is removable, since `b2` has
    /// two children; `b2` is updated to have one child.
    ///
    /// ```text
    ///              c1 - c2    <- stale tip C
    ///             /
    ///       b1 - b2
    ///      /
    /// G - a1 - a2 - a3 - a4   <- local chain
    /// ```
    ///
    /// Second pass, `prune_fork(c2)`: every block from `c2` down to `b1`
    /// is removable, since `a1` has two children; `a1` is updated to have one
    /// child.
    ///
    /// ```text
    /// G - a1 - a2 - a3 - a4   <- local chain
    /// ```
    fn prune_fork(&mut self, tip: Id) -> HashSet<Id> {
        self.branches.tips.remove_mut(&tip);

        let is_removable = |block: &&Block<Id>| block.is_tip() || block.has_single_child();

        let snapshot = self.branches.blocks.clone();

        LineageIterator::new(tip, &snapshot)
            .take_while(is_removable)
            .with_is_last()
            .map(|(block, is_last)| {
                self.remove_mut(block.branch.id());
                if is_last {
                    let lca = self.get_unwrap(block.branch.parent());
                    self.insert_mut(lca.with_child_removed());
                }
                block.branch.id()
            })
            .collect()
    }

    fn get_unwrap(&self, id: Id) -> &Block<Id> {
        // SOUNDNESS: callers only pass ids obtained from the tree itself
        // (tips, parent links, the LIB pointer), which never dangle. A miss
        // means the tree is corrupt, and panicking is deliberate.
        self.branches.blocks.get(&id).unwrap()
    }

    fn remove_mut(&mut self, id: Id) {
        self.branches.blocks.remove_mut(&id);
    }

    fn insert_mut(&mut self, block: Block<Id>) {
        self.branches.insert_mut(block);
    }

    /// Removes every ancestor of the new LIB.
    ///
    /// # Example
    ///
    /// The LIB advances from `G` to `a3`:
    ///
    /// ```text
    /// G - a1 - a2 - a3 - a4 - a5   <- local chain
    /// ^ old LIB     ^ new LIB
    /// ```
    ///
    /// `prune_immutable_blocks()` removes `a2`, `a1` and `G`:
    ///
    /// ```text
    /// a3 - a4 - a5   <- local chain
    /// ^ LIB
    /// ```
    fn prune_immutable_blocks(&mut self, new_lib: Id) -> BTreeMap<Slot, Id> {
        let snapshot = self.branches.blocks.clone();
        LineageIterator::new(new_lib, &snapshot)
            .skip(1) // don't remove the new LIB
            .map(|block| {
                self.remove_mut(block.branch.id());
                (block.branch.slot(), block.branch.id())
            })
            .collect()
    }

    /// The underlying block tree.
    pub const fn branches(&self) -> &Branches<Id> {
        &self.branches
    }

    /// Get the latest immutable block (LIB) in the chain. No re-orgs past this
    /// point are allowed.
    pub const fn lib(&self) -> Id {
        self.branches.lib
    }

    /// The latest immutable block.
    pub fn lib_branch(&self) -> &Branch<Id> {
        &self.get_unwrap(self.lib()).branch
    }

    /// The engine's current operating mode.
    pub const fn state(&self) -> &State {
        &self.state
    }

    /// Switches the engine to `State::Online`, updating the LIB and
    /// pruning accordingly.
    pub fn online(mut self) -> (Self, PrunedBlocks<Id>) {
        self.state = State::Online;
        // With the state now Online, the LIB jumps from wherever bootstrapping
        // left it to the security-param-th ancestor of the local chain tip.
        let pruned_blocks = self.update_lib();
        (self, pruned_blocks)
    }

    /// The consensus configuration.
    pub const fn config(&self) -> &Config {
        &self.config
    }
}

/// Represents blocks that have been pruned because they are no longer needed
/// for future block validations.
pub struct PrunedBlocks<Id> {
    /// Blocks from the stale forks diverged before the LIB.
    stale_blocks: HashSet<Id>,
    /// Immutable blocks that were deeper than the LIB,
    /// excluding the LIB itself.
    immutable_blocks: BTreeMap<Slot, Id>,
}

impl<Id> Default for PrunedBlocks<Id> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Id> PrunedBlocks<Id> {
    /// Creates an empty instance of [`PrunedBlocks`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            stale_blocks: HashSet::new(),
            immutable_blocks: BTreeMap::new(),
        }
    }

    /// Whether no blocks were pruned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stale_blocks.is_empty() && self.immutable_blocks.is_empty()
    }

    /// The total number of pruned blocks, stale and immutable.
    #[must_use]
    pub fn len(&self) -> usize {
        // SOUNDNESS: no process can allocate more than usize::MAX bytes
        // thus existence of these collections at runtime is proof
        // that, even combined, they don't exceed usize::MAX elements.
        self.stale_blocks.len() + self.immutable_blocks.len()
    }

    /// Returns an iterator over all pruned blocks, both stale and immutable.
    pub fn all(&self) -> impl Iterator<Item = &Id> + '_ {
        self.stale_blocks
            .iter()
            .chain(self.immutable_blocks.values())
    }

    /// Returns an iterator over pruned stale blocks.
    pub fn stale_blocks(&self) -> impl Iterator<Item = &Id> + '_ {
        self.stale_blocks.iter()
    }

    /// Returns an iterator over pruned immutable blocks in slot order.
    #[must_use]
    pub const fn immutable_blocks(&self) -> &BTreeMap<Slot, Id> {
        &self.immutable_blocks
    }
}

impl<Id> PrunedBlocks<Id>
where
    Id: Eq + Hash + Copy,
{
    /// Extends the current instance with another [`PrunedBlocks`].
    pub fn extend(&mut self, other: &Self) {
        self.stale_blocks.extend(other.stale_blocks.iter());
        self.immutable_blocks.extend(other.immutable_blocks.iter());
    }
}

/// The blocks abandoned by a reorg: the segment of the old local chain from
/// its tip down to — excluding — the common ancestor with the new chain,
/// in tip-first order.
pub struct ReorgedBlocks<Id>(Vec<Id>);

impl<Id> ReorgedBlocks<Id> {
    #[must_use]
    const fn new() -> Self {
        Self(vec![])
    }

    /// The number of reorged blocks.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no blocks were reorged.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterates over the reorged block ids, old tip first.
    pub fn iter(&self) -> std::slice::Iter<'_, Id> {
        <&Self as IntoIterator>::into_iter(self)
    }
}

impl<'a, Id> IntoIterator for &'a ReorgedBlocks<Id> {
    type Item = &'a Id;
    type IntoIter = std::slice::Iter<'a, Id>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

#[cfg(test)]
pub mod tests {
    use std::{
        hash::{DefaultHasher, Hash, Hasher as _},
        num::NonZero,
    };

    use lb_utils::math::NonNegativeRatio;

    use std::collections::HashMap;

    use super::{Branch, Cryptarchia, Error, Slot, maxvalid_bg, maxvalid_mc};
    use crate::{Config, ReorgedBlocks, State, block::Role};

    #[must_use]
    pub fn config() -> Config {
        config_with(1)
    }

    #[must_use]
    pub fn config_with(security_param: u32) -> Config {
        Config::new(
            NonZero::new(security_param).unwrap(),
            NonNegativeRatio::new(1, 10.try_into().unwrap()),
            1f64.try_into().expect("1 > 0"),
        )
    }

    fn hash<T: Hash>(t: &T) -> [u8; 32] {
        let mut s = DefaultHasher::new();
        t.hash(&mut s);
        let hash = s.finish();
        let mut res = [0; 32];
        res[..8].copy_from_slice(&hash.to_le_bytes());
        res
    }

    /// Create a canonical chain with the `length` blocks and the provided `c`
    /// config.
    ///
    /// Blocks IDs for blocks other than the genesis are the hash of each block
    /// index, so for a chain of length 10, the sequence of block IDs will be
    /// `[0, hash(1), hash(2), ..., hash(9)]`.
    fn create_canonical_chain(length: NonZero<u64>, c: Option<Config>) -> Cryptarchia<[u8; 32]> {
        let mut engine = Cryptarchia::from_lib(
            hash(&0u64),
            c.unwrap_or_else(config),
            State::Bootstrapping,
            0.into(),
            0,
        );
        let mut parent = engine.lib();
        for i in 1..length.get() {
            let new_block = hash(&i);
            let (_, reorged_blocks) = engine
                .receive_block(new_block, parent, i.into())
                .expect("test block to be applied successfully.");
            assert!(
                reorged_blocks.is_empty(),
                "no reorgs should happen in a canonical chain"
            );
            parent = new_block;
        }
        engine
    }

    /// Asserts the structural invariants of the block tree that the engine
    /// maintains by hand:
    /// - every non-LIB node has a parent that is in the tree, and is never
    ///   its own parent (only the tree root is self-parented);
    /// - only the LIB has the `Lib` role, and it is in the tree;
    /// - the LIB is either self-parented (tree root since creation) or its
    ///   parent has been pruned;
    /// - each node's role matches its actual child count: `Tip` nodes have
    ///   none, `Internal` nodes have exactly `children_count`;
    /// - the `tips` set contains exactly the childless nodes;
    /// - the local chain tip is one of the tips.
    fn assert_tree_consistent(cryptarchia: &Cryptarchia<[u8; 32]>) {
        let nodes = &cryptarchia.branches.blocks;
        let lib = cryptarchia.branches.lib;
        assert!(nodes.contains_key(&lib), "the LIB must be in the tree");

        // Count each node's children from the parent links.
        let mut children_counts: HashMap<[u8; 32], usize> = HashMap::new();
        for (id, node) in nodes.iter() {
            if node.is_lib() {
                assert_eq!(*id, lib, "only the LIB may have the Lib role");
                let parent_id = node.branch.parent();
                assert!(
                    parent_id == *id || !nodes.contains_key(&parent_id),
                    "the LIB's ancestors must have been pruned"
                );
                continue;
            }
            let parent_id = node.branch.parent();
            assert_ne!(parent_id, *id, "only the tree root may be its own parent");
            assert!(
                nodes.contains_key(&parent_id),
                "a non-LIB node's parent must be in the tree"
            );
            *children_counts.entry(parent_id).or_default() += 1;
        }

        for (id, node) in nodes.iter() {
            let actual = children_counts.get(id).copied().unwrap_or_default();
            match node.role {
                // The LIB does not track its children.
                Role::Lib => {}
                Role::Internal { children_count } => assert_eq!(
                    children_count.get(),
                    actual,
                    "children_count must match the actual number of children"
                ),
                Role::Tip => assert_eq!(actual, 0, "a tip must have no children"),
            }
            assert_eq!(
                cryptarchia.branches.tips.contains(id),
                actual == 0,
                "the tips set must contain exactly the childless nodes"
            );
        }

        assert!(
            cryptarchia
                .branches
                .tips
                .contains(&cryptarchia.local_chain.id()),
            "the local chain tip must be one of the tips"
        );
        assert_eq!(
            cryptarchia.local_chain,
            cryptarchia.branches.blocks[&cryptarchia.local_chain.id()].branch,
            "the stored local chain tip must match its map entry"
        );
    }

    #[test]
    fn tree_invariants_through_forks_reorg_and_lib_advance() {
        // G - b1 - b2 - b3 (bootstrapping, k = 2)
        let mut engine = create_canonical_chain(4.try_into().unwrap(), Some(config_with(2)));
        assert_tree_consistent(&engine);

        // Hang two forks off b1 and extend the first, making b1 a fork
        // point with three children:
        //          f1a - f1b
        //         /
        // G - b1 - b2 - b3
        //         \
        //          f2a
        let b1 = hash(&1u64);
        let (f1a, f1b, f1c, f2a) = (hash(&"f1a"), hash(&"f1b"), hash(&"f1c"), hash(&"f2a"));
        engine.receive_block(f1a, b1, 2.into()).unwrap();
        assert_tree_consistent(&engine);
        engine.receive_block(f1b, f1a, 3.into()).unwrap();
        assert_tree_consistent(&engine);
        engine.receive_block(f2a, b1, 2.into()).unwrap();
        assert_tree_consistent(&engine);

        // Extend the f1 fork past the canonical chain to force a reorg.
        let (_, reorged) = engine.receive_block(f1c, f1b, 4.into()).unwrap();
        assert!(!reorged.is_empty(), "switching to the f1 fork must reorg");
        assert_eq!(engine.tip(), f1c);
        assert_tree_consistent(&engine);

        // Going online advances the LIB to k blocks behind the tip, pruning
        // the stale forks (b2-b3 and f2a) and the immutable blocks (G, b1).
        // Pruning f2a exercises the fork-point decrement; pruning b3
        // exercises the removal of a single-child interior block (b2).
        let (mut engine, pruned) = engine.online();
        assert!(!pruned.is_empty());
        assert_eq!(engine.lib(), f1a);
        assert_tree_consistent(&engine);

        // A fork branching at the LIB must survive pruning...
        let at_lib = hash(&"at_lib");
        engine.receive_block(at_lib, f1a, 3.into()).unwrap();
        assert_tree_consistent(&engine);

        // ...until the LIB advances past its divergence point.
        let m1 = hash(&"m1");
        engine.receive_block(m1, f1c, 5.into()).unwrap();
        assert_eq!(engine.lib(), f1b);
        assert!(
            engine.branches().get(&at_lib).is_none(),
            "a fork diverged before the new LIB must be pruned"
        );
        assert_tree_consistent(&engine);
    }

    #[test]
    fn lca_alignment() {
        //  g1
        // /
        // G - b1 - b2 - b3 - b4
        //       \
        //        f1
        let mut engine = create_canonical_chain(5.try_into().unwrap(), None);
        let (genesis, b1, b2, b4) = (hash(&0u64), hash(&1u64), hash(&2u64), hash(&4u64));
        let (f1, g1) = (hash(&"f1"), hash(&"g1"));
        engine.receive_block(f1, b1, 2.into()).unwrap();
        engine.receive_block(g1, genesis, 1.into()).unwrap();

        let branches = engine.branches();
        let get = |id| *branches.get(&id).unwrap();
        let (genesis, b1, b2, b4, f1, g1) =
            (get(genesis), get(b1), get(b2), get(b4), get(f1), get(g1));

        // One branch is an ancestor of the other.
        assert_eq!(branches.lca(&b4, &b1), b1);
        assert_eq!(branches.lca(&b1, &b4), b1);

        // Tips of different lengths diverging at b1.
        assert_eq!(branches.lca(&b4, &f1), b1);
        assert_eq!(branches.lca(&f1, &b4), b1);

        // Equal-length branches diverging at b1.
        assert_eq!(branches.lca(&f1, &b2), b1);

        // A branch is its own LCA.
        assert_eq!(branches.lca(&b2, &b2), b2);

        // Worst case: only the LIB (genesis) is in common.
        assert_eq!(branches.lca(&g1, &b4), genesis);
    }

    #[test]
    fn duplicate_block_is_ignored_without_mutation() {
        // G - b1 - b2 - b3
        let mut engine = create_canonical_chain(4.try_into().unwrap(), None);
        let snapshot = engine.clone();

        // Re-sending a block with its original parent and slot is a no-op...
        let (pruned, reorged) = engine
            .receive_block(hash(&2u64), hash(&1u64), 2.into())
            .expect("duplicate blocks are ignored");
        assert!(pruned.is_empty());
        assert!(reorged.is_empty());

        // ...but a duplicate goes through the same validations as a new
        // block first, so re-sending it with garbage metadata still errors.
        let result = engine.receive_block(hash(&2u64), hash(&99u64), 9.into());
        assert!(matches!(result, Err(Error::ParentMissing(_))));

        // The slot validation also runs before the duplicate check.
        let result = engine.receive_block(hash(&2u64), hash(&1u64), 0.into());
        assert!(matches!(result, Err(Error::InvalidSlot(_))));

        assert_eq!(engine, snapshot, "an ignored block must not mutate state");
    }

    #[test]
    fn online_with_only_the_genesis_block_keeps_the_lib() {
        let genesis = hash(&0u64);
        let engine =
            <Cryptarchia<_>>::from_lib(genesis, config(), State::Bootstrapping, 0.into(), 0);

        // The LIB clamps at the tree root when there are no ancestors.
        let (engine, pruned) = engine.online();
        assert_eq!(engine.lib(), genesis);
        assert_eq!(engine.tip(), genesis);
        assert!(pruned.is_empty());
        assert_tree_consistent(&engine);
    }

    #[test]
    fn same_slot_immutable_blocks_are_all_pruned() {
        // The engine accepts a block in the same slot as its parent; all
        // same-slot blocks must be removed when they become immutable, even
        // though the pruning report keys blocks by slot.
        // G - b1 - b2 - b3 - b4   (b1 and b2 share slot 1)
        let genesis = hash(&0u64);
        let mut engine =
            Cryptarchia::from_lib(genesis, config_with(1), State::Bootstrapping, 0.into(), 0);
        let (b1, b2, b3, b4) = (hash(&"b1"), hash(&"b2"), hash(&"b3"), hash(&"b4"));
        engine.receive_block(b1, genesis, 1.into()).unwrap();
        engine.receive_block(b2, b1, 1.into()).unwrap();
        engine.receive_block(b3, b2, 2.into()).unwrap();
        engine.receive_block(b4, b3, 3.into()).unwrap();

        // Going online advances the LIB to b3 (k = 1), pruning b2, b1 and G.
        let (engine, pruned) = engine.online();
        assert_eq!(engine.lib(), b3);

        // Both same-slot blocks are gone from the tree...
        assert!(engine.branches().get(&b1).is_none());
        assert!(engine.branches().get(&b2).is_none());
        assert!(engine.branches().get(&genesis).is_none());
        // ...while the report collapses them into a single slot-1 entry.
        assert_eq!(pruned.immutable_blocks.len(), 2);
        let reported = pruned.immutable_blocks.get(&1.into()).copied();
        assert!(reported == Some(b1) || reported == Some(b2));
        assert_tree_consistent(&engine);
    }

    #[test]
    fn reorg_and_lib_advance_in_a_single_receive_block() {
        //           f3 - f4    <- fork, wins on the last block
        //          /
        // G - b1 - b2 - b3     <- local chain, LIB = b2 once online
        let mut engine = create_canonical_chain(4.try_into().unwrap(), Some(config_with(1)));
        let (b1, b2, b3) = (hash(&1u64), hash(&2u64), hash(&3u64));
        let (f3, f4) = (hash(&"f3"), hash(&"f4"));
        engine.receive_block(f3, b2, 3.into()).unwrap();
        let (mut engine, pruned) = engine.online();
        assert_eq!(engine.lib(), b2);
        // The equal-length fork survives: it diverged at the LIB.
        assert!(pruned.stale_blocks.is_empty());
        assert!(engine.branches().get(&b1).is_none());

        // One block both reorgs the local chain to the fork and advances
        // the LIB; the reorged blocks are collected against the pre-prune
        // tree, so b3 is reported as reorged *and* as pruned.
        let (pruned, reorged) = engine.receive_block(f4, f3, 4.into()).unwrap();
        assert_eq!(engine.tip(), f4);
        assert_eq!(engine.lib(), f3);
        assert_eq!(reorged.len(), 1);
        assert_eq!(reorged.iter().next(), Some(&b3));
        assert_eq!(pruned.stale_blocks, [b3].into());
        assert_eq!(pruned.immutable_blocks, [(2.into(), b2)].into());
        assert_tree_consistent(&engine);
    }

    #[test]
    fn lib_parent_follows_the_tree_root_conventions() {
        // G - b1 - b2 - b3
        let engine = create_canonical_chain(4.try_into().unwrap(), Some(config_with(1)));

        // The tree root is its own parent while it is the LIB...
        let genesis = *engine.branches().get(&hash(&0u64)).unwrap();
        assert_eq!(genesis.parent(), genesis.id());

        // ...while an advanced LIB keeps the id of its real, pruned parent.
        let (engine, _) = engine.online();
        assert_eq!(engine.lib(), hash(&2u64));
        assert_eq!(engine.lib_branch().parent(), hash(&1u64));
        assert!(
            engine.branches().get(&hash(&1u64)).is_none(),
            "the LIB's parent must be pruned from memory"
        );
    }

    #[test]
    #[should_panic(expected = "lca() requires branches that are currently in the tree")]
    fn lca_rejects_branches_not_in_the_tree() {
        let engine = create_canonical_chain(3.try_into().unwrap(), None);
        let foreign =
            Cryptarchia::from_lib(hash(&99u64), config(), State::Bootstrapping, 0.into(), 0);
        let foreign_branch = *foreign.branches().get(&hash(&99u64)).unwrap();
        let local = *engine.branches().get(&hash(&1u64)).unwrap();

        let _ = engine.branches().lca(&local, &foreign_branch);
    }

    #[test]
    fn online_fork_choice_rejects_reorgs_deeper_than_k() {
        //          f2 - f3 - f4 - f5 - f6    <- longer fork, diverged at b1
        //         /
        // G - b1 - b2 - b3 - b4 - b5         <- local chain
        //
        // The engine's pruning keeps every surviving fork within k of the
        // tip while Online, so the rejection arm of `maxvalid_mc` is tested
        // directly on a bootstrapped tree (as `test_fork_choice` does for
        // `maxvalid_bg`).
        let mut engine = create_canonical_chain(6.try_into().unwrap(), Some(config_with(2)));
        let mut parent = hash(&1u64);
        for i in 2..=6u64 {
            let block = hash(&format!("f{i}"));
            engine.receive_block(block, parent, i.into()).unwrap();
            parent = block;
        }

        let branches = engine.branches();
        let canonical_tip = *branches.get(&hash(&5u64)).unwrap();
        let fork_tip = *branches.get(&hash(&format!("f{}", 6u64))).unwrap();
        assert_eq!(fork_tip.length(), canonical_tip.length() + 1);

        // The fork diverged 4 blocks behind the canonical tip: with k = 2
        // the longer fork must not win...
        assert_eq!(maxvalid_mc(canonical_tip, branches, 2), canonical_tip);
        // ...while with k = 4 the divergence is within bounds and it must.
        assert_eq!(maxvalid_mc(canonical_tip, branches, 4), fork_tip);
    }

    #[test]
    fn reorged_blocks_are_reported_from_old_tip_to_lca() {
        //      f2 - f3 - f4    <- new local chain after the reorg
        //     /
        // G - b1 - b2 - b3     <- old local chain
        //
        // k is large so the longest-chain rule (not the density rule)
        // decides the fork choice.
        let mut engine = create_canonical_chain(4.try_into().unwrap(), Some(config_with(10)));
        let (f2, f3, f4) = (hash(&"f2"), hash(&"f3"), hash(&"f4"));
        engine.receive_block(f2, hash(&1u64), 2.into()).unwrap();
        engine.receive_block(f3, f2, 3.into()).unwrap();

        let (_, reorged) = engine.receive_block(f4, f3, 4.into()).unwrap();
        assert_eq!(engine.tip(), f4);
        assert_eq!(
            reorged.iter().copied().collect::<Vec<_>>(),
            vec![hash(&3u64), hash(&2u64)],
            "reorged blocks are the old chain from its tip down to the LCA \
             (exclusive), in child-to-parent order"
        );
    }

    #[test]
    fn equal_length_fork_does_not_displace_the_tip() {
        //      f2 - f3        <- competing fork of equal length
        //     /
        // G - b1 - b2 - b3    <- local chain (seen first, must stay)
        let mut engine = create_canonical_chain(4.try_into().unwrap(), Some(config_with(10)));
        let (f2, f3) = (hash(&"f2"), hash(&"f3"));
        engine.receive_block(f2, hash(&1u64), 2.into()).unwrap();
        let (_, reorged) = engine.receive_block(f3, f2, 3.into()).unwrap();

        assert!(reorged.is_empty());
        assert_eq!(
            engine.tip(),
            hash(&3u64),
            "the first-seen chain wins length ties"
        );
    }

    #[test]
    fn branch_serde_format() {
        // G - b1
        let engine = create_canonical_chain(2.try_into().unwrap(), None);
        let branches = engine.branches();
        let genesis = *branches.get(&hash(&0u64)).unwrap();
        let b1 = *branches.get(&hash(&1u64)).unwrap();

        // The tree root is encoded as its own parent (see `Branch::parent`)...
        let genesis_json = serde_json::to_value(genesis).unwrap();
        assert_eq!(
            genesis_json["parent"],
            serde_json::to_value(genesis.id()).unwrap()
        );
        // ...while other blocks serialize their parent id.
        let b1_json = serde_json::to_value(b1).unwrap();
        assert_eq!(
            b1_json["parent"],
            serde_json::to_value(genesis.id()).unwrap()
        );

        // Both round-trip to equal values.
        for (json, branch) in [(genesis_json, genesis), (b1_json, b1)] {
            let roundtripped: Branch<[u8; 32]> = serde_json::from_value(json).unwrap();
            assert_eq!(roundtripped, branch);
        }
    }

    #[test]
    fn test_slot_increasing() {
        // parent
        // └── child

        let mut branches = super::Branches::from_lib(hash(&0u64), 0.into(), 0);
        let parent = hash(&1u64);
        let child = hash(&2u64);

        branches
            .apply_header(parent, hash(&0u64), 2.into())
            .unwrap();
        assert!(matches!(
            branches.apply_header(child, parent, 1.into()),
            Err(Error::InvalidSlot(_))
        ));
    }

    #[test]
    fn test_immutable_fork() {
        // b0(LIB) - b1 - b2
        let cryptarchia = create_canonical_chain(3.try_into().unwrap(), Some(config_with(1)));

        // Switch to Online to update LIB and trigger pruning.
        // b1(LIB) - b2
        let (mut cryptarchia, pruned_blocks) = cryptarchia.online();
        assert_eq!(cryptarchia.lib(), hash(&1u64));
        assert_eq!(
            pruned_blocks.immutable_blocks,
            [(0.into(), hash(&0u64))].into(),
        );

        // Try to add a fork from b0, but it should fail with `Error::MissingParent`.
        //   pruned
        //   ||
        // (b0 --) b1(LIB) - b2
        //     \
        //      b3
        assert!(matches!(
            cryptarchia.receive_block(hash(&3u64), hash(&0u64), 1.into()),
            Err(Error::ParentMissing(_)),
        ));
    }

    #[test]
    fn test_fork_choice() {
        // by setting a low k we trigger the density choice rule, and the shorter chain
        // is denser after the fork
        let config = config_with(10);
        let s_gen = config.s_gen().get();
        let initial_height = 49;
        let orig_engine =
            create_canonical_chain((initial_height + 1).try_into().unwrap(), Some(config));

        let mut engine = orig_engine.clone();
        let mut long_p = engine.tip();
        let mut short_p = engine.tip();
        // the node sees first the short chain.
        for slot in initial_height..(initial_height + s_gen) {
            // build chain not too dense because we'll build a denser chain later
            if slot % 2 == 0 {
                let new_block = hash(&format!("short-{slot}"));
                let (_, reorged_blocks) = engine
                    .receive_block(new_block, short_p, slot.into())
                    .unwrap();
                assert!(reorged_blocks.is_empty());
                short_p = new_block;
            }
        }
        assert_eq!(engine.tip(), short_p);

        // then it receives a longer chain which is however less dense after the fork
        for slot in initial_height..(initial_height + s_gen) {
            if slot % 3 == 0 {
                let new_block = hash(&format!("long-{slot}"));
                let (_, reorged_blocks) = engine
                    .receive_block(new_block, long_p, slot.into())
                    .unwrap();
                assert!(reorged_blocks.is_empty());
                long_p = new_block;
            }
            assert_eq!(engine.tip(), short_p);
        }
        // even if the long chain is much longer, it will never be accepted as it's not
        // dense enough
        for slot in (initial_height + s_gen)..(initial_height + 2 * s_gen) {
            let new_block = hash(&format!("long-{slot}"));
            let (_, reorged_blocks) = engine
                .receive_block(new_block, long_p, slot.into())
                .unwrap();
            assert!(reorged_blocks.is_empty());
            long_p = new_block;
            assert_eq!(engine.tip(), short_p);
        }

        {
            let bs = engine.branches();
            let long_branch = bs.branches().find(|b| b.id() == long_p).unwrap();
            let short_branch = bs.branches().find(|b| b.id() == short_p).unwrap();

            // however, if we set k to the fork length, it will be accepted
            let k = long_branch.length();
            assert_eq!(
                maxvalid_bg(short_branch, engine.branches(), k, engine.config.s_gen()).id(),
                long_p
            );

            // a new denser chain will be selected as the main tip
            let mut parent = orig_engine.tip();
            let tip_height = engine.tip_branch().length();
            for slot in initial_height..=tip_height {
                let new_block = hash(&format!("dense-{slot}"));
                let (_, reorged_blocks) = engine
                    .receive_block(new_block, parent, slot.into())
                    .unwrap();

                if slot < tip_height {
                    assert!(reorged_blocks.is_empty());
                } else {
                    // on the last block we trigger the reorg
                    let expected_reorg_len = tip_height - initial_height;
                    assert_reorged_blocks(
                        &reorged_blocks,
                        &orig_engine.tip(),
                        &short_p,
                        expected_reorg_len as usize,
                        &engine,
                    );
                }
                parent = new_block;
            }
            assert_eq!(engine.tip(), parent);
        }
    }

    /// Check that reorged blocks are as below:
    /// origin - [... - tip]
    ///          \_________/
    ///         reorged blocks
    fn assert_reorged_blocks<Id: std::fmt::Debug + Eq + Hash + Copy>(
        blocks: &ReorgedBlocks<Id>,
        origin_excluded: &Id,
        tip: &Id,
        length: usize,
        cryptarchia: &Cryptarchia<Id>,
    ) {
        assert_eq!(blocks.iter().next().unwrap(), tip);
        assert_eq!(blocks.len(), length);
        blocks
            .iter()
            .rev()
            .fold(origin_excluded, |expected_parent, id| {
                assert_eq!(
                    &cryptarchia.branches().get(id).unwrap().parent(),
                    expected_parent
                );
                id
            });
    }

    #[test]
    fn test_getters() {
        let engine =
            <Cryptarchia<_>>::from_lib(hash(&0u64), config(), State::Bootstrapping, 0.into(), 0);
        let id_0 = engine.lib();

        // Get branch directly from HashMap
        let branch1 = engine.branches.get(&id_0).expect("branch1 should be there");

        let branches = engine.branches();

        // Get branch using getter
        let branch2 = branches.get(&id_0).expect("branch2 should be there");

        assert_eq!(branch1, branch2);
        assert_eq!(branch1.id(), branch2.id());
        assert_eq!(branch1.parent(), branch2.parent());
        assert_eq!(branch1.slot(), branch2.slot());
        assert_eq!(branch1.length(), branch2.length());

        let slot = Slot::genesis();

        assert_eq!(slot.strict_add(10.into()), Slot::from(10));

        let id_100 = hash(&100u64);

        assert!(
            branches.get(&id_100).is_none(),
            "id_100 should not be related to this branch"
        );
    }

    // It tests that nothing is pruned when the pruning depth is greater than the
    // canonical chain length.
    #[test]
    fn pruning_too_back_in_time() {
        // Create a chain with 50+1 blocks with k=50.
        // b0(LIB) - b1 - ... - b49
        //         \
        //          b100
        let mut cryptarchia = create_canonical_chain(50.try_into().unwrap(), Some(config_with(50)));
        // Add a fork from genesis block
        let (pruned_blocks, _) = cryptarchia
            .receive_block(hash(&100u64), hash(&0u64), 1.into())
            .expect("test block to be applied successfully.");
        // No block was pruned during Boostrapping.
        assert!(pruned_blocks.all().next().is_none());

        // Switch to Online to update LIB and trigger pruning.
        // b0(LIB) - b1 - ... - b49
        //         \
        //           b100
        let (mut cryptarchia, pruned_blocks) = cryptarchia.online();
        assert_eq!(cryptarchia.lib(), hash(&0u64));

        // But, no block was pruned because `security_param` is
        // greater than local chain length.
        assert!(pruned_blocks.all().next().is_none());
        assert!(cryptarchia.branches.tips.contains(&hash(&100u64)));
        assert!(cryptarchia.branches.blocks.contains_key(&hash(&100u64)));

        // Add two new blocks to the local honest chain,
        // and check if the LIB is updated and blocks are pruned.
        let (pruned_blocks, _) = cryptarchia
            .receive_block(hash(&50u64), hash(&49u64), 50.into())
            .expect("test block to be applied successfully.");
        assert!(pruned_blocks.is_empty());
        let (pruned_blocks, _) = cryptarchia
            .receive_block(hash(&51u64), hash(&50u64), 51.into())
            .expect("test block to be applied successfully.");
        // The LIB was updated to b1.
        assert_eq!(cryptarchia.lib(), hash(&1u64));
        // The stale fork b100 was pruned.
        assert_eq!(pruned_blocks.stale_blocks, [hash(&100u64)].into());
        assert!(!cryptarchia.branches.tips.contains(&hash(&100u64)));
        assert!(!cryptarchia.branches.blocks.contains_key(&hash(&100u64)));
        // The immutable block b0 was pruned.
        assert_eq!(
            pruned_blocks.immutable_blocks,
            [(0.into(), hash(&0u64))].into()
        );
        assert!(!cryptarchia.branches.tips.contains(&hash(&0u64)));
        assert!(!cryptarchia.branches.blocks.contains_key(&hash(&0u64)));
    }

    #[test]
    fn pruning_with_no_stale_fork() {
        // Create a chain with 50 blocks with k=10.
        // b0(LIB) - b1 - ... b39 - b40 - ... - b49
        //                              \
        //                               b100
        let mut cryptarchia = create_canonical_chain(50.try_into().unwrap(), Some(config_with(10)));
        let (pruned_blocks, _) = cryptarchia
            .receive_block(hash(&100u64), hash(&40u64), 41.into())
            .expect("test block to be applied successfully.");
        // No block was pruned during Boostrapping.
        assert!(pruned_blocks.all().next().is_none());

        // Switch to Online to update LIB and trigger pruning.
        // b0 - b1 - ... b39(LIB) - b40 - ... - b49
        //                              \
        //                               b100
        let (cryptarchia, pruned_blocks) = cryptarchia.online();
        assert_eq!(cryptarchia.lib(), hash(&39u64));

        // But, b100 was not pruned.
        assert!(pruned_blocks.stale_blocks.is_empty());
        assert!(cryptarchia.branches.tips.contains(&hash(&100u64)));
        assert!(cryptarchia.branches.blocks.contains_key(&hash(&100u64)));

        // Immutable blocks (excluding LIB) were pruned.
        assert_eq!(
            pruned_blocks.immutable_blocks,
            (0..=38u64).rev().map(|i| (i.into(), hash(&i))).collect()
        );
    }

    #[test]
    fn pruning_with_no_forks() {
        // Create an Online chain with 50 blocks with k=1.
        // b0 - b1 - ... - b48(LIB) - b49
        let (cryptarchia, pruned_blocks) =
            create_canonical_chain(50.try_into().unwrap(), Some(config_with(1))).online();
        assert_eq!(cryptarchia.lib(), hash(&48u64));

        // There were no stale forks.
        assert!(pruned_blocks.stale_blocks.is_empty());

        // Immutable blocks (excluding LIB) were pruned.
        assert_eq!(
            pruned_blocks.immutable_blocks,
            (0..=47u64).rev().map(|i| (i.into(), hash(&i))).collect()
        );
    }

    #[test]
    fn pruning_with_single_stale_fork() {
        // Create a chain with 50+3 blocks with k=10.
        // b0(LIB) - b1 - ... - b38 - b39 - b40 - ... - b49
        //                          \     \     \
        //                           b100  b101  b102

        let mut cryptarchia = create_canonical_chain(50.try_into().unwrap(), Some(config_with(10)));
        cryptarchia
            .receive_block(hash(&100u64), hash(&38u64), 39.into())
            .expect("test block to be applied successfully.");
        cryptarchia
            .receive_block(hash(&101u64), hash(&39u64), 40.into())
            .expect("test block to be applied successfully.");
        let (pruned_blocks, _) = cryptarchia
            .receive_block(hash(&102u64), hash(&40u64), 41.into())
            .expect("test block to be applied successfully.");
        // No block was pruned during Boostrapping.
        assert!(pruned_blocks.all().next().is_none());

        // Switch to Online to update LIB and trigger pruning.
        // b0 - b1 - ... - b38 - b39(LIB) - b40 - ... - b49
        //                     \          \     \
        //                      b100       b101  b102
        let (cryptarchia, pruned_blocks) = cryptarchia.online();
        assert_eq!(cryptarchia.lib(), hash(&39u64));

        // A fork from b38 was pruned.
        assert_eq!(pruned_blocks.stale_blocks, [hash(&100u64)].into());
        assert!(!cryptarchia.branches.tips.contains(&hash(&100u64)));
        assert!(!cryptarchia.branches.blocks.contains_key(&hash(&100u64)));

        // Other forks were not pruned
        assert!(cryptarchia.branches.tips.contains(&hash(&101u64)));
        assert!(cryptarchia.branches.blocks.contains_key(&hash(&101u64)));
        assert!(cryptarchia.branches.tips.contains(&hash(&102u64)));
        assert!(cryptarchia.branches.blocks.contains_key(&hash(&102u64)));

        // Immutable blocks (excluding LIB) were pruned.
        assert_eq!(
            pruned_blocks.immutable_blocks,
            (0..=38u64).rev().map(|i| (i.into(), hash(&i))).collect()
        );
    }

    #[test]
    fn pruning_with_multiple_stale_forks() {
        // Create a chain with 50+3 blocks with k=10.
        //                          b200
        //                          /
        // b0(LIB) - b1 - ... - b38 - b39 - b40 - ... - b49
        //                          \     \
        //                           b100  b101
        let mut cryptarchia = create_canonical_chain(50.try_into().unwrap(), Some(config_with(10)));
        cryptarchia
            .receive_block(hash(&100u64), hash(&38u64), 39.into())
            .expect("test block to be applied successfully.");
        cryptarchia
            .receive_block(hash(&200u64), hash(&38u64), 39.into())
            .expect("test block to be applied successfully.");
        let (pruned_blocks, _) = cryptarchia
            .receive_block(hash(&101u64), hash(&39u64), 40.into())
            .expect("test block to be applied successfully.");
        // No block was pruned during Boostrapping.
        assert!(pruned_blocks.all().next().is_none());

        // Switch to Online to update LIB and trigger pruning.
        //                      b200
        //                     /
        // b0 - b1 - ... - b38 - b39(LIB) - b40 - ... - b49
        //                     \          \
        //                      b100       b101
        let (cryptarchia, pruned_blocks) = cryptarchia.online();
        assert_eq!(cryptarchia.lib(), hash(&39u64));

        // Two forks (b100 and b200) from b38 were pruned.
        assert_eq!(
            pruned_blocks.stale_blocks,
            [hash(&100u64), hash(&200u64)].into()
        );
        assert!(!cryptarchia.branches.tips.contains(&hash(&100u64)));
        assert!(!cryptarchia.branches.blocks.contains_key(&hash(&100u64)));
        assert!(!cryptarchia.branches.tips.contains(&hash(&200u64)));
        assert!(!cryptarchia.branches.blocks.contains_key(&hash(&200u64)));

        // Fork at b39 was not pruned.
        assert!(cryptarchia.branches.tips.contains(&hash(&101u64)));
        assert!(cryptarchia.branches.blocks.contains_key(&hash(&101u64)));

        // Immutable blocks (excluding LIB) were pruned.
        assert_eq!(
            pruned_blocks.immutable_blocks,
            (0..=38u64).rev().map(|i| (i.into(), hash(&i))).collect()
        );
    }

    #[test]
    fn pruning_stale_fork_with_multiple_tips() {
        // Create a chain with 50+3 blocks with k=10.
        // b0(LIB) - b1 - ... - b38 - b39 - ... - b49
        //                          \
        //                           b100 - b101
        //                                \
        //                                  b200
        let mut cryptarchia = create_canonical_chain(50.try_into().unwrap(), Some(config_with(10)));
        cryptarchia
            .receive_block(hash(&100u64), hash(&38u64), 39.into())
            .expect("test block to be applied successfully.");
        cryptarchia
            .receive_block(hash(&101u64), hash(&100u64), 40.into())
            .expect("test block to be applied successfully.");
        let (pruned_blocks, _) = cryptarchia
            .receive_block(hash(&200u64), hash(&100u64), 41.into())
            .expect("test block to be applied successfully.");
        // No block was pruned during Boostrapping.
        assert!(pruned_blocks.all().next().is_none());

        // Switch to Online to update LIB and trigger pruning.
        // b0 - b1 - ... - b38 - b39(LIB) - ... - b49
        //                     \
        //                      b100 - b101
        //                           \
        //                             b200
        let (cryptarchia, pruned_blocks) = cryptarchia.online();
        assert_eq!(cryptarchia.lib(), hash(&39u64));

        // All the stale forks (b100, b101 and b200) were pruned.
        assert_eq!(
            pruned_blocks.stale_blocks,
            [hash(&100u64), hash(&101u64), hash(&200u64)].into()
        );
        assert!(!cryptarchia.branches.tips.contains(&hash(&101u64)));
        assert!(!cryptarchia.branches.blocks.contains_key(&hash(&100u64)));
        assert!(!cryptarchia.branches.blocks.contains_key(&hash(&101u64)));
        assert!(!cryptarchia.branches.tips.contains(&hash(&200u64)));
        assert!(!cryptarchia.branches.blocks.contains_key(&hash(&200u64)));

        // Immutable blocks (excluding LIB) were pruned.
        assert_eq!(
            pruned_blocks.immutable_blocks,
            (0..=38u64).rev().map(|i| (i.into(), hash(&i))).collect()
        );
    }

    #[test]
    fn pruning_forks_when_receive_block() {
        // Create an Online chain with 10 blocks with k=2.
        // b0 - b1 - ... - b7(LIB) - b8 - b9
        let (mut cryptarchia, pruned_blocks) =
            create_canonical_chain(10.try_into().unwrap(), Some(config_with(2))).online();
        assert_eq!(cryptarchia.lib(), hash(&7u64));
        // There were no stale forks
        assert!(pruned_blocks.stale_blocks.is_empty());
        // Immutable blocks (excluding LIB) were pruned.
        assert_eq!(
            pruned_blocks.immutable_blocks,
            (0..=6u64).rev().map(|i| (i.into(), hash(&i))).collect()
        );

        // Add a fork at the LIB
        // b7(LIB) - b8 - b9
        //         \
        //          b100
        let (pruned_blocks, _) = cryptarchia
            .receive_block(
                hash(&100u64),
                cryptarchia.lib(),
                cryptarchia.lib_branch().slot().strict_add(1.into()),
            )
            .expect("test block to be applied successfully.");
        assert_eq!(cryptarchia.lib(), hash(&7u64));
        // No block is pruned since LIB was not updated.
        assert!(pruned_blocks.all().next().is_none());
        assert!(cryptarchia.branches.tips.contains(&hash(&100u64)));
        assert!(cryptarchia.branches.blocks.contains_key(&hash(&100u64)));

        // Add a fork after than LIB
        // b7(LIB) - b8 - b9
        //         \    \
        //          b100 b101
        let (pruned_blocks, _) = cryptarchia
            .receive_block(
                hash(&101u64),
                cryptarchia.tip_branch().parent(),
                cryptarchia.tip_branch().slot(),
            )
            .expect("test block to be applied successfully.");
        assert_eq!(cryptarchia.lib(), hash(&7u64));
        // No block was pruned since LIB was not updated.
        assert!(pruned_blocks.all().next().is_none());
        assert!(cryptarchia.branches.tips.contains(&hash(&100u64)));
        assert!(cryptarchia.branches.blocks.contains_key(&hash(&100u64)));
        assert!(cryptarchia.branches.tips.contains(&hash(&101u64)));
        assert!(cryptarchia.branches.blocks.contains_key(&hash(&101u64)));

        // Add a block to the tip to update the LIB.
        // b7 - b8(LIB) - b9 - b102
        //    \         \
        //     b100      b101
        let (pruned_blocks, _) = cryptarchia
            .receive_block(
                hash(&102u64),
                cryptarchia.tip(),
                cryptarchia.tip_branch().slot().strict_add(1.into()),
            )
            .expect("test block to be applied successfully.");
        assert_eq!(cryptarchia.lib(), hash(&8u64));
        // One fork (b100) was pruned since LIB was updated.
        assert_eq!(pruned_blocks.stale_blocks, [hash(&100u64)].into());
        assert!(!cryptarchia.branches.tips.contains(&hash(&100u64)));
        assert!(!cryptarchia.branches.blocks.contains_key(&hash(&100u64)));
        // b101 and b102 were not pruned.
        assert!(cryptarchia.branches.tips.contains(&hash(&101u64)));
        assert!(cryptarchia.branches.blocks.contains_key(&hash(&101u64)));
        assert!(cryptarchia.branches.tips.contains(&hash(&102u64)));
        assert!(cryptarchia.branches.blocks.contains_key(&hash(&102u64)));
        // Immutable blocks (excluding LIB) were pruned.
        assert_eq!(
            pruned_blocks.immutable_blocks,
            [(7.into(), hash(&7u64))].into(),
        );
    }
}
