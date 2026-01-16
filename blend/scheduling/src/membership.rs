use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
};

use key_management_system_keys::keys::Ed25519PublicKey;
use multiaddr::Multiaddr;
use rand::{Rng, seq::IteratorRandom as _};
use serde::{Deserialize, Serialize};

/// A set of core nodes in a session.
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
        use key_management_system_keys::keys::ED25519_PUBLIC_KEY_SIZE;

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
}

#[cfg(test)]
mod tests {
    use key_management_system_keys::keys::UnsecuredEd25519Key;
    use rand::rngs::OsRng;

    use super::*;

    #[test]
    fn test_membership_new_with_local_node() {
        let nodes = vec![node(1, 1), node(2, 2), node(3, 3)];
        let local_key = key(2);

        let membership = Membership::new(&nodes, &local_key);

        assert_eq!(membership.size(), 3);
        assert_eq!(
            membership
                .core_nodes
                .keys()
                .copied()
                .collect::<HashSet<_>>(),
            HashSet::from([1, 2, 3])
        );
        assert_eq!(membership.node_indices, vec![1, 2, 3]);
        assert_eq!(membership.local_node_index, Some(1));
        assert!(membership.contains_local());
    }

    #[test]
    fn test_membership_new_without_local_node() {
        let nodes = vec![node(1, 1), node(2, 2), node(3, 3)];
        let local_key = key(99);

        let membership = Membership::new(&nodes, &local_key);

        assert_eq!(membership.size(), 3);
        assert_eq!(
            membership
                .core_nodes
                .keys()
                .copied()
                .collect::<HashSet<_>>(),
            HashSet::from([1, 2, 3])
        );
        assert_eq!(membership.node_indices, vec![1, 2, 3]);
        assert!(membership.local_node_index.is_none());
        assert!(!membership.contains_local());
    }

    #[test]
    fn test_membership_new_empty() {
        let local_key = key(99);

        let membership = Membership::<u32>::new(&[], &local_key);

        assert_eq!(membership.size(), 0);
        assert!(membership.core_nodes.keys().next().is_none());
        assert!(membership.node_indices.is_empty());
        assert!(membership.local_node_index.is_none());
        assert!(!membership.contains_local());
    }

    #[test]
    fn test_choose_remote_nodes() {
        let nodes = vec![node(1, 1), node(2, 2), node(3, 3), node(4, 4)];
        let local_key = key(99);
        let membership = Membership::new(&nodes, &local_key);

        let chosen: HashSet<_> = membership
            .choose_remote_nodes(&mut OsRng, 2)
            .map(|node| node.id)
            .collect();
        assert_eq!(chosen.len(), 2);
    }

    #[test]
    fn test_choose_remote_nodes_more_than_available() {
        let nodes = vec![node(1, 1), node(2, 2)];
        let local_key = key(99);
        let membership = Membership::new(&nodes, &local_key);

        let chosen: HashSet<_> = membership
            .choose_remote_nodes(&mut OsRng, 5)
            .map(|node| node.id)
            .collect();
        assert_eq!(chosen.len(), 2);
    }

    #[test]
    fn test_choose_remote_nodes_zero() {
        let nodes = vec![node(1, 1), node(2, 2)];
        let local_key = key(99);
        let membership = Membership::new(&nodes, &local_key);

        let mut rng = OsRng;
        let mut chosen = membership.choose_remote_nodes(&mut rng, 0);
        assert!(chosen.next().is_none());
    }

    #[test]
    fn test_filter_and_choose_remote_nodes() {
        let nodes = vec![node(1, 1), node(2, 2), node(3, 3)];
        let local_key = key(99);
        let membership = Membership::new(&nodes, &local_key);
        let exclude_peers = HashSet::from([3]);

        let chosen: HashSet<_> = membership
            .filter_and_choose_remote_nodes(&mut OsRng, 2, &exclude_peers)
            .map(|node| node.id)
            .collect();
        assert_eq!(chosen.len(), 2);
    }

    #[test]
    fn test_filter_and_choose_remote_nodes_all_excluded() {
        let nodes = vec![node(1, 1), node(2, 2)];
        let local_key = key(99);
        let membership = Membership::new(&nodes, &local_key);
        let exclude_peers = HashSet::from([1, 2]);

        let chosen: HashSet<_> = membership
            .filter_and_choose_remote_nodes(&mut OsRng, 2, &exclude_peers)
            .map(|node| node.id)
            .collect();
        assert!(chosen.is_empty());
    }

    #[test]
    fn test_contains() {
        let nodes = vec![node(1, 1)];
        let local_key = key(99);
        let membership = Membership::new(&nodes, &local_key);

        assert!(membership.contains(&1));
        assert!(!membership.contains(&2));
    }

    #[test]
    fn get_node_at() {
        let nodes = vec![node(1, 1)];
        let local_key = key(99);
        let membership = Membership::new(&nodes, &local_key);

        assert_eq!(membership.get_node_at(0), Some(&node(1, 1)));
        assert_eq!(membership.get_node_at(1), None);
    }

    #[test]
    #[should_panic(expected = "Membership info contained a duplicate node.")]
    fn duplicate_remote_node() {
        let nodes = vec![node(1, 1), node(1, 2)];
        let local_key = key(99);
        drop(Membership::new(&nodes, &local_key));
    }

    fn key(seed: u8) -> Ed25519PublicKey {
        UnsecuredEd25519Key::from_bytes(&[seed; 32]).public_key()
    }

    fn node(id: u32, seed: u8) -> Node<u32> {
        Node {
            id,
            address: Multiaddr::empty(),
            public_key: key(seed),
        }
    }
}
