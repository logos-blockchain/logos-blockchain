use std::collections::VecDeque;

use crate::bounded::BoundedLen;

impl<T> BoundedLen for VecDeque<T> {
    fn bounded_len(&self) -> usize {
        self.len()
    }
}
