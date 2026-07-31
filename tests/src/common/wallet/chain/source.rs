use lb_common_http_client::{ApiBlock, Error as HttpClientError};
use lb_core::header::HeaderId;
use lb_testing_framework::NodeHttpClient;
use tokio::task::JoinSet;

/// Source of blocks for building wallet chain state outside the feed.
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
    fallback: Option<(String, NodeHttpClient)>,
    tip: HeaderId,
}

impl NodeHttpWalletChainSource {
    #[must_use]
    pub const fn from_tip(source_node_name: String, client: NodeHttpClient, tip: HeaderId) -> Self {
        Self {
            source_node_name,
            client,
            fallback: None,
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
        let Some((_, fallback_client)) = &self.fallback else {
            return self.client.block(&header_id).await;
        };

        let mut jobs = JoinSet::new();
        let primary_client = self.client.clone();
        let fallback_client = fallback_client.clone();

        jobs.spawn(async move { primary_client.block(&header_id).await });
        jobs.spawn(async move { fallback_client.block(&header_id).await });

        let mut first_error = None;
        while let Some(result) = jobs.join_next().await {
            match result {
                Ok(Ok(block)) => return Ok(block),
                Ok(Err(error)) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
                Err(_) => {}
            }
        }

        if let Some(error) = first_error {
            return Err(error);
        }

        self.client.block(&header_id).await
    }
}
