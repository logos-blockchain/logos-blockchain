use std::{net::SocketAddr, pin::Pin};

use common_http_client::{
    ApiBlock, BasicAuthCredentials, CommonHttpClient, Error, ProcessedBlockEvent,
};
use futures::Stream;
use lb_blend_service::message::NetworkInfo as BlendNetworkInfo;
use lb_chain_service::ChainServiceInfo;
use lb_core::{
    header::HeaderId,
    mantle::{SignedMantleTx, TxHash},
    sdp::Declaration,
};
use lb_http_api_common::{
    bodies::wallet::transfer_funds::{
        WalletTransferFundsRequestBody, WalletTransferFundsResponseBody,
    },
    paths::{
        BLEND_NETWORK_INFO, DIAL_PEER, MANTLE_METRICS, MANTLE_SDP_DECLARATIONS, MEMPOOL_VIEW,
        NETWORK_INFO,
    },
    queries::BlocksStreamQuery,
};
use lb_libp2p::{Multiaddr, PeerId};
use lb_network_service::backends::libp2p::Libp2pInfo;
use lb_tx_service::MempoolMetrics;
use reqwest::Url;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct NodeHttpClient {
    base_url: Url,
    http_client: CommonHttpClient,
}

impl NodeHttpClient {
    #[must_use]
    pub fn new(base_addr: SocketAddr) -> Self {
        let base_url = Url::parse(&format!("http://{base_addr}"))
            .expect("SocketAddr should always render as a valid URL host:port");

        Self::from_url(base_url)
    }

    #[must_use]
    pub fn from_url(base_url: Url) -> Self {
        Self::from_url_with_basic_auth(base_url, None)
    }

    #[must_use]
    pub fn from_url_with_basic_auth(
        base_url: Url,
        basic_auth: Option<BasicAuthCredentials>,
    ) -> Self {
        Self {
            base_url,
            http_client: CommonHttpClient::new(basic_auth),
        }
    }

    pub async fn consensus_info(&self) -> Result<ChainServiceInfo, Error> {
        self.http_client.consensus_info(self.base_url.clone()).await
    }

    pub async fn network_info(&self) -> Result<Libp2pInfo, Error> {
        self.network_info_at(self.base_url.clone()).await
    }

    pub async fn block(&self, id: &HeaderId) -> Result<Option<ApiBlock>, Error> {
        self.http_client
            .get_block_by_id(self.base_url.clone(), *id)
            .await
    }

    pub async fn blend_info(&self) -> Result<Option<BlendNetworkInfo<PeerId>>, Error> {
        let request_url = Self::join_path(&self.base_url, BLEND_NETWORK_INFO)?;

        self.http_client
            .get::<(), Option<BlendNetworkInfo<PeerId>>>(request_url, None)
            .await
    }

    pub async fn mantle_metrics(&self) -> Result<MempoolMetrics, Error> {
        let request_url = Self::join_path(&self.base_url, MANTLE_METRICS)?;

        self.http_client
            .get::<(), MempoolMetrics>(request_url, None)
            .await
    }

    /// Opens a processed-block stream from the node HTTP API.
    pub async fn blocks_stream(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = ProcessedBlockEvent> + Send + '_>>, Error> {
        let stream = self
            .http_client
            .get_blocks_stream(self.base_url.clone())
            .await?;
        Ok(Box::pin(stream))
    }

    /// Opens a processed-block stream from the node HTTP API with a limited
    /// range.
    pub async fn blocks_range_stream(
        &self,
        params: BlocksStreamQuery,
    ) -> Result<Pin<Box<dyn Stream<Item = ProcessedBlockEvent> + Send + '_>>, Error> {
        let stream = self
            .http_client
            .get_blocks_range_stream(self.base_url.clone(), params)
            .await?;
        Ok(Box::pin(stream))
    }

    pub async fn submit_transaction(&self, tx: &SignedMantleTx) -> Result<(), Error> {
        self.http_client
            .post_transaction(self.base_url.clone(), tx.clone())
            .await
    }

    pub async fn transfer_funds(
        &self,
        body: WalletTransferFundsRequestBody,
    ) -> Result<WalletTransferFundsResponseBody, Error> {
        self.http_client
            .transfer_funds(self.base_url.clone(), body)
            .await
    }

    pub async fn get_sdp_declarations(&self) -> Result<Vec<Declaration>, Error> {
        self.get_sdp_declarations_at(self.base_url.clone()).await
    }

    pub async fn test_mempool_view(&self) -> Result<Vec<TxHash>, Error> {
        let request_url = Self::join_path(&self.base_url, MEMPOOL_VIEW)?;

        self.http_client
            .get::<(), Vec<TxHash>>(request_url, None)
            .await
    }

    pub async fn dial_peer(&self, addr: Multiaddr) -> Result<PeerId, Error> {
        let request_url = Self::join_path(&self.base_url, DIAL_PEER)?;

        self.http_client
            .post::<_, PeerId>(request_url, &DialPeerRequestBody { addr })
            .await
    }

    #[must_use]
    pub const fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Fetches network info from one explicit base URL.
    async fn network_info_at(&self, base_url: Url) -> Result<Libp2pInfo, Error> {
        let request_url = Self::join_path(&base_url, NETWORK_INFO)?;

        self.http_client
            .get::<(), Libp2pInfo>(request_url, None)
            .await
    }

    /// Fetches testing-only SDP declarations from one explicit base URL.
    async fn get_sdp_declarations_at(&self, base_url: Url) -> Result<Vec<Declaration>, Error> {
        let request_url = Self::join_path(&base_url, MANTLE_SDP_DECLARATIONS)?;

        self.http_client
            .get::<(), Vec<Declaration>>(request_url, None)
            .await
    }

    /// Joins one static API path against a base URL.
    fn join_path(base_url: &Url, path: &str) -> Result<Url, Error> {
        base_url
            .join(path.trim_start_matches('/'))
            .map_err(Error::Url)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DialPeerRequestBody {
    addr: Multiaddr,
}
