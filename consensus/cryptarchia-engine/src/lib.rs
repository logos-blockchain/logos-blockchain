pub mod config;
pub mod time;

mod fixtures;

use core::{fmt::Debug, hash::Hash};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    num::NonZero,
};

pub use config::*;
use lb_utils::bounded::UpperBoundedVec;
use rpds::{HashTrieMapSync, HashTrieSetSync};
use thiserror::Error;
pub use time::{Epoch, EpochConfig, Slot};

pub(crate) const LOG_TARGET: &str = "cryptarchia::engine";

/// Slots occupied by the uncles a block references.
pub type UncleSlots = UpperBoundedVec<Slot, MAX_UNCLES>;

#[derive(Clone, Debug, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum State {
    Bootstrapping,
    Online,
}

impl State {
    #[must_use]
    pub const fn is_bootstrapping(&self) -> bool {
        matches!(self, Self::Bootstrapping)
    }

    #[must_use]
    pub const fn is_online(&self) -> bool {
        matches!(self, Self::Online)
    }

    /// Runs the fork choice rule and returns the selected new local chain tip.
    fn fork_choice<Id>(cryptarchia: &Cryptarchia<Id>) -> &Branch<Id>
    where
        Id: Eq + Hash + Copy,
    {
        match cryptarchia.state {
            Self::Bootstrapping => {
                let k = cryptarchia.config.security_param().get().into();
                let s_gen = cryptarchia.config.s_gen();
                maxvalid_bg(&cryptarchia.local_chain, &cryptarchia.branches, k, s_gen)
            }
            Self::Online => {
                let k = cryptarchia.config.security_param().get().into();
                maxvalid_mc(&cryptarchia.local_chain, &cryptarchia.branches, k)
            }
        }
    }

    fn lib<Id>(cryptarchia: &Cryptarchia<Id>) -> Id
    where
        Id: Eq + Hash + Copy,
    {
        match cryptarchia.state {
            Self::Bootstrapping => cryptarchia.branches.lib,
            Self::Online => cryptarchia
                .branches
                .nth_ancestor(
                    &cryptarchia.local_chain,
                    cryptarchia.config.security_param().get().into(),
                )
                .id(),
        }
    }
}

/// Implementation of the fork choice rule as defined in the Ouroboros Genesis
/// paper k defines the forking depth of chain we accept without more
/// analysis s defines the length of time (unit of slots) after the fork
/// happened we will inspect for chain density
fn maxvalid_bg<'b, Id>(
    local_chain: &'b Branch<Id>,
    branches: &'b Branches<Id>,
    k: u64,
    s_gen: NonZero<u64>,
) -> &'b Branch<Id>
where
    Id: Eq + Hash + Copy,
{
    let mut cmax = local_chain;

    let forks = branches.branches();
    for chain in forks {
        let lowest_common_ancestor = branches
            .lca(cmax, chain)
            .expect("local chain and fork must have a common ancestor");
        let m = cmax.length - lowest_common_ancestor.length;
        if m <= k {
            // Classic longest chain rule with parameter k
            if cmax.length < chain.length {
                cmax = chain;
            }
        } else {
            // The chain is forking too much, we need to pay a bit more attention
            // In particular, select the chain that is the densest after the fork
            let density_slot = Slot::from(u64::from(lowest_common_ancestor.slot) + s_gen.get());
            let cmax_density = branches.walk_back_before(cmax, density_slot).length;
            let candidate_density = branches.walk_back_before(chain, density_slot).length;
            if cmax_density < candidate_density {
                cmax = chain;
            }
        }
    }
    cmax
}

/// Implementation of the fork choice rule as defined in the Ouroboros Praos
/// paper k defines the forking depth of chain we can accept.
fn maxvalid_mc<'b, Id>(
    local_chain: &'b Branch<Id>,
    branches: &'b Branches<Id>,
    k: u64,
) -> &'b Branch<Id>
where
    Id: Eq + Hash + Copy,
{
    let mut cmax = local_chain;

    let forks = branches.branches();
    for chain in forks {
        let lowest_common_ancestor = branches
            .lca(cmax, chain)
            .expect("local chain and fork must have a common ancestor");
        let m = cmax.length - lowest_common_ancestor.length;
        if m <= k && cmax.length < chain.length {
            // Classic longest chain rule with parameter k
            cmax = chain;
        }
    }
    cmax
}

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

#[derive(Clone, Debug)]
pub struct Branches<Id>
where
    Id: Eq + Hash,
{
    branches: HashTrieMapSync<Id, Branch<Id>>,
    tips: HashTrieSetSync<Id>,
    lib: Id,
}

impl<Id> PartialEq for Branches<Id>
where
    Id: Eq + Hash,
{
    fn eq(&self, other: &Self) -> bool {
        self.branches == other.branches && self.tips == other.tips && self.lib == other.lib
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Branch<Id> {
    id: Id,
    parent: Id,
    slot: Slot,
    // chain length
    length: u64,
    /// Slots of the uncles this block references, for the uncle selection of
    /// the blocks extending it.
    uncle_slots: UncleSlots,
}

impl<Id: Copy> Branch<Id> {
    pub const fn id(&self) -> Id {
        self.id
    }
    pub const fn parent(&self) -> Id {
        self.parent
    }
    pub const fn slot(&self) -> Slot {
        self.slot
    }
    pub const fn length(&self) -> u64 {
        self.length
    }
    pub const fn uncle_slots(&self) -> &UncleSlots {
        &self.uncle_slots
    }
}

impl<Id> Branches<Id>
where
    Id: Eq + Hash + Copy,
{
    pub fn from_lib(lib: Id, slot: Slot, length: u64, uncle_slots: UncleSlots) -> Self {
        let mut branches = HashTrieMapSync::new_sync();
        branches.insert_mut(
            lib,
            Branch {
                id: lib,
                parent: lib,
                slot,
                length,
                uncle_slots,
            },
        );
        let mut tips = HashTrieSetSync::new_sync();
        tips.insert_mut(lib);
        Self {
            branches,
            tips,
            lib,
        }
    }

    /// Apply a new header to the branches.
    ///
    /// On error, `self` is not modified.
    #[must_use = "this returns the result of the operation, without modifying the original"]
    fn apply_header(
        &mut self,
        header: Id,
        parent: Id,
        slot: Slot,
        uncle_slots: UncleSlots,
    ) -> Result<(), Error<Id>> {
        let parent_branch = self
            .branches
            .get(&parent)
            .ok_or(Error::ParentMissing(parent))?;

        if parent_branch.slot > slot {
            return Err(Error::InvalidSlot(parent));
        }

        let length = parent_branch
            .length
            .checked_add(1)
            .expect("New branch height overflows.");

        self.tips.remove_mut(&parent);
        self.tips.insert_mut(header);

        self.branches.insert_mut(
            header,
            Branch {
                id: header,
                parent,
                length,
                slot,
                uncle_slots,
            },
        );

        Ok(())
    }

    pub fn branches(&self) -> impl Iterator<Item = &Branch<Id>> + '_ {
        self.tips.iter().map(|id| &self.branches[id])
    }

    /// find the lowest common ancestor of two branches
    ///
    /// `None` if the two branches have no common ancestor in this tree.
    pub fn lca<'a>(
        &'a self,
        mut b1: &'a Branch<Id>,
        mut b2: &'a Branch<Id>,
    ) -> Option<&'a Branch<Id>> {
        // first reduce branches to the same length
        while b1.length > b2.length {
            b1 = self.parent(b1)?;
        }

        while b2.length > b1.length {
            b2 = self.parent(b2)?;
        }

        // then walk up the chain until we find the common ancestor
        while b1.id != b2.id {
            b1 = self.parent(b1)?;
            b2 = self.parent(b2)?;
        }

        Some(b1)
    }

    pub fn get(&self, id: &Id) -> Option<&Branch<Id>> {
        self.branches.get(id)
    }

    /// The parent of `branch`, or `None` if `branch` is the oldest block in the
    /// tree, whose parent is either itself (genesis) or outside the tree
    /// (pruned).
    fn parent<'a>(&'a self, branch: &Branch<Id>) -> Option<&'a Branch<Id>> {
        if branch.parent == branch.id {
            return None;
        }
        self.branches.get(&branch.parent)
    }

    /// Walk back the chain until the target slot, stopping at the oldest block
    /// in the tree.
    fn walk_back_before<'a>(&'a self, branch: &'a Branch<Id>, slot: Slot) -> &'a Branch<Id> {
        let mut current = branch;
        while current.slot > slot {
            let Some(parent) = self.parent(current) else {
                break;
            };
            current = parent;
        }
        current
    }

    /// Walk back the chain and return all blocks in the range
    /// `[branch.id, target_exclusive)`.
    ///
    /// Ends at the oldest block in the tree if `target_exclusive` is not an
    /// ancestor of `branch` or is not in the tree (pruned).
    fn walk_back_to_block<'s>(
        &'s self,
        branch: &'s Branch<Id>,
        target_exclusive: Id,
    ) -> impl Iterator<Item = Id> + 's {
        let mut current = Some(branch);
        std::iter::from_fn(move || {
            let branch = current?;
            if branch.id == target_exclusive {
                return None;
            }
            current = self.parent(branch);
            Some(branch.id)
        })
    }

    /// Returns the min(n, A)-th ancestor of the provided block, where A is the
    /// number of ancestors of this block.
    fn nth_ancestor<'a>(&'a self, branch: &'a Branch<Id>, mut n: u64) -> &'a Branch<Id> {
        let mut current = branch;
        while n > 0 {
            n -= 1;
            let Some(parent) = self.parent(current) else {
                return current;
            };
            current = parent;
        }
        current
    }

    /// Walks back from `branch`, newest first, while the slot is newer than
    /// `oldest_slot`. `branch` itself is yielded first if it qualifies.
    ///
    /// Ends early at the oldest block in the tree.
    pub fn ancestors_newer_than<'s>(
        &'s self,
        branch: &'s Branch<Id>,
        oldest_slot: Slot,
    ) -> impl Iterator<Item = &'s Branch<Id>> + 's {
        let mut current = Some(branch);
        core::iter::from_fn(move || {
            let branch = current?;
            if branch.slot <= oldest_slot {
                return None;
            }
            current = self.parent(branch);
            Some(branch)
        })
    }
}

#[derive(Debug, Clone, Error)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub enum Error<Id> {
    #[error("Parent block: {0:?} is not know to this node")]
    ParentMissing(Id),
    #[error("Orphan proof has was not found in the ledger: {0:?}, can't import it")]
    OrphanMissing(Id),
    #[error("Invalid slot for block {0:?}, parent slot is greater than child slot")]
    InvalidSlot(Id),
}

/// Information about a fork's divergence from the canonical branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkDivergenceInfo<Id> {
    /// The tip of the diverging fork.
    pub tip: Branch<Id>,
    /// The LCA (lowest common ancestor) of the fork and the local canonical
    /// chain.
    pub lca: Branch<Id>,
}

impl<Id> Cryptarchia<Id>
where
    Id: Eq + Hash + Copy + Debug,
{
    pub fn from_lib(
        id: Id,
        config: Config,
        state: State,
        slot: Slot,
        length: u64,
        uncle_slots: UncleSlots,
    ) -> Self {
        Self {
            branches: Branches::from_lib(id, slot, length, uncle_slots.clone()),
            local_chain: Branch {
                id,
                length,
                parent: id,
                slot,
                uncle_slots,
            },
            config,
            state,
        }
    }

    /// Apply the given block.
    ///
    /// On success, returns the pruned/reorged blocks resulting from the update.
    /// On error, `self` is not modified.
    #[must_use = "Returns a new instance with the updated state, without modifying the original."]
    pub fn receive_block(
        &mut self,
        id: Id,
        parent: Id,
        slot: Slot,
        uncle_slots: UncleSlots,
    ) -> Result<(PrunedBlocks<Id>, ReorgedBlocks<Id>), Error<Id>> {
        let old_local_chain = self.local_chain.clone();

        self.branches.apply_header(id, parent, slot, uncle_slots)?;
        self.local_chain = self.fork_choice().clone();

        // Before `update_lib` which may prune blocks,
        // collect the reorged blocks in the old local chain.
        let reorged_blocks = if self.local_chain.id == old_local_chain.id {
            ReorgedBlocks::new()
        } else {
            // It's safer to compute LCA here, not in `fork_choice`,
            // because `fork_choice` may walk through multiple candidates
            // whose pairwise LCAs don't lie on `old_local_chain`'s parent chain.
            let lca = self
                .branches
                .lca(&old_local_chain, &self.local_chain)
                .expect("old and new local chains must have a common ancestor");
            ReorgedBlocks(
                self.branches
                    .walk_back_to_block(&old_local_chain, lca.id())
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
    /// If the LIB is updated, forks that diverged before the new LIB
    /// are pruned, and the blocks of the pruned forks are returned.
    /// as [`PrunedBlocks`].
    /// Otherwise, an empty [`PrunedBlocks`] is returned.
    fn update_lib(&mut self) -> PrunedBlocks<Id> {
        let new_lib = State::lib(&*self);
        // Trigger pruning only if the LIB has changed.
        if self.branches.lib == new_lib {
            PrunedBlocks::new()
        } else {
            self.branches.lib = new_lib;
            PrunedBlocks {
                // TODO: Eliminate the need of `lib_depth` by refactoring `prune_stale_forks`,
                //       similar as `prune_immutable_blocks`.
                stale_blocks: self.prune_stale_forks(self.lib_depth()).collect(),
                immutable_blocks: self.prune_immutable_blocks().collect(),
            }
        }
    }

    /// Runs the fork choice rule and returns the selected new local chain tip.
    pub fn fork_choice(&self) -> &Branch<Id> {
        State::fork_choice(self)
    }

    pub const fn tip(&self) -> Id {
        self.local_chain.id
    }

    pub const fn tip_branch(&self) -> &Branch<Id> {
        &self.local_chain
    }

    /// Prune all blocks that are included in forks that diverged before
    /// the `max_div_depth`-th block from the current local chain tip.
    /// It returns the block IDs that were part of the pruned forks.
    ///
    /// For example,
    /// Given a block tree:
    ///               b6
    ///             /
    /// G - b1 - b2 - b3 - b4 - b5 == local chain tip
    ///                  \
    ///                    b7
    /// Calling `prune_forks(2)` will remove `b6` because it is diverged from
    /// `b2`, which is deeper than the 2nd block `b3` from the local chain tip.
    /// The `b7` is not removed since it is diverged from `b3`.
    fn prune_stale_forks(&mut self, max_div_depth: u64) -> impl Iterator<Item = Id> + '_ {
        #[expect(
            clippy::needless_collect,
            reason = "We need to collect since we cannot borrow both immutably (in `self.prunable_forks`) and mutably (in `self.prune_fork`) at the same time."
        )]
        // Collect prunable forks first to avoid borrowing issues
        let forks: Vec<_> = self.prunable_forks(max_div_depth).collect();
        forks
            .into_iter()
            .flat_map(move |prunable_fork_info| self.prune_fork(&prunable_fork_info))
    }

    /// Get an iterator over the prunable forks that diverged before
    /// the `max_div_depth`-th block from the current local chain tip.
    fn prunable_forks(
        &self,
        max_div_depth: u64,
    ) -> impl Iterator<Item = ForkDivergenceInfo<Id>> + '_ {
        let local_chain = &self.local_chain;
        let Some(deepest_div_block) = local_chain.length.checked_sub(max_div_depth) else {
            tracing::debug!(
                target: LOG_TARGET,
                "No prunable fork, the canonical chain is not longer than the provided depth. Canonical chain length: {}, provided max_div_depth: {}", local_chain.length, max_div_depth
            );
            return Box::new(core::iter::empty())
                as Box<dyn Iterator<Item = ForkDivergenceInfo<Id>>>;
        };
        Box::new(self.non_canonical_forks().filter_map(move |fork| {
            // We calculate LCA once and store it in `ForkInfo` so it can be consumed
            // elsewhere without the need to re-calculate it.
            let lca = self
                .branches
                .lca(local_chain, fork)
                .expect("local chain and fork must have a common ancestor");
            // If the fork is diverged deeper than `deepest_div_block`, it's prunable.
            (lca.length < deepest_div_block).then_some(ForkDivergenceInfo {
                tip: fork.clone(),
                lca: lca.clone(),
            })
        }))
    }

    /// Returns all the forks that are not part of the local canonical chain.
    ///
    /// The result contains both prunable and non prunable forks.
    pub fn non_canonical_forks(&self) -> impl Iterator<Item = &Branch<Id>> + '_ {
        self.branches
            .branches()
            .filter(|fork_tip| fork_tip.id != self.tip())
    }

    /// Remove all blocks of a fork from `tip` to `lca`, excluding `lca`.
    fn prune_fork(&mut self, ForkDivergenceInfo { lca, tip }: &ForkDivergenceInfo<Id>) -> Vec<Id> {
        let tip_removed = self.branches.tips.remove_mut(&tip.id);
        if !tip_removed {
            tracing::error!(target: LOG_TARGET, "Fork tip {tip:#?} not found in the set of tips.");
        }

        let mut current_tip = tip.id;
        let mut removed_blocks = vec![];
        while current_tip != lca.id {
            let Some(branch) = self.branches.branches.get(&current_tip).cloned() else {
                // If tip is not in branch set, it means this tip was sharing part of its
                // history with another fork that has already been removed.
                break;
            };
            self.branches.branches.remove_mut(&current_tip);
            removed_blocks.push(branch.id);
            current_tip = branch.parent;
        }
        tracing::debug!(
            target: LOG_TARGET,
            "Pruned {} blocks from {tip:#?} to {current_tip:#?}.", removed_blocks.len()
        );
        removed_blocks
    }

    /// Prunes all immutable blocks (excluding LIB) that are deeper than LIB,
    /// and returns the slots and IDs of the pruned blocks.
    fn prune_immutable_blocks(&mut self) -> impl Iterator<Item = (Slot, Id)> + '_ {
        let mut block = self.lib_branch().parent;
        std::iter::from_fn(move || {
            let &Branch {
                id, parent, slot, ..
            } = self.branches.branches.get(&block)?;
            self.branches.branches.remove_mut(&block);
            block = parent;
            Some((slot, id))
        })
    }

    pub const fn branches(&self) -> &Branches<Id> {
        &self.branches
    }

    /// Get the latest immutable block (LIB) in the chain. No re-orgs past this
    /// point are allowed.
    pub const fn lib(&self) -> Id {
        self.branches.lib
    }

    pub fn lib_branch(&self) -> &Branch<Id> {
        &self.branches.branches[&self.lib()]
    }

    pub const fn state(&self) -> &State {
        &self.state
    }

    /// Calculate the depth of LIB from the local chain tip.
    fn lib_depth(&self) -> u64 {
        self.tip_branch()
            .length()
            .checked_sub(self.lib_branch().length())
            .expect("Local chain tip height must be >= LIB height.")
    }

    pub fn online(mut self) -> (Self, PrunedBlocks<Id>) {
        self.state = State::Online;
        // Update the LIB to the current local chain's tip
        let pruned_blocks = self.update_lib();
        (self, pruned_blocks)
    }

    pub const fn config(&self) -> &Config {
        &self.config
    }

    /// Selects uncles for a new block extending `parent` at `slot`.
    ///
    /// First, this function filters candidates which meet all of the following
    /// conditions:
    /// - An uncle is the 1st block of a fork off the chain of `parent`.
    /// - An uncle's slot is `< slot`.
    /// - An uncle's parent slot is within the uncle reference window.
    ///   - `0 < slot - uncle.parent.slot <= w_u`.
    /// - An uncle's slot is not already occupied by the ancestors of `parent`
    ///   (including `parent` itself) and their uncles.
    ///
    /// After that,
    /// - All uncle candidates are ordered by `(parent.slot, slot, id)`.
    /// - The first [`MAX_UNCLES`] uncles are selected.
    /// - But, if an uncle's slot is already taken by another uncle selected
    ///   earlier, the uncle is excluded.
    ///
    /// # Example
    /// ```text
    ///          |---------------------- window -------------------------|
    ///          |                                                       |
    /// - b1(1) - b2(2) ------ b3(4, uncle_slots=[3]) -- b4(6)           <- adding b5(9)
    ///    |       |            |                         |----------------- u9(9)
    ///    |       |            |                         |---u7(7)--u8(8)
    ///    |       |            |----------------------------------- u6(8)
    ///    |       |            |------------------- u5(5)
    ///    |       |-------------------------------- u4(5)
    ///    |       |---------- u3(4)
    ///    |       |---- u2(3)
    ///    |------ u1(2)
    /// ```
    /// Only `[u4, u6, u7]` are selected.
    /// - u1's parent is out of the window.
    /// - u2's slot(3) is already occupied by b3's uncle.
    /// - u3's slot(4) is already occupied by b3.
    /// - u5's slot is already taken by u4 selected earlier.
    /// - u8 is not the 1st block of a fork.
    /// - u9's slot(9) is not smaller than the slot(9) of the new block.
    pub fn select_uncles(&self, parent: &Branch<Id>, slot: Slot) -> Vec<&Branch<Id>>
    where
        Id: Ord,
    {
        let window_start =
            u64::from(slot).saturating_sub(self.config.uncle_reference_window().get());

        let (ancestors, occupied_slots) = self.collect_chain_within_window(parent, window_start);
        let mut candidates = self.uncle_candidates(slot, window_start, &ancestors, &occupied_slots);

        // Oldest parent first, because the slot window moves and the oldest candidates
        // are the closest to expiring.
        // One uncle per slot, breaking ties by the uncle's slot and ID.
        candidates
            .sort_unstable_by_key(|(parent_slot, uncle)| (*parent_slot, uncle.slot, uncle.id));
        let mut selected_slots = HashSet::new();
        candidates
            .into_iter()
            .filter(|(_, uncle)| selected_slots.insert(uncle.slot))
            .take(MAX_UNCLES)
            .map(|(_, uncle)| uncle)
            .collect()
    }

    /// Within the window, collects the ancestors of `parent` (including itself)
    /// and the slots occupied by them and their uncles.
    ///
    /// Ancestors outside the window don't need to be checked because their
    /// uncles must be older than the window.
    /// Complexity: `O(window_size)`
    fn collect_chain_within_window(
        &self,
        parent: &Branch<Id>,
        window_start: u64,
    ) -> (HashMap<Id, Slot>, HashSet<Slot>) {
        let mut ancestors = HashMap::new();
        let mut occupied_slots = HashSet::new();
        let mut ancestor = Some(parent);
        while let Some(block) = ancestor {
            if block.slot.into_inner() < window_start {
                break;
            }
            ancestors.insert(block.id, block.slot);
            occupied_slots.insert(block.slot);
            occupied_slots.extend(
                block
                    .uncle_slots
                    .iter()
                    .filter(|uncle_slot| uncle_slot.into_inner() >= window_start)
                    .copied(),
            );
            ancestor = self.branches.parent(block);
        }
        (ancestors, occupied_slots)
    }

    /// Collects uncle candidates by walking back all branches in the tree,
    /// to find the 1st block of each fork that meets the criteria.
    ///
    /// Each uncle candidate is returned with its parent's slot.
    ///
    /// Complexity: `O(n_forks * window_size)`
    fn uncle_candidates(
        &self,
        slot: Slot,
        window_start: u64,
        ancestors: &HashMap<Id, Slot>,
        occupied_slots: &HashSet<Slot>,
    ) -> Vec<(Slot, &Branch<Id>)> {
        let mut candidates: Vec<(Slot, &Branch<Id>)> = Vec::new();
        for branch_tip in self.branches.branches() {
            let mut current = Some(branch_tip);
            while let Some(block) = current {
                // Reached the chain of `parent` or the window's end.
                if block.slot.into_inner() < window_start || ancestors.contains_key(&block.id) {
                    break;
                }
                if block.slot < slot
                    && let Some(parent_slot) = ancestors.get(&block.parent)
                    && !occupied_slots.contains(&block.slot)
                {
                    candidates.push((*parent_slot, block));
                }
                current = self.branches.parent(block);
            }
        }
        candidates
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

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stale_blocks.is_empty() && self.immutable_blocks.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
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

pub struct ReorgedBlocks<Id>(Vec<Id>);

impl<Id> ReorgedBlocks<Id> {
    #[must_use]
    const fn new() -> Self {
        Self(vec![])
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

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

    use super::{Cryptarchia, Error, Slot, UncleSlots, maxvalid_bg};
    use crate::{Config, ReorgedBlocks, State};

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
            NonZero::new(12).unwrap(),
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
            UncleSlots::default(),
        );
        let mut parent = engine.lib();
        for i in 1..length.get() {
            let new_block = hash(&i);
            let (_, reorged_blocks) = engine
                .receive_block(new_block, parent, i.into(), UncleSlots::default())
                .expect("test block to be applied successfully.");
            assert!(
                reorged_blocks.is_empty(),
                "no reorgs should happen in a canonical chain"
            );
            parent = new_block;
        }
        engine
    }

    #[test]
    fn test_slot_increasing() {
        // parent
        // └── child

        let mut branches =
            super::Branches::from_lib(hash(&0u64), 0.into(), 0, UncleSlots::default());
        let parent = hash(&1u64);
        let child = hash(&2u64);

        branches
            .apply_header(parent, hash(&0u64), 2.into(), UncleSlots::default())
            .unwrap();
        assert!(matches!(
            branches.apply_header(child, parent, 1.into(), UncleSlots::default()),
            Err(Error::InvalidSlot(_))
        ));
    }

    #[test]
    fn lca_with_branch_outside_the_tree() {
        // b0(LIB) - b1 - b2      c0 (a separate tree)
        let cryptarchia = create_canonical_chain(3.try_into().unwrap(), None);
        let branches = cryptarchia.branches();
        let other = super::Branches::from_lib(hash(&100u64), 0.into(), 0, UncleSlots::default());

        assert!(
            branches
                .lca(
                    branches.get(&hash(&2u64)).unwrap(),
                    other.get(&hash(&100u64)).unwrap(),
                )
                .is_none()
        );
    }

    #[test]
    fn walk_back_before_stops_at_the_oldest_block() {
        // b0(LIB, slot 5) - b1(slot 6)
        let mut branches =
            super::Branches::from_lib(hash(&0u64), 5.into(), 0, UncleSlots::default());
        branches
            .apply_header(hash(&1u64), hash(&0u64), 6.into(), UncleSlots::default())
            .unwrap();

        // Slot 0 precedes the oldest block, so the walk stops there.
        assert_eq!(
            branches
                .walk_back_before(branches.get(&hash(&1u64)).unwrap(), 0.into())
                .id(),
            hash(&0u64)
        );
    }

    #[test]
    fn walk_back_to_block_outside_the_tree() {
        // b0(LIB) - b1 - b2
        let cryptarchia = create_canonical_chain(3.try_into().unwrap(), None);
        let branches = cryptarchia.branches();

        // The target is not an ancestor, so the walk ends at the oldest block.
        assert_eq!(
            branches
                .walk_back_to_block(branches.get(&hash(&2u64)).unwrap(), hash(&100u64))
                .collect::<Vec<_>>(),
            vec![hash(&2u64), hash(&1u64), hash(&0u64)]
        );
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
            cryptarchia.receive_block(hash(&3u64), hash(&0u64), 1.into(), UncleSlots::default()),
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
                    .receive_block(new_block, short_p, slot.into(), UncleSlots::default())
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
                    .receive_block(new_block, long_p, slot.into(), UncleSlots::default())
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
                .receive_block(new_block, long_p, slot.into(), UncleSlots::default())
                .unwrap();
            assert!(reorged_blocks.is_empty());
            long_p = new_block;
            assert_eq!(engine.tip(), short_p);
        }

        {
            let bs = engine.branches();
            let long_branch = bs.branches().find(|b| b.id == long_p).unwrap();
            let short_branch = bs.branches().find(|b| b.id == short_p).unwrap();

            // however, if we set k to the fork length, it will be accepted
            let k = long_branch.length;
            assert_eq!(
                maxvalid_bg(short_branch, engine.branches(), k, engine.config.s_gen()).id,
                long_p
            );

            // a new denser chain will be selected as the main tip
            let mut parent = orig_engine.tip();
            let tip_height = engine.tip_branch().length;
            for slot in initial_height..=tip_height {
                let new_block = hash(&format!("dense-{slot}"));
                let (_, reorged_blocks) = engine
                    .receive_block(new_block, parent, slot.into(), UncleSlots::default())
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
        let engine = <Cryptarchia<_>>::from_lib(
            hash(&0u64),
            config(),
            State::Bootstrapping,
            0.into(),
            0,
            UncleSlots::default(),
        );
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
            .receive_block(hash(&100u64), hash(&0u64), 1.into(), UncleSlots::default())
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
        assert!(cryptarchia.branches.branches.contains_key(&hash(&100u64)));

        // Add two new blocks to the local honest chain,
        // and check if the LIB is updated and blocks are pruned.
        let (pruned_blocks, _) = cryptarchia
            .receive_block(hash(&50u64), hash(&49u64), 50.into(), UncleSlots::default())
            .expect("test block to be applied successfully.");
        assert!(pruned_blocks.is_empty());
        let (pruned_blocks, _) = cryptarchia
            .receive_block(hash(&51u64), hash(&50u64), 51.into(), UncleSlots::default())
            .expect("test block to be applied successfully.");
        // The LIB was updated to b1.
        assert_eq!(cryptarchia.lib(), hash(&1u64));
        // The stale fork b100 was pruned.
        assert_eq!(pruned_blocks.stale_blocks, [hash(&100u64)].into());
        assert!(!cryptarchia.branches.tips.contains(&hash(&100u64)));
        assert!(!cryptarchia.branches.branches.contains_key(&hash(&100u64)));
        // The immutable block b0 was pruned.
        assert_eq!(
            pruned_blocks.immutable_blocks,
            [(0.into(), hash(&0u64))].into()
        );
        assert!(!cryptarchia.branches.tips.contains(&hash(&0u64)));
        assert!(!cryptarchia.branches.branches.contains_key(&hash(&0u64)));
    }

    #[test]
    fn pruning_with_no_stale_fork() {
        // Create a chain with 50 blocks with k=10.
        // b0(LIB) - b1 - ... b39 - b40 - ... - b49
        //                              \
        //                               b100
        let mut cryptarchia = create_canonical_chain(50.try_into().unwrap(), Some(config_with(10)));
        let (pruned_blocks, _) = cryptarchia
            .receive_block(
                hash(&100u64),
                hash(&40u64),
                41.into(),
                UncleSlots::default(),
            )
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
        assert!(cryptarchia.branches.branches.contains_key(&hash(&100u64)));

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
            .receive_block(
                hash(&100u64),
                hash(&38u64),
                39.into(),
                UncleSlots::default(),
            )
            .expect("test block to be applied successfully.");
        cryptarchia
            .receive_block(
                hash(&101u64),
                hash(&39u64),
                40.into(),
                UncleSlots::default(),
            )
            .expect("test block to be applied successfully.");
        let (pruned_blocks, _) = cryptarchia
            .receive_block(
                hash(&102u64),
                hash(&40u64),
                41.into(),
                UncleSlots::default(),
            )
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
        assert!(!cryptarchia.branches.branches.contains_key(&hash(&100u64)));

        // Other forks were not pruned
        assert!(cryptarchia.branches.tips.contains(&hash(&101u64)));
        assert!(cryptarchia.branches.branches.contains_key(&hash(&101u64)));
        assert!(cryptarchia.branches.tips.contains(&hash(&102u64)));
        assert!(cryptarchia.branches.branches.contains_key(&hash(&102u64)));

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
            .receive_block(
                hash(&100u64),
                hash(&38u64),
                39.into(),
                UncleSlots::default(),
            )
            .expect("test block to be applied successfully.");
        cryptarchia
            .receive_block(
                hash(&200u64),
                hash(&38u64),
                39.into(),
                UncleSlots::default(),
            )
            .expect("test block to be applied successfully.");
        let (pruned_blocks, _) = cryptarchia
            .receive_block(
                hash(&101u64),
                hash(&39u64),
                40.into(),
                UncleSlots::default(),
            )
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
        assert!(!cryptarchia.branches.branches.contains_key(&hash(&100u64)));
        assert!(!cryptarchia.branches.tips.contains(&hash(&200u64)));
        assert!(!cryptarchia.branches.branches.contains_key(&hash(&200u64)));

        // Fork at b39 was not pruned.
        assert!(cryptarchia.branches.tips.contains(&hash(&101u64)));
        assert!(cryptarchia.branches.branches.contains_key(&hash(&101u64)));

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
            .receive_block(
                hash(&100u64),
                hash(&38u64),
                39.into(),
                UncleSlots::default(),
            )
            .expect("test block to be applied successfully.");
        cryptarchia
            .receive_block(
                hash(&101u64),
                hash(&100u64),
                40.into(),
                UncleSlots::default(),
            )
            .expect("test block to be applied successfully.");
        let (pruned_blocks, _) = cryptarchia
            .receive_block(
                hash(&200u64),
                hash(&100u64),
                41.into(),
                UncleSlots::default(),
            )
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
        assert!(!cryptarchia.branches.branches.contains_key(&hash(&100u64)));
        assert!(!cryptarchia.branches.branches.contains_key(&hash(&101u64)));
        assert!(!cryptarchia.branches.tips.contains(&hash(&200u64)));
        assert!(!cryptarchia.branches.branches.contains_key(&hash(&200u64)));

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
                cryptarchia.lib_branch().slot.strict_add(1.into()),
                UncleSlots::default(),
            )
            .expect("test block to be applied successfully.");
        assert_eq!(cryptarchia.lib(), hash(&7u64));
        // No block is pruned since LIB was not updated.
        assert!(pruned_blocks.all().next().is_none());
        assert!(cryptarchia.branches.tips.contains(&hash(&100u64)));
        assert!(cryptarchia.branches.branches.contains_key(&hash(&100u64)));

        // Add a fork after than LIB
        // b7(LIB) - b8 - b9
        //         \    \
        //          b100 b101
        let (pruned_blocks, _) = cryptarchia
            .receive_block(
                hash(&101u64),
                cryptarchia.tip_branch().parent,
                cryptarchia.tip_branch().slot,
                UncleSlots::default(),
            )
            .expect("test block to be applied successfully.");
        assert_eq!(cryptarchia.lib(), hash(&7u64));
        // No block was pruned since LIB was not updated.
        assert!(pruned_blocks.all().next().is_none());
        assert!(cryptarchia.branches.tips.contains(&hash(&100u64)));
        assert!(cryptarchia.branches.branches.contains_key(&hash(&100u64)));
        assert!(cryptarchia.branches.tips.contains(&hash(&101u64)));
        assert!(cryptarchia.branches.branches.contains_key(&hash(&101u64)));

        // Add a block to the tip to update the LIB.
        // b7 - b8(LIB) - b9 - b102
        //    \         \
        //     b100      b101
        let (pruned_blocks, _) = cryptarchia
            .receive_block(
                hash(&102u64),
                cryptarchia.tip(),
                cryptarchia.tip_branch().slot.strict_add(1.into()),
                UncleSlots::default(),
            )
            .expect("test block to be applied successfully.");
        assert_eq!(cryptarchia.lib(), hash(&8u64));
        // One fork (b100) was pruned since LIB was updated.
        assert_eq!(pruned_blocks.stale_blocks, [hash(&100u64)].into());
        assert!(!cryptarchia.branches.tips.contains(&hash(&100u64)));
        assert!(!cryptarchia.branches.branches.contains_key(&hash(&100u64)));
        // b101 and b102 were not pruned.
        assert!(cryptarchia.branches.tips.contains(&hash(&101u64)));
        assert!(cryptarchia.branches.branches.contains_key(&hash(&101u64)));
        assert!(cryptarchia.branches.tips.contains(&hash(&102u64)));
        assert!(cryptarchia.branches.branches.contains_key(&hash(&102u64)));
        // Immutable blocks (excluding LIB) were pruned.
        assert_eq!(
            pruned_blocks.immutable_blocks,
            [(7.into(), hash(&7u64))].into(),
        );
    }
}

#[cfg(test)]
mod uncle_tests {
    use std::num::NonZero;

    use lb_utils::math::NonNegativeRatio;

    use crate::{Config, Cryptarchia, Slot, State, UncleSlots};

    #[test]
    fn select_uncles_honors_the_window() {
        //           |----- window ------|
        //           |                   |
        // g(0) ----- b1(8) -- b2(10)     <- adding b2(11)
        //  |          |-- u3(9)
        //  |          |------------------- u4(11)
        //  |-- u1(7)
        //  |-------- u2(8)
        //
        // Only u3 is selected. Other uncles are excluded because of the following
        // reasons:
        // - u1's parent is older than the window.
        // - u2's parent is older than the window.
        // - u4 is out of the window.
        let [g, b1, b2, u1, u2, u3, u4] = [0u64, 1, 2, 3, 4, 5, 6];
        let window = 3;
        let engine = build_tree(
            window,
            g,
            [
                (b1, g, 8.into()),
                (b2, b1, 10.into()),
                (u1, g, 7.into()),
                (u2, g, 8.into()),
                (u3, b1, 9.into()),
                (u4, b1, 11.into()),
            ],
        );

        let selected: Vec<_> = engine
            .select_uncles(engine.branches().get(&b2).unwrap(), 11.into())
            .iter()
            .map(|uncle| uncle.id())
            .collect();
        assert_eq!(selected, [u3]);
    }

    #[test]
    fn select_uncles_selects_nothing_with_the_minimum_window() {
        //              |-- window ---|
        //              |             |
        // g(0) -- b1(9) -- b2(10)     <- adding b2(11)
        //          |        |----------- u3(11)
        //          |
        //  |       |------ u2(10)
        //  |
        //  |----- u1(9)
        //
        // With `w_u = 1`, a proposal at slot 11 can only reference slot 10.
        // It means that no uncle can be selected.
        let [g, b1, b2, u1, u2, u3] = [0u64, 1, 2, 3, 4, 5];
        let window = 1;
        let engine = build_tree(
            window,
            g,
            [
                (b1, g, 9.into()),
                (b2, b1, 10.into()),
                (u1, g, 9.into()),
                (u2, b1, 10.into()),
                (u3, b2, 11.into()),
            ],
        );

        assert!(
            engine
                .select_uncles(engine.branches().get(&b1).unwrap(), 11.into())
                .is_empty()
        );
    }

    #[test]
    fn select_uncles_takes_at_most_max_uncles() {
        //  |----------------- window -------------------|
        //  |                                            |
        // g(0) ----------------------------------- b1(6)  <- adding b2(7)
        //  |
        //  |- u1(1)
        //  |-------- u2(2)
        //  |--------------- u3(3)
        //  |--------------------- u4(4)
        //  |--------------------------- u5(5)
        //
        // u1~u4 are selected. u5 is excluded because of `MAX_UNCLES=4`.
        let [g, b1, u1, u2, u3, u4, u5] = [0u64, 1, 2, 3, 4, 5, 6];
        let window = 10;
        let engine = build_tree(
            window,
            g,
            [
                (b1, g, 6.into()),
                (u1, g, 1.into()),
                (u2, g, 2.into()),
                (u3, g, 3.into()),
                (u4, g, 4.into()),
                (u5, g, 5.into()),
            ],
        );

        let selected: Vec<_> = engine
            .select_uncles(engine.branches().get(&b1).unwrap(), 7.into())
            .iter()
            .map(|uncle| uncle.id())
            .collect();
        assert_eq!(selected, [u1, u2, u3, u4]);
    }

    #[test]
    fn select_uncles_takes_only_first_block_of_forks() {
        //  |----------------- window -------------------|
        //  |                                            |
        // g(0) ----------------------------------- b1(6)  <- adding b2(7)
        //  |
        //  |- u1(1) -- u2(2)
        //
        // Only u1 is selected. u2 is excluded because it is not the first
        // block of the fork.
        let [g, b1, u1, u2] = [0u64, 1u64, 2, 3];
        let window = 10;
        let engine = build_tree(
            window,
            g,
            [(b1, g, 6.into()), (u1, g, 1.into()), (u2, u1, 2.into())],
        );

        let selected: Vec<_> = engine
            .select_uncles(engine.branches().get(&b1).unwrap(), 7.into())
            .iter()
            .map(|uncle| uncle.id())
            .collect();
        assert_eq!(selected, [u1]);
    }

    #[test]
    fn select_uncles_skips_occupied_slots() {
        //        |------------------- window ------------------|
        //        |                                             |
        // g(0) -- b1(5) -- b3(7, uncle_slots=[6]) -- b4(8)     <- adding b5(10)
        //           |       |                         |---u4(9)
        //           |       |
        //           |       |----------------------- u3(8)
        //           |
        //           |-- u1(6)
        //           |-- u2(7)
        //
        // Only u4 is selected. Other uncles' slots are already occupied:
        // - u1's slot 6 is occupied by b3's uncle slot.
        // - u2's slot 7 is occupied by b3.
        // - u3's slot 8 is occupied by b4.

        let [g, b1, b3, b4, u1, u2, u3, u4] = [0u64, 1, 2, 3, 4, 5, 6, 7];
        let window = 5;
        let engine = build_tree_with_uncle_slots(
            window,
            g,
            [
                (b1, g, 5.into(), UncleSlots::default()),
                (b3, b1, 7.into(), [6u64.into()].into()),
                (b4, b3, 8.into(), UncleSlots::default()),
                (u1, b1, 6.into(), UncleSlots::default()),
                (u2, b1, 7.into(), UncleSlots::default()),
                (u3, b3, 8.into(), UncleSlots::default()),
                (u4, b4, 9.into(), UncleSlots::default()),
            ],
        );

        let selected: Vec<_> = engine
            .select_uncles(engine.branches().get(&b4).unwrap(), 10.into())
            .iter()
            .map(|uncle| uncle.id())
            .collect();
        assert_eq!(selected, [u4]);
    }

    #[test]
    fn select_uncles_takes_one_uncle_per_slot() {
        // |----------- window -----------|
        // |                              |
        // g(0) -- b1(6) --- b2(8)         <- adding b3(11)
        //          |        |-------u3(10)
        //          |        |--- u2(9)
        //          |--------------- u1(10)
        //
        // [u1, u2] are selected. u3's slot is colliding with u1's slot.
        let [g, b1, b2, u1, u2, u3] = [0u64, 1u64, 2, 3, 4, 5];
        let window = 15;
        let engine = build_tree(
            window,
            g,
            [
                (b1, g, 6.into()),
                (b2, b1, 8.into()),
                (u1, b1, 10.into()),
                (u2, b2, 9.into()),
                (u3, b2, 10.into()),
            ],
        );

        let selected: Vec<_> = engine
            .select_uncles(engine.branches().get(&b2).unwrap(), 11.into())
            .iter()
            .map(|uncle| uncle.id())
            .collect();
        assert_eq!(selected, [u1, u2]);
    }

    #[test]
    fn select_uncles_ordering() {
        // |-------------------- window ------------------|
        // |                                              |
        // g(0) -- b1(3) -- b2(4)                         <-- adding b3(8)
        //          |         |------------------- u4(7)
        //          |         |------------------- u3(7)
        //          |--------------------- u1(6)
        //          |--------------- u2(5)
        //
        // The order of the selected uncles should be: [u2, u1, u3].
        // - The parent slot(3) of u1 and u2 is smaller than the parent slot(4) of u3
        //   and u4.
        // - u2's slot(5) is smaller than u1's slot(6).
        // - u3's ID(3) is smaller than u4's ID(4).
        let [g, b1, b2, u1, u2, u3, u4] = [0u64, 1u64, 2, 3, 4, 5, 6];
        let window = 10;
        let engine = build_tree(
            window,
            g,
            [
                (b1, g, 3.into()),
                (b2, b1, 4.into()),
                (u1, b1, 6.into()),
                (u2, b1, 5.into()),
                (u3, b2, 7.into()),
                (u4, b2, 7.into()),
            ],
        );

        let selected: Vec<_> = engine
            .select_uncles(engine.branches().get(&b2).unwrap(), 8.into())
            .iter()
            .map(|uncle| uncle.id())
            .collect();
        assert_eq!(selected, [u2, u1, u3]);
    }

    /// A config whose uncle reference window `floor(W/f)` is exactly
    /// `uncle_reference_window` slots, by picking an `f` just below 1.
    #[must_use]
    fn config(uncle_reference_window: u32) -> Config {
        Config::new(
            NonZero::new(10).unwrap(),
            NonNegativeRatio::new(
                uncle_reference_window + 1,
                (uncle_reference_window + 2).try_into().unwrap(),
            ),
            1f64.try_into().expect("1 > 0"),
            uncle_reference_window.try_into().unwrap(),
        )
    }

    type HeaderId = u64;

    fn build_tree(
        uncle_reference_window: u32,
        genesis: HeaderId,
        blocks: impl IntoIterator<Item = (HeaderId, HeaderId, Slot)>,
    ) -> Cryptarchia<HeaderId> {
        build_tree_with_uncle_slots(
            uncle_reference_window,
            genesis,
            blocks
                .into_iter()
                .map(|(id, parent, slot)| (id, parent, slot, UncleSlots::default())),
        )
    }

    fn build_tree_with_uncle_slots(
        uncle_reference_window: u32,
        genesis: HeaderId,
        blocks: impl IntoIterator<Item = (HeaderId, HeaderId, Slot, UncleSlots)>,
    ) -> Cryptarchia<HeaderId> {
        let mut engine = Cryptarchia::from_lib(
            genesis,
            config(uncle_reference_window),
            State::Bootstrapping,
            0.into(),
            0,
            UncleSlots::default(),
        );
        for (id, parent, slot, uncle_slots) in blocks {
            engine.receive_block(id, parent, slot, uncle_slots).unwrap();
        }
        engine
    }
}
