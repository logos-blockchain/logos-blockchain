use std::{hash::Hash, marker::PhantomData};

use futures::StreamExt as _;
use lb_blend::{
    crypto::merkle::sort_nodes_and_build_merkle_tree,
    scheduling::membership::{Membership, Node},
};
use lb_chain_broadcast_service::{BlockBroadcastMsg, SessionSubscription, SessionUpdate};
use lb_core::sdp::{ProviderId, ProviderInfo};
use lb_key_management_system_service::keys::{Ed25519PublicKey, ZkPublicKey};
use overwatch::{
    DynError,
    services::{ServiceData, relay::OutboundRelay},
};
use tokio::sync::oneshot;
use tracing::warn;

use crate::membership::{MembershipInfo, MembershipStream, ServiceMessage, ZkInfo, node_id};

/// Wrapper around [`Node`] that includes its ZK public key.
#[derive(Debug, Clone)]
struct ZkNode<NodeId> {
    pub node: Node<NodeId>,
    pub zk_key: ZkPublicKey,
}

pub struct Adapter<Service, NodeId>
where
    Service: ServiceData,
{
    /// A relay to send messages to the membership service.
    relay: OutboundRelay<<Service as ServiceData>::Message>,
    /// A signing public key of the local node, required to
    /// build a [`Membership`] instance.
    signing_public_key: Ed25519PublicKey,
    zk_public_key: Option<ZkPublicKey>,
    _phantom: PhantomData<NodeId>,
}

#[async_trait::async_trait]
impl<Service, NodeId> super::Adapter for Adapter<Service, NodeId>
where
    Service: ServiceData<Message = BlockBroadcastMsg>,
    NodeId: node_id::TryFrom + Clone + Hash + Eq + Sync,
{
    type Service = Service;
    type NodeId = NodeId;
    type Error = Error;

    fn new(
        relay: OutboundRelay<ServiceMessage<Self>>,
        signing_public_key: Ed25519PublicKey,
        zk_public_key: Option<ZkPublicKey>,
    ) -> Self {
        Self {
            relay,
            signing_public_key,
            zk_public_key,
            _phantom: PhantomData,
        }
    }

    /// Subscribe to membership updates.
    ///
    /// It returns a stream of [`Membership`] instances,
    async fn subscribe(&self) -> Result<MembershipStream<Self::NodeId>, Self::Error> {
        let signing_public_key = self.signing_public_key;
        let maybe_zk_public_key = self.zk_public_key;

        let session_stream = self.subscribe_stream().await?;

        Ok(Box::pin(
            session_stream
                .map(
                    |SessionUpdate {
                         providers,
                         session_number,
                     }| {
                        (
                            providers
                                .iter()
                                .filter_map(|(provider_id, provider_info)| {
                                    node_from_provider::<NodeId>(provider_id, provider_info)
                                })
                                .collect::<Vec<_>>(),
                            session_number,
                        )
                    },
                )
                // Sort nodes (if any) by their ZK public key to build a Merkle tree, since the
                // returned `HashMap` from the chain broadcast service is
                // non-deterministic across different machines.
                .map(move |(mut nodes, session_number)| {
                    let zk_info = if nodes.is_empty() {
                        None
                    } else {
                        let zk_tree = sort_nodes_and_build_merkle_tree(
                            &mut nodes,
                            |ZkNode { zk_key, .. }| zk_key.into_inner(),
                        )
                        .expect(
                            "Should not fail to build Merkle tree of core nodes' zk public keys.",
                        );
                        let core_and_path_selectors = maybe_zk_public_key.map(|zk_public_key| {
                            zk_tree.get_proof_for_key(zk_public_key.as_fr()).expect(
                                "Zk public key of core node should be part of membership info.",
                            )
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
                    let membership = Membership::new(&membership_nodes, &signing_public_key);
                    MembershipInfo {
                        membership,
                        zk: zk_info,
                        session_number,
                    }
                }),
        ))
    }
}

impl<Service, NodeId> Adapter<Service, NodeId>
where
    Service: ServiceData<Message = BlockBroadcastMsg>,
    NodeId: Sync,
{
    /// Subscribe to membership updates for the given service type.
    async fn subscribe_stream(&self) -> Result<SessionSubscription, Error> {
        let (sender, receiver) = oneshot::channel();

        self.relay
            .send(BlockBroadcastMsg::SubscribeBlendSession {
                result_sender: sender,
            })
            .await
            .map_err(|(e, _)| Error::Other(e.into()))?;

        receiver.await.map_err(|e| Error::Other(e.into()))
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
    let address = locators.first()?.clone();
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
