use lb_common_http_client::{ApiBlock, Error as HttpClientError};
use lb_core::header::HeaderId;
use lb_testing_framework::NodeHttpClient;

/// Source of blocks for wallet chain sync.
///
/// Today this is backed by node HTTP calls. Keeping the sync code behind this
/// small boundary also lets tests later feed blocks from a live block stream.
pub trait WalletChainSource {
    type Error;

    fn source_node_name(&self) -> &str;

    fn tip(&self) -> HeaderId;

    fn fetch_block(
        &mut self,
        header_id: HeaderId,
    ) -> impl Future<Output = Result<Option<ApiBlock>, Self::Error>>;
}

pub struct NodeHttpWalletChainSource {
    source_node_name: String,
    client: NodeHttpClient,
    tip: HeaderId,
}

impl NodeHttpWalletChainSource {
    pub async fn from_client(
        source_node_name: impl Into<String>,
        client: NodeHttpClient,
    ) -> Result<Self, HttpClientError> {
        let consensus = client.consensus_info().await?;

        Ok(Self {
            source_node_name: source_node_name.into(),
            client,
            tip: consensus.cryptarchia_info.tip,
        })
    }

    #[must_use]
    pub const fn from_tip(source_node_name: String, client: NodeHttpClient, tip: HeaderId) -> Self {
        Self {
            source_node_name,
            client,
            tip,
        }
    }
}

impl WalletChainSource for NodeHttpWalletChainSource {
    type Error = HttpClientError;

    fn source_node_name(&self) -> &str {
        &self.source_node_name
    }

    fn tip(&self) -> HeaderId {
        self.tip
    }

    async fn fetch_block(&mut self, header_id: HeaderId) -> Result<Option<ApiBlock>, Self::Error> {
        self.client.block(&header_id).await
    }
}
