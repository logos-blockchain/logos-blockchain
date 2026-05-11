use std::{net::SocketAddr, num::NonZero, pin::Pin};

use common_http_client::{
    ApiBlock, BasicAuthCredentials, CommonHttpClient, Error, ProcessedBlockEvent,
};
use futures::Stream;
use lb_blend_service::message::NetworkInfo as BlendNetworkInfo;
use lb_chain_service::ChainServiceInfo;
use lb_core::{header::HeaderId, mantle::SignedMantleTx, sdp::Declaration};
use lb_http_api_common::{
    bodies::wallet::transfer_funds::{
        WalletTransferFundsRequestBody, WalletTransferFundsResponseBody,
    },
    paths::{BLEND_NETWORK_INFO, DIAL_PEER, MANTLE_METRICS, MANTLE_SDP_DECLARATIONS, NETWORK_INFO},
};
use lb_libp2p::{Multiaddr, PeerId};
use lb_network_service::backends::libp2p::Libp2pInfo;
use lb_tx_service::MempoolMetrics;
use reqwest::Url;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct NodeHttpClient {
    base_url: Url,
    testing_url: Option<Url>,
    http_client: CommonHttpClient,
}

impl NodeHttpClient {
    #[must_use]
    pub fn new(base_addr: SocketAddr, testing_addr: Option<SocketAddr>) -> Self {
        let base_url = Url::parse(&format!("http://{base_addr}"))
            .expect("SocketAddr should always render as a valid URL host:port");
        let testing_url = testing_addr.map(|addr| {
            Url::parse(&format!("http://{addr}"))
                .expect("SocketAddr should always render as a valid URL host:port")
        });

        Self::from_urls(base_url, testing_url)
    }

    #[must_use]
    pub fn from_urls(base_url: Url, testing_url: Option<Url>) -> Self {
        Self::from_urls_with_basic_auth(base_url, testing_url, None)
    }

    #[must_use]
    pub fn from_urls_with_basic_auth(
        base_url: Url,
        testing_url: Option<Url>,
        basic_auth: Option<BasicAuthCredentials>,
    ) -> Self {
        Self {
            base_url,
            testing_url,
            http_client: CommonHttpClient::new(basic_auth),
        }
    }

    pub async fn consensus_info(&self) -> Result<ChainServiceInfo, Error> {
        self.http_client.consensus_info(self.base_url.clone()).await
    }

    pub async fn network_info(&self) -> Result<Libp2pInfo, Error> {
        match self.network_info_at(self.base_url.clone()).await {
            Ok(info) => Ok(info),
            Err(base_err) => {
                if let Some(testing_url) = self.testing_url.clone() {
                    self.network_info_at(testing_url)
                        .await
                        .map_err(|_| base_err)
                } else {
                    Err(base_err)
                }
            }
        }
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
        blocks_limit: Option<NonZero<usize>>,
        slot_from: Option<u64>,
        slot_to: Option<u64>,
        descending: Option<bool>,
        server_batch_size: Option<NonZero<usize>>,
        immutable_only: Option<bool>,
    ) -> Result<Pin<Box<dyn Stream<Item = ProcessedBlockEvent> + Send + '_>>, Error> {
        let stream = self
            .http_client
            .get_blocks_range_stream(
                self.base_url.clone(),
                blocks_limit,
                slot_from,
                slot_to,
                descending,
                server_batch_size,
                immutable_only,
            )
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
        if let Some(testing_url) = self.testing_url.clone()
            && let Ok(declarations) = self.get_sdp_declarations_at(testing_url).await
        {
            return Ok(declarations);
        }

        self.get_sdp_declarations_at(self.base_url.clone()).await
    }

    pub async fn dial_peer(&self, addr: Multiaddr) -> Result<PeerId, Error> {
        let testing_url = self
            .testing_url
            .clone()
            .ok_or_else(|| Error::Client("testing api unavailable".to_owned()))?;
        let request_url = Self::join_path(&testing_url, DIAL_PEER)?;

        self.http_client
            .post::<_, PeerId>(request_url, &DialPeerRequestBody { addr })
            .await
    }

    #[must_use]
    pub const fn base_url(&self) -> &Url {
        &self.base_url
    }

    #[must_use]
    pub const fn testing_url(&self) -> Option<&Url> {
        self.testing_url.as_ref()
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
