use testing_framework_core::observation::ObservedSource;

use crate::node::NodeHttpClient;

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
