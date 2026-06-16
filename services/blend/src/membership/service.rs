use std::hash::Hash;

use lb_blend::{
    crypto::merkle::sort_nodes_and_build_merkle_tree,
    scheduling::membership::{Membership, Node},
};
use lb_core::sdp::{ProviderId, ProviderInfo, ServiceType};
use lb_key_management_system_service::keys::{Ed25519PublicKey, ZkPublicKey};
use lb_ledger::EpochState;
use overwatch::DynError;
use tracing::{debug, warn};

use crate::membership::{MembershipInfo, ZkInfo, node_id};

/// Wrapper around [`Node`] that includes its ZK public key.
#[derive(Debug, Clone)]
struct ZkNode<NodeId> {
    pub node: Node<NodeId>,
    pub zk_key: ZkPublicKey,
}

/// Build [`MembershipInfo`] from the SDP membership snapshot frozen into
/// `epoch_state`.
#[must_use]
pub fn membership_info_from_epoch_state<NodeId>(
    epoch_state: &EpochState,
    signing_public_key: &Ed25519PublicKey,
    maybe_zk_public_key: Option<ZkPublicKey>,
) -> MembershipInfo<NodeId>
where
    NodeId: node_id::TryFrom + Clone + Hash + Eq,
{
    let mut nodes: Vec<ZkNode<NodeId>> = epoch_state
        .active_declarations
        .for_service(&ServiceType::BlendNetwork)
        .iter()
        .flat_map(|declarations| declarations.values())
        .filter_map(|declaration| {
            let provider_info = ProviderInfo {
                locators: declaration.locators.clone(),
                zk_id: declaration.zk_id,
            };
            node_from_provider::<NodeId>(&declaration.provider_id, &provider_info)
        })
        .collect();

    let zk_info = if nodes.is_empty() {
        None
    } else {
        let zk_tree = sort_nodes_and_build_merkle_tree(&mut nodes, |ZkNode { zk_key, .. }| {
            zk_key.into_inner()
        })
        .expect("Should not fail to build Merkle tree of core nodes' zk public keys.");
        let core_and_path_selectors = maybe_zk_public_key.and_then(|zk_public_key| {
            let Some(proof) = zk_tree.get_proof_for_key(zk_public_key.as_fr()) else {
                debug!(
                    "Local node's ZK public key not found in membership Merkle tree: node is not a core member."
                );
                return None;
            };
            Some(proof)
        });
        Some(ZkInfo {
            core_and_path_selectors,
            root: zk_tree.root(),
        })
    };
    let membership_nodes = nodes
        .into_iter()
        .map(|ZkNode { node, .. }| node)
        .collect::<Vec<_>>();
    let membership = Membership::new(&membership_nodes, signing_public_key);
    MembershipInfo {
        membership,
        zk: zk_info,
    }
}

/// Builds a [`ZkNode`] from a [`ProviderId`] and a set of [`Locator`]s.
/// Returns [`None`] if the locators set is empty or if the provider ID cannot
/// be decoded.
fn node_from_provider<NodeId>(
    provider_id: &ProviderId,
    ProviderInfo { locators, zk_id }: &ProviderInfo,
) -> Option<ZkNode<NodeId>>
where
    NodeId: node_id::TryFrom,
{
    let provider_id = provider_id.0.as_bytes();
    // TODO: Once we provide a proper API for non-empty vectors, we can expose a
    // `first()` method that returns `&T` instead of `Option<&T>`, and remove this
    // `expect`.
    let address = locators
        .first()
        .expect("Locators set cannot be empty")
        .clone();
    let id = NodeId::try_from_provider_id(provider_id)
        .map_err(|e| {
            warn!("Failed to decode provider_id to node ID: {e:?}");
        })
        .ok()?;
    let public_key = Ed25519PublicKey::from_bytes(provider_id)
        .map_err(|e| {
            warn!("Failed to decode provider_id to public_key: {e:?}");
        })
        .ok()?;
    Some(ZkNode {
        node: Node {
            id,
            address: address.into_inner(),
            public_key,
        },
        zk_key: *zk_id,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Other error: {0}")]
    Other(#[from] DynError),
}
