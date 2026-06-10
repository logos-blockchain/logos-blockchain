use std::{marker::PhantomData, pin::Pin};

use futures::{Stream, StreamExt as _};

use crate::{
    block::MAX_BLOCK_TRANSACTIONS,
    mantle::{StorageSize, Transaction, TxSelect},
    utils,
};

#[derive(Default, Clone, Copy)]
pub struct FillSize<const SIZE: usize, Tx> {
    _tx: PhantomData<Tx>,
}

impl<const SIZE: usize, Tx> FillSize<SIZE, Tx> {
    #[must_use]
    pub const fn new() -> Self {
        Self { _tx: PhantomData }
    }
}

impl<const SIZE: usize, Tx: Transaction + StorageSize + Send> TxSelect for FillSize<SIZE, Tx> {
    type Tx = Tx;
    type Settings = ();

    fn new((): Self::Settings) -> Self {
        Self::new()
    }

    /// Selects a *prefix* of `txs` that fits within both the byte budget `SIZE`
    /// and [`MAX_BLOCK_TRANSACTIONS`].
    ///
    /// Selection stops at the first transaction that would trip either limit
    /// rather than skipping it to fit a later, smaller one. Callers feed
    /// transactions in dependency order, so preserving the prefix avoids
    /// dropping a transaction that a later one depends on.
    fn select_tx_from<'i, S>(&self, txs: S) -> Pin<Box<dyn Stream<Item = Self::Tx> + Send + 'i>>
    where
        S: Stream<Item = Self::Tx> + Send + 'i,
    {
        let stream = utils::select::select_from_till_fill_size_stream::<SIZE, Self::Tx>(
            |tx: &Self::Tx| tx.storage_size(),
            txs.take(MAX_BLOCK_TRANSACTIONS),
        );
        Box::pin(stream)
    }
}

#[cfg(test)]
mod tests {
    use futures::stream;

    use super::*;
    use crate::{block::MAX_BLOCK_SIZE, mantle::TransactionHasher};

    #[derive(Clone, Copy, Debug)]
    struct TestTx {
        size: usize,
    }

    impl Transaction for TestTx {
        const HASHER: TransactionHasher<Self> = |tx| tx.size;
        type Hash = usize;

        fn as_signing(&self) -> Vec<u8> {
            Vec::new()
        }
    }

    impl StorageSize for TestTx {
        fn storage_size(&self) -> usize {
            self.size
        }
    }

    async fn select<const SIZE: usize>(txs: Vec<TestTx>) -> Vec<TestTx> {
        FillSize::<SIZE, TestTx>::new()
            .select_tx_from(stream::iter(txs))
            .collect()
            .await
    }

    #[tokio::test]
    async fn respects_transaction_count_limit() {
        let txs = vec![TestTx { size: 1 }; MAX_BLOCK_TRANSACTIONS + 1];

        let selected = select::<MAX_BLOCK_SIZE>(txs).await;

        assert_eq!(selected.len(), MAX_BLOCK_TRANSACTIONS);
    }

    #[tokio::test]
    async fn respects_block_size_limit() {
        let txs = vec![
            TestTx {
                size: MAX_BLOCK_SIZE / 2,
            },
            TestTx {
                size: MAX_BLOCK_SIZE / 2,
            },
            TestTx { size: 1 },
        ];

        let selected = select::<MAX_BLOCK_SIZE>(txs).await;
        let selected_size: usize = selected.iter().map(StorageSize::storage_size).sum();

        assert_eq!(selected.len(), 2);
        assert_eq!(selected_size, MAX_BLOCK_SIZE);
    }

    #[tokio::test]
    async fn stops_at_first_transaction_that_does_not_fit() {
        // The middle transaction does not fit alongside the first, so selection
        // must stop there and must not pull the third (which would fit on its
        // own) ahead of it — doing so could drop a dependency of the third.
        let txs = vec![
            TestTx { size: 10 },
            TestTx {
                size: MAX_BLOCK_SIZE,
            },
            TestTx { size: 10 },
        ];

        let selected = select::<MAX_BLOCK_SIZE>(txs).await;

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].storage_size(), 10);
    }

    #[tokio::test]
    async fn stops_at_leading_oversized_transaction() {
        // A transaction larger than the whole block can never fit. Selection
        // stops at it rather than skipping past to later transactions, which may
        // depend on it. (In practice such transactions are filtered out before
        // reaching here, but the prefix invariant must hold regardless.)
        let txs = vec![
            TestTx {
                size: MAX_BLOCK_SIZE + 1,
            },
            TestTx { size: 1 },
        ];

        let selected = select::<MAX_BLOCK_SIZE>(txs).await;

        assert!(selected.is_empty());
    }
}
