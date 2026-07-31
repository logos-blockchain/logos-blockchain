use std::hash::Hash;

pub type Hasher<T> = fn(&T) -> <T as Hashable>::Hash;

pub trait Hashable {
    const HASHER: Hasher<Self>;
    type Hash: Hash + Eq + Clone;

    fn hash(&self) -> Self::Hash {
        Self::HASHER(self)
    }

    /// Returns the bytes that are used to form a signature of a transaction.
    ///
    /// The resulting bytes are then used by the `HASHER` to produce the
    /// transaction's unique hash, which is what is typically signed by the
    /// transaction originator.
    fn as_signing(&self) -> Vec<u8>;
}

impl<T: Hashable> Hashable for &T {
    //noinspection RsTypeCheck: The type is correct, but the linter is confused by
    // the closure.
    const HASHER: Hasher<Self> = |tx| T::HASHER(tx);
    type Hash = T::Hash;

    fn as_signing(&self) -> Vec<u8> {
        T::as_signing(self)
    }
}
