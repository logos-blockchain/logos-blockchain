use std::{ops::Deref, slice::Iter};

use lb_utils::bounded::{BoundedError, UpperBoundedVec};

use crate::mantle::transactions::MAX_OPS_PER_TX;

pub type TxBoundedVec<T> = UpperBoundedVec<T, MAX_OPS_PER_TX>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxList<T>(pub(crate) TxBoundedVec<T>);

impl<T> TxList<T> {
    #[must_use]
    pub const fn empty() -> Self {
        Self(TxBoundedVec::empty())
    }

    #[must_use]
    pub const fn new_unchecked(ops: Vec<T>) -> Self {
        Self(TxBoundedVec::new_unchecked(ops))
    }

    pub fn try_from_iter<I>(iterable: I) -> Result<Self, BoundedError>
    where
        I: IntoIterator<Item = T>,
    {
        TxBoundedVec::try_from_iter(iterable).map(Self)
    }

    #[must_use]
    pub const fn inner(&self) -> &TxBoundedVec<T> {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> TxBoundedVec<T> {
        self.0
    }

    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        self.0.as_slice()
    }

    pub fn try_push(&mut self, op: T) -> Result<(), BoundedError> {
        self.0.try_push(op)
    }
}

impl<T> Deref for TxList<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

// ** Conversions ** //

impl<T> TryFrom<Vec<T>> for TxList<T> {
    type Error = BoundedError;

    fn try_from(ops: Vec<T>) -> Result<Self, Self::Error> {
        TxBoundedVec::try_from(ops).map(Self)
    }
}

impl<T, const N: usize> From<[T; N]> for TxList<T> {
    fn from(ops: [T; N]) -> Self {
        Self(TxBoundedVec::from(ops))
    }
}

// ** Iteration ** //

impl<T> IntoIterator for TxList<T> {
    type Item = T;
    type IntoIter = <TxBoundedVec<T> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a TxList<T> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
