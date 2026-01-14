use key_management_system_service::keys::Ed25519Key;
use nomos_blend::{
    message::encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
    scheduling::membership::Membership,
};
use overwatch::overwatch::handle::OverwatchHandle;
use rand::RngCore;

#[cfg(feature = "libp2p")]
pub mod libp2p;

/// A trait for blend backends that send messages to the blend network.
#[async_trait::async_trait]
pub trait BlendBackend<NodeId, RuntimeServiceId>
where
    NodeId: Clone,
{
    type Settings: Clone + Send + Sync + 'static;

    fn new<Rng>(
        settings: Self::Settings,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
        membership: Membership<NodeId>,
        rng: Rng,
        membership_key: Ed25519Key,
    ) -> Self
    where
        Rng: RngCore + Send + 'static;
    fn shutdown(self);
    /// Send a message to the blend network.
    async fn send(&self, msg: EncapsulatedMessageWithVerifiedPublicHeader);
}
