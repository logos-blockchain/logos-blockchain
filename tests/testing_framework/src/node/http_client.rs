use std::{net::SocketAddr, pin::Pin};

use common_http_client::{BasicAuthCredentials, CommonHttpClient, Error, ProcessedBlockEvent};
use futures::Stream;
use lb_chain_service::CryptarchiaInfo;
use lb_core::{block::Block, header::HeaderId, mantle::SignedMantleTx};
use lb_http_api_common::{
    bodies::wallet::transfer_funds::{
        WalletTransferFundsRequestBody, WalletTransferFundsResponseBody,
    },
    paths::NETWORK_INFO,
};
use lb_network_service::backends::libp2p::Libp2pInfo;
use reqwest::Url;

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

    pub async fn consensus_info(&self) -> Result<CryptarchiaInfo, Error> {
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

    pub async fn block(&self, id: &HeaderId) -> Result<Option<Block<SignedMantleTx>>, Error> {
        self.http_client
            .get_block_by_id(self.base_url.clone(), *id)
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

    #[must_use]
    pub const fn base_url(&self) -> &Url {
        &self.base_url
    }

    #[must_use]
    pub const fn testing_url(&self) -> Option<&Url> {
        self.testing_url.as_ref()
    }

    async fn network_info_at(&self, base_url: Url) -> Result<Libp2pInfo, Error> {
        let request_url = base_url
            .join(NETWORK_INFO.trim_start_matches('/'))
            .map_err(Error::Url)?;
        self.http_client
            .get::<(), Libp2pInfo>(request_url, None)
            .await
    }
}
