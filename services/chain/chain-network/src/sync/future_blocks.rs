use std::{
    collections::{BTreeMap, HashSet},
    mem,
};

use lb_core::header::HeaderId;
use lb_cryptarchia_engine::Slot;

/// How many slots ahead of the local clock a block may be and still be kept
/// for later, to absorb clock skew between honest nodes. Anything further
/// ahead is dropped without being looked at.
pub const MAX_FUTURE_SLOTS: u64 = 2;

/// Default number of future blocks kept at once.
pub const DEFAULT_FUTURE_BLOCKS_CAPACITY: usize = 32;

/// Returns `true` if `block_slot` is further ahead of `current_slot` than the
/// clock-skew window allows.
pub const fn is_too_far_in_future(block_slot: Slot, current_slot: Slot) -> bool {
    block_slot.into_inner() > current_slot.into_inner().saturating_add(MAX_FUTURE_SLOTS)
}

#[derive(Debug, PartialEq, Eq)]
pub enum NotParked {
    TooFarInFuture,
    AlreadyParked,
    Full,
}

/// Bounded buffer for blocks whose slot the local clock has not reached yet.
///
/// Blocks are handed back by [`Self::take_due`] once a slot tick reaches their
/// slot, so that the event loop never waits on the clock. When full, a block
/// nearer in time evicts the furthest one.
pub struct FutureBlocks<Block> {
    by_slot: BTreeMap<Slot, Vec<(HeaderId, Block)>>,
    ids: HashSet<HeaderId>,
    capacity: usize,
}

impl<Block> FutureBlocks<Block> {
    pub fn new(capacity: usize) -> Self {
        Self {
            by_slot: BTreeMap::new(),
            ids: HashSet::new(),
            capacity,
        }
    }

    pub fn park(
        &mut self,
        block_id: HeaderId,
        block_slot: Slot,
        current_slot: Slot,
        block: Block,
    ) -> Result<(), NotParked> {
        if is_too_far_in_future(block_slot, current_slot) {
            return Err(NotParked::TooFarInFuture);
        }
        if self.ids.contains(&block_id) {
            return Err(NotParked::AlreadyParked);
        }
        if self.ids.len() >= self.capacity && !self.evict_latest_after(block_slot) {
            return Err(NotParked::Full);
        }

        self.ids.insert(block_id);
        self.by_slot
            .entry(block_slot)
            .or_default()
            .push((block_id, block));
        Ok(())
    }

    /// Removes and returns every block whose slot is at or before
    /// `current_slot`, oldest slot first.
    pub fn take_due(&mut self, current_slot: Slot) -> Vec<Block> {
        let later = current_slot
            .into_inner()
            .checked_add(1)
            .map_or_else(BTreeMap::new, |next| {
                self.by_slot.split_off(&Slot::new(next))
            });
        let due = mem::replace(&mut self.by_slot, later);

        due.into_values()
            .flatten()
            .map(|(block_id, block)| {
                self.ids.remove(&block_id);
                block
            })
            .collect()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Evicts one block from the latest slot if that slot is after `slot`.
    fn evict_latest_after(&mut self, slot: Slot) -> bool {
        let Some(mut latest) = self.by_slot.last_entry() else {
            return false;
        };
        if *latest.key() <= slot {
            return false;
        }
        if let Some((evicted, _)) = latest.get_mut().pop() {
            self.ids.remove(&evicted);
        }
        if latest.get().is_empty() {
            latest.remove();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> HeaderId {
        HeaderId::from([byte; 32])
    }

    #[test]
    fn blocks_beyond_the_skew_window_are_not_parked() {
        let mut blocks = FutureBlocks::new(4);
        let now = Slot::new(10);

        assert_eq!(
            blocks.park(id(1), Slot::new(10 + MAX_FUTURE_SLOTS + 1), now, ()),
            Err(NotParked::TooFarInFuture)
        );
        assert_eq!(
            blocks.park(id(2), Slot::new(1_000_010), now, ()),
            Err(NotParked::TooFarInFuture)
        );
        assert_eq!(blocks.len(), 0);
    }

    #[test]
    fn the_same_block_is_parked_once() {
        let mut blocks = FutureBlocks::new(4);
        let now = Slot::new(10);

        assert_eq!(blocks.park(id(1), Slot::new(11), now, ()), Ok(()));
        assert_eq!(
            blocks.park(id(1), Slot::new(11), now, ()),
            Err(NotParked::AlreadyParked)
        );
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn blocks_are_handed_back_once_their_slot_is_reached() {
        let mut blocks = FutureBlocks::new(4);
        let now = Slot::new(10);
        blocks.park(id(1), Slot::new(12), now, 12).unwrap();
        blocks.park(id(2), Slot::new(11), now, 11).unwrap();

        assert_eq!(blocks.take_due(now), Vec::<i32>::new());
        assert_eq!(blocks.take_due(Slot::new(11)), vec![11]);
        assert_eq!(blocks.take_due(Slot::new(13)), vec![12]);
        assert_eq!(blocks.len(), 0);

        // A taken block may be parked again.
        assert_eq!(blocks.park(id(1), Slot::new(14), Slot::new(13), 14), Ok(()));
    }

    #[test]
    fn a_full_buffer_keeps_the_nearest_blocks() {
        let mut blocks = FutureBlocks::new(2);
        let now = Slot::new(10);
        blocks.park(id(1), Slot::new(12), now, 12).unwrap();
        blocks.park(id(2), Slot::new(12), now, 12).unwrap();

        // Not nearer than anything kept.
        assert_eq!(
            blocks.park(id(3), Slot::new(12), now, 12),
            Err(NotParked::Full)
        );
        // Nearer: evicts one of the furthest.
        assert_eq!(blocks.park(id(4), Slot::new(11), now, 11), Ok(()));

        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks.take_due(Slot::new(12)), vec![11, 12]);
    }

    #[test]
    fn a_zero_capacity_buffer_parks_nothing() {
        let mut blocks = FutureBlocks::new(0);

        assert_eq!(
            blocks.park(id(1), Slot::new(11), Slot::new(10), ()),
            Err(NotParked::Full)
        );
    }
}
