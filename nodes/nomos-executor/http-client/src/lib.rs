use std::{collections::HashSet, hash::Hash};

pub use common_http_client::{BasicAuthCredentials, CommonHttpClient, Error};
use futures::Stream;
use nomos_core::{
    da::{BlobId, blob::Share},
    mantle::ops::channel::{ChannelId, Ed25519PublicKey, MsgId},
};
use nomos_http_api_common::{bodies::dispersal::DispersalRequestBody, paths};
use reqwest::Url;
use serde::{Serialize, de::DeserializeOwned};

#[derive(Clone)]
pub struct ExecutorHttpClient {
    client: CommonHttpClient,
}

impl ExecutorHttpClient {
    #[must_use]
    pub fn new(basic_auth: Option<BasicAuthCredentials>) -> Self {
        Self {
            client: CommonHttpClient::new(basic_auth),
        }
    }

    /// Send a `Blob` to be dispersed
    pub async fn publish_blob(
        &self,
        base_url: Url,
        channel_id: ChannelId,
        parent_msg_id: MsgId,
        signer: Ed25519PublicKey,
        data: Vec<u8>,
    ) -> Result<BlobId, Error> {
        let req = DispersalRequestBody {
            channel_id,
            parent_msg_id,
            signer,
            data,
        };
        let path = paths::DISPERSE_DATA.trim_start_matches('/');
        let request_url = base_url.join(path).map_err(Error::Url)?;

        self.client
            .post::<DispersalRequestBody, BlobId>(request_url, &req)
            .await
    }

    /// Get the commitments for a specific `BlobId`
    pub async fn get_commitments<S>(
        &self,
        base_url: Url,
        blob_id: S::BlobId,
    ) -> Result<Option<S::SharesCommitments>, Error>
    where
        S: Share + Send,
        <S as Share>::BlobId: Serialize + Send + Sync,
        <S as Share>::SharesCommitments: DeserializeOwned + Send + Sync,
    {
        self.client
            .get_storage_commitments::<S>(base_url, blob_id)
            .await
    }

    /// Get share by blob id and share index
    pub async fn get_share<S, C>(
        &self,
        base_url: Url,
        blob_id: S::BlobId,
        share_idx: S::ShareIndex,
    ) -> Result<Option<C>, Error>
    where
        C: DeserializeOwned + Send + Sync,
        S: Share + DeserializeOwned + Send + Sync,
        <S as Share>::BlobId: Serialize + Send + Sync,
        <S as Share>::ShareIndex: Serialize + Send + Sync,
    {
        self.client
            .get_share::<S, C>(base_url, blob_id, share_idx)
            .await
    }

    pub async fn get_shares<B>(
        &self,
        base_url: Url,
        blob_id: B::BlobId,
        requested_shares: HashSet<B::ShareIndex>,
        filter_shares: HashSet<B::ShareIndex>,
        return_available: bool,
    ) -> Result<impl Stream<Item = B::LightShare>, Error>
    where
        B: Share,
        <B as Share>::BlobId: Serialize + Send + Sync,
        <B as Share>::ShareIndex: Serialize + DeserializeOwned + Eq + Hash + Send + Sync,
        <B as Share>::LightShare: DeserializeOwned + Send + Sync,
    {
        self.client
            .get_shares::<B>(
                base_url,
                blob_id,
                requested_shares,
                filter_shares,
                return_available,
            )
            .await
    }
}
