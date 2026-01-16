pub mod info;
pub mod metadata;
pub mod select;

pub trait LightShare {
    type ShareIndex;
    fn share_idx(&self) -> Self::ShareIndex;
}

pub trait Share {
    type BlobId;
    type ShareIndex;
    type LightShare;
    type SharesCommitments;

    fn blob_id(&self) -> Self::BlobId;
    fn share_idx(&self) -> Self::ShareIndex;
    fn into_share_and_commitments(self) -> (Self::LightShare, Self::SharesCommitments);

    fn from_share_and_commitments(
        light_blob: Self::LightShare,
        shared_commitments: Self::SharesCommitments,
    ) -> Self;
}

pub trait BlobSelect {
    type BlobId: info::DispersedBlobInfo;
    type Settings: Clone;

    fn new(settings: Self::Settings) -> Self;
    fn select_blob_from<'i, I: Iterator<Item = Self::BlobId> + 'i>(
        &self,
        certificates: I,
    ) -> impl Iterator<Item = Self::BlobId> + 'i;
}
