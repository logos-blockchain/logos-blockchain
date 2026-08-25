use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
};

use lb_key_management_system_keys::keys::Ed25519PublicKey;
use multiaddr::Multiaddr;
use rand::{Rng, seq::IteratorRandom as _};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

/// A set of core nodes in an epoch.
#[derive(Clone, Debug)]
pub struct Membership<NodeId> {
    /// All nodes, including local and remote.
    core_nodes: HashMap<NodeId, Node<NodeId>>,
    /// List of node indices, used for proof of selection generation. It
    /// contains all nodes in the `nodes` map.
    node_indices: Vec<NodeId>,
    /// ID of the local node in the `node_indices` vector, if present (i.e., if
    /// the local node is a core node).
    local_node_index: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Node<Id> {
    /// An unique identifier of the node,
    /// which is usually corresponding to the network node identifier
    /// but depending on the network backend.
    pub id: Id,
    /// A listening address
    pub address: Multiaddr,
    /// A public key used for the blend message encryption
    pub public_key: Ed25519PublicKey,
}

impl<NodeId> Membership<NodeId>
where
    NodeId: Clone + Hash + Eq,
{
    #[must_use]
    pub fn new(nodes: &[Node<NodeId>], local_public_key: &Ed25519PublicKey) -> Self {
        let mut core_nodes = HashMap::with_capacity(nodes.len());
        let mut node_indices = Vec::with_capacity(nodes.len());
        let mut local_node_index = None;
        for (index, node) in nodes.iter().enumerate() {
            assert!(
                core_nodes.insert(node.id.clone(), node.clone()).is_none(),
                "Membership info contained a duplicate node."
            );
            node_indices.push(node.id.clone());
            if node.public_key == *local_public_key {
                local_node_index = Some(index);
            }
        }

        Self {
            core_nodes,
            node_indices,
            local_node_index,
        }
    }

    #[cfg(any(test, feature = "unsafe-test-functions"))]
    #[must_use]
    pub fn new_without_local(nodes: &[Node<NodeId>]) -> Self {
        use lb_key_management_system_keys::keys::ED25519_PUBLIC_KEY_SIZE;

        Self::new(
            nodes,
            &Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE]).unwrap(),
        )
    }
}

impl<NodeId> Membership<NodeId>
where
    NodeId: Eq + Hash,
{
    /// Choose `amount` random remote nodes.
    pub fn choose_remote_nodes<R: Rng>(
        &self,
        rng: &mut R,
        amount: usize,
    ) -> impl Iterator<Item = &Node<NodeId>> + use<'_, R, NodeId> {
        self.filter_and_choose_remote_nodes(rng, amount, &HashSet::new())
    }

    /// Choose `amount` random remote nodes excluding the given set of node IDs.
    pub fn filter_and_choose_remote_nodes<R: Rng>(
        &self,
        rng: &mut R,
        amount: usize,
        exclude_peers: &HashSet<NodeId>,
    ) -> impl Iterator<Item = &Node<NodeId>> + use<'_, R, NodeId> {
        self.node_indices
            .iter()
            .enumerate()
            // Filter out excluded peers.
            .filter(|(_, node_id)| !exclude_peers.contains(node_id))
            // Filter out local node, if the local node is a core node.
            .filter(|(index, _)| self.local_node_index != Some(*index))
            // Discard index after it's used.
            .map(|(_, node)| node)
            .choose_multiple(rng, amount)
            .into_iter()
            .map(|id| {
                self.core_nodes
                    .get(id)
                    .expect("Node ID must exist in core nodes.")
            })
    }

    pub fn contains(&self, node_id: &NodeId) -> bool {
        self.core_nodes.contains_key(node_id)
    }

    #[must_use]
    pub fn get_node_at(&self, index: usize) -> Option<&Node<NodeId>> {
        self.core_nodes.get(self.node_indices.get(index)?)
    }
}

impl<NodeId> Membership<NodeId> {
    #[must_use]
    pub const fn local_index(&self) -> Option<usize> {
        self.local_node_index
    }

    #[must_use]
    pub const fn contains_local(&self) -> bool {
        self.local_node_index.is_some()
    }

    /// Returns the number of all nodes, including local and remote.
    #[must_use]
    pub fn size(&self) -> usize {
        self.core_nodes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.core_nodes.is_empty()
    }
}
