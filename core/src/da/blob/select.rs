use std::marker::PhantomData;

use crate::{
    da::blob::{BlobSelect, info::DispersedBlobInfo},
    utils,
};

#[derive(Default, Clone, Copy)]
pub struct FillSize<const SIZE: usize, B> {
    _blob: PhantomData<B>,
}

impl<const SIZE: usize, B> FillSize<SIZE, B> {
    #[must_use]
    pub const fn new() -> Self {
        Self { _blob: PhantomData }
    }
}

impl<const SIZE: usize, B: DispersedBlobInfo> BlobSelect for FillSize<SIZE, B> {
    type BlobId = B;

    type Settings = ();

    fn new(_settings: Self::Settings) -> Self {
        Self::new()
    }

    fn select_blob_from<'i, I: Iterator<Item = Self::BlobId> + 'i>(
        &self,
        certificates: I,
    ) -> impl Iterator<Item = Self::BlobId> + 'i {
        #[expect(
            clippy::redundant_closure_for_method_calls,
            reason = "TODO: Replace this redundant closure with `B::size` without triggering compiler errors about B not living long enough."
        )]
        {
            utils::select::select_from_till_fill_size::<SIZE, Self::BlobId>(
                |c| c.size(),
                certificates,
            )
        }
    }
}
