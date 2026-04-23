use async_trait::async_trait;
use testing_framework_core::{
    observation::{BoxedSourceProvider, ObservationRuntime, ObservedSource, StaticSourceProvider},
    scenario::{
        Application, DynError, NodeClients, PreparedRuntimeExtension, RuntimeExtensionFactory,
    },
};

use super::{BlockFeed, BlockFeedObserver};
use crate::{framework::LbcEnv, node::NodeHttpClient};

/// Builds the fixed source provider used by scenario-based Logos tests.
pub fn block_feed_source_provider(
    _deployment: &<LbcEnv as Application>::Deployment,
    node_clients: &NodeClients<LbcEnv>,
) -> Result<BoxedSourceProvider<NodeHttpClient>, DynError> {
    Ok(Box::new(StaticSourceProvider::new(block_feed_sources(
        node_clients.snapshot(),
    ))))
}

/// Runtime extension factory that starts the Logos block feed for one scenario
/// run.
#[derive(Clone, Debug, Default)]
pub struct BlockFeedExtensionFactory;

#[async_trait]
impl RuntimeExtensionFactory<LbcEnv> for BlockFeedExtensionFactory {
    async fn prepare(
        &self,
        deployment: &<LbcEnv as Application>::Deployment,
        node_clients: NodeClients<LbcEnv>,
    ) -> Result<PreparedRuntimeExtension, DynError> {
        let provider = block_feed_source_provider(deployment, &node_clients)?;
        let runtime =
            ObservationRuntime::start(provider, BlockFeedObserver, BlockFeedObserver::config())
                .await?;
        let (handle, task) = runtime.into_parts();

        Ok(PreparedRuntimeExtension::from_task(
            BlockFeed::new(handle),
            task,
        ))
    }
}

/// Builds named sources from a client list using each client's base URL.
#[must_use]
pub fn block_feed_sources(clients: Vec<NodeHttpClient>) -> Vec<ObservedSource<NodeHttpClient>> {
    named_block_feed_sources(clients.into_iter().map(|client| {
        let name = client.base_url().to_string();
        (name, client)
    }))
}

/// Builds named sources from logical source names and node clients.
#[must_use]
pub fn named_block_feed_sources(
    named_clients: impl IntoIterator<Item = (String, NodeHttpClient)>,
) -> Vec<ObservedSource<NodeHttpClient>> {
    named_clients
        .into_iter()
        .map(|(name, client)| ObservedSource::new(&name, client))
        .collect()
}
