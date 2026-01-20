use async_trait::async_trait;
use overwatch::DynError;
use tokio::sync::oneshot;

pub mod http;

/// Trait to support `Logos Blockchain` API requests
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
    async fn request_commitments(
        &self,
        request: Self::BlobId,
        reply_channel: oneshot::Sender<Option<Self::Commitments>>,
    ) -> Result<(), DynError>;
}
