use std::ops::RangeInclusive;

use lb_cryptarchia_engine::Slot;

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct BlockDensity {
    period_range: RangeInclusive<Slot>,
    density: u64,
}

impl BlockDensity {
    pub fn new(period: u64, starting_slot: Slot) -> Self {
        Self {
            period_range: starting_slot..=starting_slot + period,
            density: 0,
        }
    }

    pub fn increment_block_density(&mut self, new_slot: Slot) {
        if self.period_range.contains(&new_slot) {
            self.density += 1;
        }
    }

    pub const fn current_block_density(&self) -> u64 {
        self.density
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper method to create a BlockDensityInference with a given period
    fn create_inference(period: u64, current_slot: u64) -> BlockDensity {
        BlockDensity::new(period, Slot::from(current_slot))
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
        // 100 empty slots pushed, more than period, so block density is 0
        assert_eq!(inference.current_block_density(), 0);
    }

    #[test]
    fn test_slot_saturation_same_slot() {
        let mut inference = create_inference(5, 10);
        inference.increment_block_density(Slot::from(10));
        // saturating_sub(10, 10) = 0, so only 1 block is added
        assert_eq!(inference.current_block_density(), 1);
    }
}
