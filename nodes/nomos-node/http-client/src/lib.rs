use std::{collections::HashSet, fmt::Debug, hash::Hash, sync::Arc};

use broadcast_service::BlockInfo;
use chain_service::CryptarchiaInfo;
use futures::{Stream, StreamExt as _};
use nomos_core::{block::Block, da::blob::Share, header::HeaderId, mantle::SignedMantleTx};
use nomos_da_messages::http::da::{
    DASharesCommitmentsRequest, DaSamplingRequest, GetSharesRequest,
};
use nomos_http_api_common::paths::{
    CRYPTARCHIA_INFO, CRYPTARCHIA_LIB_STREAM, DA_GET_LIGHT_SHARE, DA_GET_SHARES,
    DA_GET_STORAGE_SHARES_COMMITMENTS, MEMPOOL_ADD_TX, STORAGE_BLOCK,
};
use reqwest::{Client, ClientBuilder, RequestBuilder, StatusCode, Url};
use serde::{Serialize, de::DeserializeOwned};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Internal server error: {0}")]
    Server(String),
    #[error("Failed to execute request: {0}")]
    Client(String),
    #[error(transparent)]
    Request(#[from] reqwest::Error),
    #[error(transparent)]
    Url(#[from] url::ParseError),
}

#[derive(Clone)]
pub struct BasicAuthCredentials {
    username: String,
    password: Option<String>,
}

impl BasicAuthCredentials {
    #[must_use]
    pub const fn new(username: String, password: Option<String>) -> Self {
        Self { username, password }
    }
}

#[derive(Clone)]
pub struct CommonHttpClient {
    client: Arc<Client>,
    basic_auth: Option<BasicAuthCredentials>,
}

impl CommonHttpClient {
    #[must_use]
    pub fn new(basic_auth: Option<BasicAuthCredentials>) -> Self {
        let client = ClientBuilder::new()
            .build()
            .expect("Client from default settings should be able to build");
        Self {
            client: Arc::new(client),
            basic_auth,
        }
    }

    #[must_use]
    pub fn new_with_client(client: Client, basic_auth: Option<BasicAuthCredentials>) -> Self {
        Self {
            client: Arc::new(client),
            basic_auth,
        }
    }

    pub async fn post<Req, Res>(&self, request_url: Url, request_body: &Req) -> Result<Res, Error>
    where
        Req: Serialize + ?Sized + Send + Sync,
        Res: DeserializeOwned + Send + Sync,
    {
        let request = self.client.post(request_url).json(request_body);
        self.execute_request::<Res>(request).await
    }

    pub async fn get<Req, Res>(
        &self,
        request_url: Url,
        request_body: Option<&Req>,
    ) -> Result<Res, Error>
    where
        Req: Serialize + ?Sized + Send + Sync,
        Res: DeserializeOwned + Send + Sync,
    {
        let mut request = self.client.get(request_url);
        if let Some(request_body) = request_body {
            request = request.json(request_body);
        }
        self.execute_request::<Res>(request).await
    }

    async fn execute_request<Res: DeserializeOwned>(
        &self,
        mut request: RequestBuilder,
    ) -> Result<Res, Error> {
        if let Some(basic_auth) = &self.basic_auth {
            request = request.basic_auth(&basic_auth.username, basic_auth.password.as_deref());
        }

        let response = request.send().await.map_err(Error::Request)?;
        let status = response.status();
        let body = response.text().await.map_err(Error::Request)?;

        match status {
            StatusCode::OK => serde_json::from_str(&body)
                .map_err(|e| Error::Server(format!("Failed to parse response: {e}"))),
            StatusCode::INTERNAL_SERVER_ERROR => Err(Error::Server(body)),
            _ => Err(Error::Server(format!(
                "Unexpected response [{status}]: {body}",
            ))),
        }
    }

    pub async fn get_lib_stream(
        &self,
        base_url: Url,
    ) -> Result<impl Stream<Item = BlockInfo>, Error> {
        let request_url = base_url
            .join(CRYPTARCHIA_LIB_STREAM.trim_start_matches('/'))
            .map_err(Error::Url)?;
        let mut request = self.client.get(request_url);

        if let Some(basic_auth) = &self.basic_auth {
            request = request.basic_auth(&basic_auth.username, basic_auth.password.as_deref());
        }

        let response = request.send().await.map_err(Error::Request)?;
        let status = response.status();

        let lib_stream = response.bytes_stream().filter_map(async |item| {
            let bytes = item.ok()?;
            serde_json::from_slice::<BlockInfo>(&bytes).ok()
        });
        match status {
            StatusCode::OK => Ok(lib_stream),
            StatusCode::INTERNAL_SERVER_ERROR => Err(Error::Server("Error".to_owned())),
            _ => Err(Error::Server(format!("Unexpected response [{status}]",))),
        }
    }

    pub async fn get_block_by_id<HeaderId>(
        &self,
        base_url: Url,
        header_id: HeaderId,
    ) -> Result<Option<Block<SignedMantleTx>>, Error>
    where
        HeaderId: Serialize + Send + Sync,
    {
        let request_url = base_url
            .join(STORAGE_BLOCK.trim_start_matches('/'))
            .map_err(Error::Url)?;
        self.post(request_url, &header_id).await
    }

    /// Get the commitments for a Blob
    pub async fn get_storage_commitments<S>(
        &self,
        base_url: Url,
        blob_id: S::BlobId,
    ) -> Result<Option<S::SharesCommitments>, Error>
    where
        S: Share,
        S::SharesCommitments: DeserializeOwned + Send + Sync,
        <S as Share>::BlobId: Serialize + Send + Sync,
    {
        let request: DASharesCommitmentsRequest<S> = DASharesCommitmentsRequest { blob_id };
        let path = DA_GET_STORAGE_SHARES_COMMITMENTS.trim_start_matches('/');
        let request_url = base_url.join(path).map_err(Error::Url)?;
        self.get(request_url, Some(&request)).await
    }

    /// Get blob by blob id and column index
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
        let request: DaSamplingRequest<S> = DaSamplingRequest { blob_id, share_idx };
        let path = DA_GET_LIGHT_SHARE.trim_start_matches('/');
        let request_url = base_url.join(path).map_err(Error::Url)?;
        self.get(request_url, Some(&request)).await
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
        let request: GetSharesRequest<B> = GetSharesRequest {
            blob_id,
            requested_shares,
            filter_shares,
            return_available,
        };
        let request_url = base_url
            .join(DA_GET_SHARES.trim_start_matches('/'))
            .map_err(Error::Url)?;
        let mut request = self.client.get(request_url).json(&request);

        if let Some(basic_auth) = &self.basic_auth {
            request = request.basic_auth(&basic_auth.username, basic_auth.password.as_deref());
        }

        let response = request.send().await.map_err(Error::Request)?;
        let status = response.status();

        let shares_stream = response.bytes_stream().filter_map(async |item| {
            let bytes = item.ok()?;
            serde_json::from_slice::<B::LightShare>(&bytes).ok()
        });
        match status {
            StatusCode::OK => Ok(shares_stream),
            StatusCode::INTERNAL_SERVER_ERROR => Err(Error::Server("Error".to_owned())),
            _ => Err(Error::Server(format!("Unexpected response [{status}]",))),
        }
    }

    pub async fn post_transaction<Tx>(&self, base_url: Url, transaction: Tx) -> Result<(), Error>
    where
        Tx: Serialize + Send + Sync + 'static,
    {
        let request_url = base_url
            .join(MEMPOOL_ADD_TX.trim_start_matches('/'))
            .map_err(Error::Url)?;
        self.post(request_url, &transaction).await
    }

    /// Get consensus info (tip, height, etc.)
    pub async fn consensus_info(&self, base_url: Url) -> Result<CryptarchiaInfo, Error> {
        let request_url = base_url
            .join(CRYPTARCHIA_INFO.trim_start_matches('/'))
            .map_err(Error::Url)?;
        self.get::<(), CryptarchiaInfo>(request_url, None).await
    }

    /// Get a block by its header ID
    pub async fn get_block(
        &self,
        base_url: Url,
        header_id: HeaderId,
    ) -> Result<Option<Block<SignedMantleTx>>, Error> {
        let request_url = base_url
            .join(STORAGE_BLOCK.trim_start_matches('/'))
            .map_err(Error::Url)?;
        self.post(request_url, &header_id).await
    }
}
