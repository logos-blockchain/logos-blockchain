use std::{marker::PhantomData, pin::Pin};

use futures::Stream;

use crate::{
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

    fn select_tx_from<'i, S>(&self, txs: S) -> Pin<Box<dyn Stream<Item = Self::Tx> + Send + 'i>>
    where
        S: Stream<Item = Self::Tx> + Send + 'i,
    {
        let stream = utils::select::select_from_till_fill_size_stream::<SIZE, Self::Tx>(
            |tx: &Self::Tx| tx.storage_size(),
            txs,
        );
        Box::pin(stream)
    }
}

#[cfg(test)]
mod tests {
    use futures::{StreamExt as _, stream};
    use lb_groth16::Fr;

    use super::*;
    use crate::mantle::{TransactionHasher, TxHash};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct TestTx {
        id: u64,
        size: usize,
    }

    impl Transaction for TestTx {
        const HASHER: TransactionHasher<Self> = |tx| TxHash(Fr::from(tx.id));
        type Hash = TxHash;

        fn as_signing_frs(&self) -> Vec<Fr> {
            vec![Fr::from(self.id)]
        }
    }

    impl StorageSize for TestTx {
        fn storage_size(&self) -> usize {
            self.size
        }
    }

    #[tokio::test]
    async fn stop_fill_size_storage_budget() {
        let selector = FillSize::<10, TestTx>::new();
        let txs = vec![
            TestTx { id: 1, size: 3 },
            TestTx { id: 2, size: 4 },
            TestTx { id: 3, size: 5 },
            TestTx { id: 4, size: 1 },
        ];

        let selected: Vec<_> = selector.select_tx_from(stream::iter(txs)).collect().await;

        assert_eq!(
            selected,
            vec![TestTx { id: 1, size: 3 }, TestTx { id: 2, size: 4 }]
        );
    }
}
