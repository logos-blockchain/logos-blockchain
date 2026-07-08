use std::{net::SocketAddr, pin::Pin, time::Duration};

use common_http_client::{
    ApiBlock, BasicAuthCredentials, CommonHttpClient, Error, ProcessedBlockEvent,
};
use futures::Stream;
use lb_blend_service::message::NetworkInfo as BlendNetworkInfo;
use lb_chain_service::ChainServiceInfo;
use lb_core::{
    header::HeaderId,
    mantle::{NoteId, SignedMantleTx, TxHash},
    sdp::{Declaration, DeclarationId, Locator},
};
use lb_http_api_common::{
    bodies::{
        blend::JoinBlendRequestBody,
        mantle::GasPricesResponseBody,
        wallet::{
            balance::WalletBalanceResponseBody,
            fund::{WalletFundRequestBody, WalletFundResponseBody},
            transfer_funds::{WalletTransferFundsRequestBody, WalletTransferFundsResponseBody},
        },
    },
    paths::{
        BLEND_NETWORK_INFO, DIAL_PEER, MANTLE_METRICS, MANTLE_SDP_DECLARATIONS, MEMPOOL_VIEW,
        NETWORK_INFO,
    },
    queries::BlocksStreamQuery,
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_libp2p::{Multiaddr, PeerId};
use lb_network_service::backends::libp2p::Libp2pInfo;
use lb_tx_service::MempoolMetrics;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use tokio::time::timeout;

#[derive(Clone)]
pub struct NodeHttpClient {
    base_url: Url,
    http_client: CommonHttpClient,
    /// Default timeout for all requests
    timeout: Duration,
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
            timeout: Duration::from_secs(15),
        }
    }

    pub async fn consensus_info(&self) -> Result<ChainServiceInfo, Error> {
        self.with_timeout(
            "Consensus info request",
            self.http_client.consensus_info(self.base_url.clone()),
        )
        .await
    }

    pub async fn gas_prices(&self) -> Result<GasPricesResponseBody, Error> {
        self.with_timeout(
            "Gas prices request",
            self.http_client.gas_prices(self.base_url.clone()),
        )
        .await
    }

    pub async fn network_info(&self) -> Result<Libp2pInfo, Error> {
        self.network_info_at(self.base_url.clone()).await
    }

    pub async fn block(&self, id: &HeaderId) -> Result<Option<ApiBlock>, Error> {
        self.with_timeout(
            "Get block by ID request",
            self.http_client.get_block_by_id(self.base_url.clone(), *id),
        )
        .await
    }

    pub async fn wallet_balance(
        &self,
        zk_pk: ZkPublicKey,
        tip: Option<HeaderId>,
    ) -> Result<WalletBalanceResponseBody, Error> {
        self.with_timeout(
            "Wallet balance request",
            self.http_client
                .get_wallet_balance(self.base_url.clone(), zk_pk, tip),
        )
        .await
    }

    pub async fn blend_info(&self) -> Result<Option<BlendNetworkInfo<PeerId>>, Error> {
        let request_url = Self::join_path(&self.base_url, BLEND_NETWORK_INFO)?;

        self.with_timeout(
            "Blend info request",
            self.http_client
                .get::<(), Option<BlendNetworkInfo<PeerId>>>(request_url, None),
        )
        .await
    }

    pub async fn mantle_metrics(&self) -> Result<MempoolMetrics, Error> {
        let request_url = Self::join_path(&self.base_url, MANTLE_METRICS)?;

        self.with_timeout(
            "Mantle metrics request",
            self.http_client
                .get::<(), MempoolMetrics>(request_url, None),
        )
        .await
    }

    /// Opens a processed-block stream from the node HTTP API.
    pub async fn blocks_stream(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = ProcessedBlockEvent> + Send + '_>>, Error> {
        let stream = self
            .with_timeout(
                "Blocks stream request",
                self.http_client.get_blocks_stream(self.base_url.clone()),
            )
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
            .with_timeout(
                "Blocks range stream request",
                self.http_client
                    .get_blocks_range_stream(self.base_url.clone(), params),
            )
            .await?;

        Ok(Box::pin(stream))
    }

    pub async fn submit_transaction(&self, tx: &SignedMantleTx) -> Result<(), Error> {
        self.with_timeout(
            "Submit transaction request",
            self.http_client
                .post_transaction(self.base_url.clone(), tx.clone()),
        )
        .await
    }

    pub async fn transfer_funds(
        &self,
        body: WalletTransferFundsRequestBody,
    ) -> Result<WalletTransferFundsResponseBody, Error> {
        self.with_timeout(
            "Transfer funds request",
            self.http_client.transfer_funds(self.base_url.clone(), body),
        )
        .await
    }

    pub async fn fund_tx(
        &self,
        body: WalletFundRequestBody,
    ) -> Result<WalletFundResponseBody, Error> {
        self.with_timeout(
            "Fund transaction request",
            self.http_client.fund_tx(self.base_url.clone(), body),
        )
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

        self.with_timeout(
            "Dial peer request",
            self.http_client
                .post::<_, PeerId>(request_url, &DialPeerRequestBody { addr }),
        )
        .await
    }

    #[must_use]
    pub const fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Fetches network info from one explicit base URL.
    async fn network_info_at(&self, base_url: Url) -> Result<Libp2pInfo, Error> {
        let request_url = Self::join_path(&base_url, NETWORK_INFO)?;

        self.with_timeout(
            "Network info request",
            self.http_client.get::<(), Libp2pInfo>(request_url, None),
        )
        .await
    }

    /// Fetches testing-only SDP declarations from one explicit base URL.
    async fn get_sdp_declarations_at(&self, base_url: Url) -> Result<Vec<Declaration>, Error> {
        let request_url = Self::join_path(&base_url, MANTLE_SDP_DECLARATIONS)?;

        self.with_timeout(
            "SDP declarations request",
            self.http_client
                .get::<(), Vec<Declaration>>(request_url, None),
        )
        .await
    }

    async fn with_timeout<T, F>(&self, operation_name: &str, operation: F) -> Result<T, Error>
    where
        F: Future<Output = Result<T, Error>>,
    {
        let operation_timeout = self.timeout;
        timeout(operation_timeout, operation)
            .await
            .unwrap_or_else(|_| {
                Err(Error::Server(format!(
                    "{operation_name} timed out after {operation_timeout:?}"
                )))
            })
    }

    /// Joins one static API path against a base URL.
    fn join_path(base_url: &Url, path: &str) -> Result<Url, Error> {
        base_url
            .join(path.trim_start_matches('/'))
            .map_err(Error::Url)
    }

    pub async fn join_blend_network(
        &self,
        locator: Locator,
        locked_note_id: NoteId,
    ) -> Result<DeclarationId, Error> {
        self.http_client
            .join_blend_network(
                &self.base_url,
                JoinBlendRequestBody {
                    locator,
                    locked_note_id,
                },
            )
            .await
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DialPeerRequestBody {
    addr: Multiaddr,
}
