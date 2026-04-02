use std::sync::Arc;

use futures::{Stream, StreamExt as _};
pub use lb_chain_broadcast_service::BlockInfo;
use lb_chain_service::CryptarchiaInfo;
pub use lb_chain_service::Slot;
use lb_core::{
    block::Block,
    header::{ContentId, HeaderId},
    mantle::SignedMantleTx,
    proofs::leader_proof::Groth16LeaderProof,
};
use lb_groth16::fr_to_bytes;
use lb_http_api_common::{
    bodies::wallet::{
        balance::WalletBalanceResponseBody,
        transfer_funds::{WalletTransferFundsRequestBody, WalletTransferFundsResponseBody},
    },
    paths::{
        BLOCKS, BLOCKS_STREAM, CRYPTARCHIA_INFO, CRYPTARCHIA_LIB_STREAM, MEMPOOL_ADD_TX,
        STORAGE_BLOCK,
        wallet::{BALANCE, TRANSACTIONS_TRANSFER_FUNDS},
    },
    settings::default_max_body_size,
};
use lb_key_management_system_keys::keys::ZkPublicKey;
use reqwest::{Client, ClientBuilder, RequestBuilder, StatusCode, Url};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Client-side header representation matching the server's
/// `ApiHeaderSerializer`.
#[derive(Clone, Debug, Deserialize)]
pub struct ApiHeader {
    pub id: HeaderId,
    pub parent_block: HeaderId,
    pub slot: Slot,
    pub block_root: ContentId,
    pub proof_of_leadership: Groth16LeaderProof,
}

/// Client-side block representation matching the server's `ApiBlockSerializer`.
/// Note: The server omits the signature field.
#[derive(Clone, Debug, Deserialize)]
pub struct ApiBlock {
    pub header: ApiHeader,
    pub transactions: Vec<SignedMantleTx>,
}

/// Processed block event from the blocks stream.
/// Matches the server's `ApiProcessedBlockEvent`.
#[derive(Clone, Debug, Deserialize)]
pub struct ProcessedBlockEvent {
    pub block: ApiBlock,
    pub tip: HeaderId,
    pub lib: HeaderId,
}

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
        let initial_stream_window_size: u32 =
            u32::try_from(6 * default_max_body_size() / 10).unwrap_or(4 * 1025);
        let client = ClientBuilder::new()
            .http2_initial_stream_window_size(initial_stream_window_size)
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
            StatusCode::OK | StatusCode::CREATED => serde_json::from_str(&body)
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

    /// Get blocks in a slot range.
    pub async fn get_blocks(
        &self,
        base_url: Url,
        slot_from: u64,
        slot_to: u64,
    ) -> Result<Vec<ApiBlock>, Error> {
        let mut request_url = base_url
            .join(BLOCKS.trim_start_matches('/'))
            .map_err(Error::Url)?;
        request_url
            .query_pairs_mut()
            .append_pair("slot_from", &slot_from.to_string())
            .append_pair("slot_to", &slot_to.to_string());
        self.get::<(), Vec<ApiBlock>>(request_url, None).await
    }

    /// Subscribe to the processed blocks stream.
    /// Each event contains the block, current tip, and current LIB.
    pub async fn get_blocks_stream(
        &self,
        base_url: Url,
    ) -> Result<impl Stream<Item = ProcessedBlockEvent> + use<>, Error> {
        let request_url = base_url
            .join(BLOCKS_STREAM.trim_start_matches('/'))
            .map_err(Error::Url)?;
        let mut request = self.client.get(request_url);

        if let Some(basic_auth) = &self.basic_auth {
            request = request.basic_auth(&basic_auth.username, basic_auth.password.as_deref());
        }

        let response = request.send().await.map_err(Error::Request)?;
        let status = response.status();

        let blocks_stream = response.bytes_stream().filter_map(async |item| {
            let bytes = item.ok()?;
            serde_json::from_slice::<ProcessedBlockEvent>(&bytes).ok()
        });
        match status {
            StatusCode::OK => Ok(blocks_stream),
            StatusCode::INTERNAL_SERVER_ERROR => Err(Error::Server("Error".to_owned())),
            _ => Err(Error::Server(format!("Unexpected response [{status}]",))),
        }
    }

    /// Get the balance for a specific `ZkPublicKey`.
    pub async fn get_wallet_balance(
        &self,
        base_url: Url,
        zk_pk: ZkPublicKey,
        tip: Option<HeaderId>,
    ) -> Result<WalletBalanceResponseBody, Error> {
        let key_id = hex::encode(fr_to_bytes(zk_pk.as_fr()));
        let mut request_url = base_url
            .join(&BALANCE.replace(":public_key", &key_id))
            .map_err(Error::Url)?;

        if let Some(t) = tip {
            request_url
                .query_pairs_mut()
                .append_pair("tip", &t.to_string());
        }

        self.get::<(), WalletBalanceResponseBody>(request_url, None)
            .await
    }

    /// Post a request to transfer funds.
    pub async fn transfer_funds(
        &self,
        base_url: Url,
        body: WalletTransferFundsRequestBody,
    ) -> Result<WalletTransferFundsResponseBody, Error> {
        let request_url = base_url
            .join(TRANSACTIONS_TRANSFER_FUNDS.trim_start_matches('/'))
            .map_err(Error::Url)?;

        self.post(request_url, &body).await
    }
}
