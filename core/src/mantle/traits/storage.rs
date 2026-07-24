pub trait StorageSize {
    fn storage_size(&self) -> usize;
}

impl<T: StorageSize> StorageSize for &T {
    fn storage_size(&self) -> usize {
        T::storage_size(self)
    }
}
