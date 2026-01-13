use async_trait::async_trait;

pub mod http;

/// Trait to support `Nomos` API requests
#[async_trait]
pub trait ApiAdapter {
    type Settings;
    type Share;
    type BlobId;
    type Commitments;
    type Membership;
    type Addressbook;

    fn new(
        settings: Self::Settings,
        membership: Self::Membership,
        addressbook: Self::Addressbook,
    ) -> Self;
}
