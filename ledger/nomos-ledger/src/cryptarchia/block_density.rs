use cryptarchia_engine::Slot;
use fixed_slice_deque::FixedSliceDeque;

#[derive(Clone)]
pub struct BlockDensity {
    // TODO: this can be optimized using a bitarray family data structure and shifting instead
    // current available option bitvec crate doesnt support fixed size structures so we go for this
    // instead for now.
    pub slots_window: FixedSliceDeque<bool>,
    current_slot: Slot,
}

impl BlockDensity {
    pub fn new(period: u64, current_slot: Slot) -> Self {
        let slots_window = FixedSliceDeque::new(period as usize);
        Self {
            slots_window,
            current_slot,
        }
    }

    pub fn increment_block_density(&mut self, new_slot: Slot) {
        let slot_difference = new_slot.saturating_sub(self.current_slot).into_inner();
        // fill back empty slots
        for _ in 1..slot_difference {
            self.slots_window.push_back(false);
        }
        // fill incoming slot
        self.slots_window.push_back(true);
        // set new current slot
        self.current_slot = new_slot;
    }

    pub fn current_block_density(&self) -> u64 {
        self.slots_window.iter().filter(|&&filled| filled).count() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper method to create a BlockDensityInference with a given period
    fn create_inference(period: u64, current_slot: u64) -> BlockDensity {
        BlockDensity::new(period, Slot::from(current_slot))
    }

    // Helper method to fill window with a specific number of blocks and empty slots
    fn fill_window(inference: &mut BlockDensity, blocks: &[bool]) {
        for &has_block in blocks {
            if has_block {
                inference.slots_window.push_back(true);
            } else {
                inference.slots_window.push_back(false);
            }
        }
    }

    #[test]
    fn test_initial_block_density_is_zero() {
        let inference = create_inference(10, 0);
        assert_eq!(inference.current_block_density(), 0);
    }

    #[test]
    fn test_increment_by_one_slot_with_block() {
        let mut inference = create_inference(10, 0);
        inference.increment_block_density(Slot::from(1));
        assert_eq!(inference.current_block_density(), 1);
    }

    #[test]
    fn test_increment_by_multiple_empty_slots() {
        let mut inference = create_inference(10, 0);
        inference.increment_block_density(Slot::from(5));
        // 5 empty slots (0-4) + 1 filled slot (5) = 1 block in window
        assert_eq!(inference.current_block_density(), 1);
    }

    #[test]
    fn test_increment_with_gaps_between_blocks() {
        let mut inference = create_inference(10, 0);
        inference.increment_block_density(Slot::from(2));
        assert_eq!(inference.current_block_density(), 1);
        inference.increment_block_density(Slot::from(5));
        assert_eq!(inference.current_block_density(), 2);
    }

    #[test]
    fn test_fill_entire_window_with_blocks() {
        let mut inference = create_inference(5, 0);
        inference.increment_block_density(Slot::from(1));
        inference.increment_block_density(Slot::from(2));
        inference.increment_block_density(Slot::from(3));
        inference.increment_block_density(Slot::from(4));
        inference.increment_block_density(Slot::from(5));
        assert_eq!(inference.current_block_density(), 5);
    }

    #[test]
    fn test_window_overflow_pushes_old_slots_out() {
        let mut inference = create_inference(3, 0);
        inference.increment_block_density(Slot::from(1)); // window: [false, true]
        inference.increment_block_density(Slot::from(2)); // window: [false, true, true]
        assert_eq!(inference.current_block_density(), 2);
        inference.increment_block_density(Slot::from(3)); // window: [true, true, true]
        assert_eq!(inference.current_block_density(), 3);
        inference.increment_block_density(Slot::from(4)); // window: [true, true, true], oldest pushed out
        assert_eq!(inference.current_block_density(), 3);
    }

    #[test]
    fn test_consecutive_block_increments() {
        let mut inference = create_inference(5, 0);
        for i in 1..=3 {
            inference.increment_block_density(Slot::from(i));
        }
        assert_eq!(inference.current_block_density(), 3);
    }

    #[test]
    fn test_large_slot_jump() {
        let mut inference = create_inference(5, 0);
        inference.increment_block_density(Slot::from(100));
        // 100 empty slots pushed (only last 4 remain in window) + 1 filled = 1 block
        assert_eq!(inference.current_block_density(), 1);
    }

    #[test]
    fn test_slot_saturation_same_slot() {
        let mut inference = create_inference(5, 10);
        inference.increment_block_density(Slot::from(10));
        // saturating_sub(10, 10) = 0, so only 1 block is added
        assert_eq!(inference.current_block_density(), 1);
    }
}
