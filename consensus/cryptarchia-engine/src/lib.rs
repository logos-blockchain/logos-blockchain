#![expect(
    clippy::disallowed_script_idents,
    reason = "The crate `cfg_eval` contains Sinhala script identifiers. \
    Using the `expect` or `allow` macro on top of their usage does not remove the warning"
)]

pub mod config;
pub mod time;

use core::{fmt::Debug, hash::Hash};
use std::collections::{BTreeMap, HashMap, HashSet};

pub use config::*;
use thiserror::Error;
pub use time::{Epoch, EpochConfig, Slot};

pub(crate) const LOG_TARGET: &str = "cryptarchia::engine";

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
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

    /// Runs the fork choice rule to select the new local chain.
    /// Returns the new local chain and the lowest common ancestor with the
    /// previous.
    fn fork_choice<Id>(cryptarchia: &Cryptarchia<Id>) -> (Branch<Id>, Branch<Id>)
    where
        Id: Eq + Hash + Copy,
    {
        match cryptarchia.state {
            Self::Bootstrapping => {
                let k = cryptarchia.config.security_param().get().into();
                let s = cryptarchia.config.s();
                maxvalid_bg(cryptarchia.local_chain, &cryptarchia.branches, k, s)
            }
            Self::Online => {
                let k = cryptarchia.config.security_param().get().into();
                maxvalid_mc(cryptarchia.local_chain, &cryptarchia.branches, k)
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
///
/// It returns the new local chain and the lowest common ancestor with the
/// previous.
fn maxvalid_bg<Id>(
    local_chain: Branch<Id>,
    branches: &Branches<Id>,
    k: u64,
    s: u64,
) -> (Branch<Id>, Branch<Id>)
where
    Id: Eq + Hash + Copy,
{
    let mut cmax = local_chain;
    let mut lowest_common_ancestor_with_cmax = local_chain;

    let forks = branches.branches();
    for chain in forks {
        let lowest_common_ancestor = branches.lca(&cmax, &chain);
        let m = cmax.length - lowest_common_ancestor.length;
        if m <= k {
            // Classic longest chain rule with parameter k
            if cmax.length < chain.length {
                cmax = chain;
                lowest_common_ancestor_with_cmax = lowest_common_ancestor;
            }
        } else {
            // The chain is forking too much, we need to pay a bit more attention
            // In particular, select the chain that is the densest after the fork
            let density_slot = Slot::from(u64::from(lowest_common_ancestor.slot) + s);
            let cmax_density = branches.walk_back_before(&cmax, density_slot).length;
            let candidate_density = branches.walk_back_before(&chain, density_slot).length;
            if cmax_density < candidate_density {
                cmax = chain;
                lowest_common_ancestor_with_cmax = lowest_common_ancestor;
            }
        }
    }
    (cmax, lowest_common_ancestor_with_cmax)
}

/// Implementation of the fork choice rule as defined in the Ouroboros Praos
/// paper k defines the forking depth of chain we can accept.
///
/// It returns the new local chain and the lowest common ancestor with the
/// previous.
fn maxvalid_mc<Id>(
    local_chain: Branch<Id>,
    branches: &Branches<Id>,
    k: u64,
) -> (Branch<Id>, Branch<Id>)
where
    Id: Eq + Hash + Copy,
{
    let mut cmax = local_chain;
    let mut lowest_common_ancestor_with_cmax = local_chain;

    let forks = branches.branches();
    for chain in forks {
        let lowest_common_ancestor = branches.lca(&cmax, &chain);
        let m = cmax.length - lowest_common_ancestor.length;
        if m <= k && cmax.length < chain.length {
            // Classic longest chain rule with parameter k
            cmax = chain;
            lowest_common_ancestor_with_cmax = lowest_common_ancestor;
        }
    }
    (cmax, lowest_common_ancestor_with_cmax)
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
pub struct Branches<Id> {
    branches: HashMap<Id, Branch<Id>>,
    tips: HashSet<Id>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Branch<Id> {
    id: Id,
    parent: Id,
    slot: Slot,
    // chain length
    length: u64,
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
}

impl<Id> Branches<Id>
where
    Id: Eq + Hash + Copy,
{
    pub fn from_lib(lib: Id, slot: Slot, length: u64) -> Self {
        let mut branches = HashMap::new();
        branches.insert(
            lib,
            Branch {
                id: lib,
                parent: lib,
                slot,
                length,
            },
        );
        let tips = HashSet::from([lib]);
        Self {
            branches,
            tips,
            lib,
        }
    }

    /// Create a new [`Branches`] instance with the updated state.
    #[must_use = "this returns the result of the operation, without modifying the original"]
    fn apply_header(&self, header: Id, parent: Id, slot: Slot) -> Result<Self, Error<Id>> {
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

        let mut branches = self.branches.clone();
        let mut tips = self.tips.clone();

        tips.remove(&parent);
        tips.insert(header);

        branches.insert(
            header,
            Branch {
                id: header,
                parent,
                length,
                slot,
            },
        );

        Ok(Self {
            branches,
            tips,
            lib: self.lib,
        })
    }

    pub fn branches(&self) -> impl Iterator<Item = Branch<Id>> + '_ {
        self.tips.iter().map(|id| self.branches[id])
    }

    /// find the lowest common ancestor of two branches
    pub fn lca<'a>(&'a self, mut b1: &'a Branch<Id>, mut b2: &'a Branch<Id>) -> Branch<Id> {
        // first reduce branches to the same length
        while b1.length > b2.length {
            b1 = &self.branches[&b1.parent];
        }

        while b2.length > b1.length {
            b2 = &self.branches[&b2.parent];
        }

        // then walk up the chain until we find the common ancestor
        while b1.id != b2.id {
            b1 = &self.branches[&b1.parent];
            b2 = &self.branches[&b2.parent];
        }

        *b1
    }

    pub fn get(&self, id: &Id) -> Option<&Branch<Id>> {
        self.branches.get(id)
    }

    pub fn get_length_for_header(&self, header_id: &Id) -> Option<u64> {
        self.get(header_id).map(|branch| branch.length)
    }

    /// Walk back the chain until the target slot
    fn walk_back_before(&self, branch: &Branch<Id>, slot: Slot) -> Branch<Id> {
        let mut current = branch;
        while current.slot > slot {
            current = &self.branches[&current.parent];
        }
        *current
    }

    /// Walk back the chain and return all blocks in the range
    /// `[branch.id, target_exclusive)`.
    fn walk_back_to_block<'s>(
        &'s self,
        branch: &'s Branch<Id>,
        target_exclusive: Id,
    ) -> impl Iterator<Item = Id> + 's {
        let mut current = branch.id;
        std::iter::from_fn(move || {
            if current == target_exclusive {
                None
            } else {
                let branch = &self.branches[&current];
                current = branch.parent;
                Some(branch.id)
            }
        })
    }

    /// Returns the min(n, A)-th ancestor of the provided block, where A is the
    /// number of ancestors of this block.
    fn nth_ancestor(&self, branch: &Branch<Id>, mut n: u64) -> Branch<Id> {
        let mut current = branch;
        while n > 0 {
            n -= 1;
            if let Some(parent) = self.branches.get(&current.parent) {
                current = parent;
            } else {
                return *current;
            }
        }
        *current
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
    pub fn from_lib(id: Id, config: Config, state: State, slot: Slot, length: u64) -> Self {
        Self {
            branches: Branches::from_lib(id, slot, length),
            local_chain: Branch {
                id,
                length,
                parent: id,
                slot,
            },
            config,
            state,
        }
    }

    /// Create a new [`Cryptarchia`] instance with the updated state
    /// after applying the given block.
    #[must_use = "Returns a new instance with the updated state, without modifying the original."]
    pub fn receive_block(
        &self,
        id: Id,
        parent: Id,
        slot: Slot,
    ) -> Result<UpdatedCryptarchia<Id>, Error<Id>> {
        let mut new: Self = self.clone();
        new.branches = new.branches.apply_header(id, parent, slot)?;
        let (new_local_chain, lca) = new.fork_choice();
        new.local_chain = new_local_chain;
        let pruned_blocks = new.update_lib();

        // On reorg, collect the reorged blocks in the old local chain.
        let reorged_blocks = if new.local_chain.id == self.local_chain.id {
            ReorgedBlocks::new()
        } else {
            ReorgedBlocks(
                self.branches
                    .walk_back_to_block(&self.local_chain, lca.id())
                    .collect(),
            )
        };

        Ok(UpdatedCryptarchia {
            cryptarchia: new,
            pruned_blocks,
            reorged_blocks,
        })
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

    /// Runs the fork choice rule to select the new local chain.
    /// Returns the new local chain and the lowest common ancestor with the
    /// previous.
    pub fn fork_choice(&self) -> (Branch<Id>, Branch<Id>) {
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
        let local_chain = self.local_chain;
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
            let lca = self.branches.lca(&local_chain, &fork);
            // If the fork is diverged deeper than `deepest_div_block`, it's prunable.
            (lca.length < deepest_div_block).then_some(ForkDivergenceInfo { tip: fork, lca })
        }))
    }

    /// Returns all the forks that are not part of the local canonical chain.
    ///
    /// The result contains both prunable and non prunable forks.
    pub fn non_canonical_forks(&self) -> impl Iterator<Item = Branch<Id>> + '_ {
        self.branches
            .branches()
            .filter(|fork_tip| fork_tip.id != self.tip())
    }

    /// Remove all blocks of a fork from `tip` to `lca`, excluding `lca`.
    fn prune_fork(&mut self, &ForkDivergenceInfo { lca, tip }: &ForkDivergenceInfo<Id>) -> Vec<Id> {
        let tip_removed = self.branches.tips.remove(&tip.id);
        if !tip_removed {
            tracing::error!(target: LOG_TARGET, "Fork tip {tip:#?} not found in the set of tips.");
        }

        let mut current_tip = tip.id;
        let mut removed_blocks = vec![];
        while current_tip != lca.id {
            let Some(branch) = self.branches.branches.remove(&current_tip) else {
                // If tip is not in branch set, it means this tip was sharing part of its
                // history with another fork that has already been removed.
                break;
            };
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
            self.branches.branches.remove(&block).map(|branch| {
                block = branch.parent;
                (branch.slot, branch.id)
            })
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

    pub fn online(self) -> (Self, PrunedBlocks<Id>) {
        let mut new = Self {
            local_chain: self.local_chain,
            branches: self.branches.clone(),
            config: self.config,
            state: State::Online,
        };
        // Update the LIB to the current local chain's tip
        let pruned_blocks = new.update_lib();
        (new, pruned_blocks)
    }
}

/// The output of applying a new block to [`Cryptarchia`]
pub struct UpdatedCryptarchia<Id: Eq + Hash> {
    /// The updated Cryptarchia instance.
    pub cryptarchia: Cryptarchia<Id>,
    /// Blocks in the forks pruned due to LIB update.
    pub pruned_blocks: PrunedBlocks<Id>,
    /// Blocks part of the previous local chain, on reorg.
    pub reorged_blocks: ReorgedBlocks<Id>,
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

    use super::{Cryptarchia, Error, Slot, maxvalid_bg};
    use crate::{Config, ReorgedBlocks, State, UpdatedCryptarchia};

    #[must_use]
    pub fn config() -> Config {
        config_with(1)
    }

    #[must_use]
    pub fn config_with(security_param: u32) -> Config {
        Config::new(
            NonZero::new(security_param).unwrap(),
            1f64,
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
            let UpdatedCryptarchia {
                cryptarchia,
                reorged_blocks,
                ..
            } = engine
                .receive_block(new_block, parent, i.into())
                .expect("test block to be applied successfully.");
            assert!(
                reorged_blocks.is_empty(),
                "no reorgs should happen in a canonical chain"
            );
            engine = cryptarchia;
            parent = new_block;
        }
        engine
    }

    #[test]
    fn test_slot_increasing() {
        // parent
        // └── child

        let mut branches = super::Branches::from_lib(hash(&0u64), 0.into(), 0);
        let parent = hash(&1u64);
        let child = hash(&2u64);

        branches = branches
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
        let (cryptarchia, pruned_blocks) = cryptarchia.online();
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
        let orig_engine = create_canonical_chain(50.try_into().unwrap(), Some(config));

        let mut engine = orig_engine.clone();
        let mut long_p = engine.tip();
        let mut short_p = engine.tip();
        // the node sees first the short chain
        for slot in 50..70 {
            let new_block = hash(&format!("short-{slot}"));
            let UpdatedCryptarchia {
                cryptarchia,
                reorged_blocks,
                ..
            } = engine
                .receive_block(new_block, short_p, slot.into())
                .unwrap();
            assert!(reorged_blocks.is_empty());
            engine = cryptarchia;
            short_p = new_block;
        }
        assert_eq!(engine.tip(), short_p);

        // then it receives a longer chain which is however less dense after the fork
        for slot in 50..70 {
            if slot % 2 == 0 {
                let new_block = hash(&format!("long-{slot}"));
                let UpdatedCryptarchia {
                    cryptarchia,
                    reorged_blocks,
                    ..
                } = engine
                    .receive_block(new_block, long_p, slot.into())
                    .unwrap();
                assert!(reorged_blocks.is_empty());
                engine = cryptarchia;
                long_p = new_block;
            }
            assert_eq!(engine.tip(), short_p);
        }
        // even if the long chain is much longer, it will never be accepted as it's not
        // dense enough
        for slot in 70..100 {
            let new_block = hash(&format!("long-{slot}"));
            let UpdatedCryptarchia {
                cryptarchia,
                reorged_blocks,
                ..
            } = engine
                .receive_block(new_block, long_p, slot.into())
                .unwrap();
            assert!(reorged_blocks.is_empty());
            engine = cryptarchia;
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
                maxvalid_bg(short_branch, engine.branches(), k, engine.config.s())
                    .0
                    .id,
                long_p
            );

            // a longer chain which is equally dense after the fork will be selected as the
            // main tip
            let mut parent = orig_engine.tip();
            for slot in 50..71 {
                let new_block = hash(&format!("long-dense-{slot}"));
                let UpdatedCryptarchia {
                    cryptarchia,
                    reorged_blocks,
                    ..
                } = engine
                    .receive_block(new_block, parent, slot.into())
                    .unwrap();

                if slot < 70 {
                    assert!(reorged_blocks.is_empty());
                } else {
                    // on the last block we trigger the reorg
                    assert_reorged_blocks(
                        &reorged_blocks,
                        &orig_engine.tip(),
                        &short_p,
                        20,
                        &cryptarchia,
                    );
                }
                engine = cryptarchia;
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

        assert_eq!(slot + 10u64, Slot::from(10));

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
        let UpdatedCryptarchia {
            cryptarchia,
            pruned_blocks,
            ..
        } = create_canonical_chain(50.try_into().unwrap(), Some(config_with(50)))
            // Add a fork from genesis block
            .receive_block(hash(&100u64), hash(&0u64), 1.into())
            .expect("test block to be applied successfully.");
        // No block was pruned during Boostrapping.
        assert!(pruned_blocks.all().next().is_none());

        // Switch to Online to update LIB and trigger pruning.
        // b0(LIB) - b1 - ... - b49
        //         \
        //           b100
        let (cryptarchia, pruned_blocks) = cryptarchia.online();
        assert_eq!(cryptarchia.lib(), hash(&0u64));

        // But, no block was pruned because `security_param` is
        // greater than local chain length.
        assert!(pruned_blocks.all().next().is_none());
        assert!(cryptarchia.branches.tips.contains(&hash(&100u64)));
        assert!(cryptarchia.branches.branches.contains_key(&hash(&100u64)));

        // Add two new blocks to the local honest chain,
        // and check if the LIB is updated and blocks are pruned.
        let UpdatedCryptarchia {
            cryptarchia,
            pruned_blocks,
            ..
        } = cryptarchia
            .receive_block(hash(&50u64), hash(&49u64), 50.into())
            .expect("test block to be applied successfully.")
            .cryptarchia
            .receive_block(hash(&51u64), hash(&50u64), 51.into())
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
        let UpdatedCryptarchia {
            cryptarchia,
            pruned_blocks,
            ..
        } = create_canonical_chain(50.try_into().unwrap(), Some(config_with(10)))
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
        let UpdatedCryptarchia {
            cryptarchia,
            pruned_blocks,
            ..
        } = create_canonical_chain(50.try_into().unwrap(), Some(config_with(10)))
            .receive_block(hash(&100u64), hash(&38u64), 39.into())
            .expect("test block to be applied successfully.")
            .cryptarchia
            .receive_block(hash(&101u64), hash(&39u64), 40.into())
            .expect("test block to be applied successfully.")
            .cryptarchia
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
        let UpdatedCryptarchia {
            cryptarchia,
            pruned_blocks,
            ..
        } = create_canonical_chain(50.try_into().unwrap(), Some(config_with(10)))
            .receive_block(hash(&100u64), hash(&38u64), 39.into())
            .expect("test block to be applied successfully.")
            .cryptarchia
            .receive_block(hash(&200u64), hash(&38u64), 39.into())
            .expect("test block to be applied successfully.")
            .cryptarchia
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
        let UpdatedCryptarchia {
            cryptarchia,
            pruned_blocks,
            ..
        } = create_canonical_chain(50.try_into().unwrap(), Some(config_with(10)))
            .receive_block(hash(&100u64), hash(&38u64), 39.into())
            .expect("test block to be applied successfully.")
            .cryptarchia
            .receive_block(hash(&101u64), hash(&100u64), 40.into())
            .expect("test block to be applied successfully.")
            .cryptarchia
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
        let (cryptarchia, pruned_blocks) =
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
        let UpdatedCryptarchia {
            cryptarchia,
            pruned_blocks,
            ..
        } = cryptarchia
            .receive_block(
                hash(&100u64),
                cryptarchia.lib(),
                cryptarchia.lib_branch().slot + 1,
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
        let UpdatedCryptarchia {
            cryptarchia,
            pruned_blocks,
            ..
        } = cryptarchia
            .receive_block(
                hash(&101u64),
                cryptarchia.tip_branch().parent,
                cryptarchia.tip_branch().slot,
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
        let UpdatedCryptarchia {
            cryptarchia,
            pruned_blocks,
            ..
        } = cryptarchia
            .receive_block(
                hash(&102u64),
                cryptarchia.tip(),
                cryptarchia.tip_branch().slot + 1,
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
