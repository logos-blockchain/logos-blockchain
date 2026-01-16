use rand::{Rng as _, RngCore};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FisherYates;

impl FisherYates {
    pub fn shuffle<R: RngCore, T>(elements: &mut [T], rng: &mut R) {
        // Implementation of fisher yates shuffling
        // https://en.wikipedia.org/wiki/Fisher%E2%80%93Yates_shuffle
        for i in (1..elements.len()).rev() {
            let j = rng.gen_range(0..=i);
            elements.swap(i, j);
        }
    }

    pub fn sample<R: RngCore, T: Ord>(
        elements: impl IntoIterator<Item = T>,
        sample_size: usize,
        rng: &mut R,
    ) -> impl Iterator<Item = T> {
        let mut elements: Vec<T> = elements.into_iter().collect();
        elements.sort();
        Self::shuffle(&mut elements, rng);

        elements.into_iter().take(sample_size)
    }
}
