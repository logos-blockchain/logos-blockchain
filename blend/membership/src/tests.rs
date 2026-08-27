use std::collections::HashSet;

use lb_key_management_system_keys::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
use multiaddr::Multiaddr;
use rand::rngs::OsRng;

use crate::{Membership, Node};

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
