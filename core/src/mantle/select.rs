use std::{marker::PhantomData, pin::Pin};

use futures::Stream;

use crate::{
    mantle::{Transaction, TxSelect},
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

impl<const SIZE: usize, Tx: Transaction + Send> TxSelect for FillSize<SIZE, Tx> {
    type Tx = Tx;
    type Settings = ();

    fn new((): Self::Settings) -> Self {
        Self::new()
    }

    fn select_tx_from<'i, S>(&self, txs: S) -> Pin<Box<dyn Stream<Item = Self::Tx> + Send + 'i>>
    where
        S: Stream<Item = Self::Tx> + Send + 'i,
    {
        let stream = utils::select::select_from_till_fill_size_stream::<SIZE, Self::Tx>(|_| 1, txs);
        Box::pin(stream)
    }
}
