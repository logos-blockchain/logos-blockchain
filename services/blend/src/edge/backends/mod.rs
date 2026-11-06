use lb_blend::{
    message::encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
    scheduling::membership::Membership,
};
use lb_key_management_system_service::keys::UnsecuredEd25519Key;
use overwatch::overwatch::handle::OverwatchHandle;
use rand::RngCore;

pub mod libp2p;

/// A trait for blend backends that send messages to the blend network.
#[async_trait::async_trait]
pub trait BlendBackend<NodeId, RuntimeServiceId>
where
    NodeId: Clone,
{
    /// The backend's settings that are the same in every era.
    type Settings: Clone + Send + Sync + 'static;
    /// The backend's settings in one era.
    type EraSettings: Clone + Send + Sync + 'static;

    fn new<Rng>(
        settings: Self::Settings,
        era_settings: Self::EraSettings,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
        membership: Membership<NodeId>,
        rng: Rng,
        // TODO: This should go once we find a way to integrate KMS into libp2p.
        non_ephemeral_signing_key: UnsecuredEd25519Key,
    ) -> Self
    where
        Rng: RngCore + Send + 'static;
    fn shutdown(self);
    /// Send a message to the blend network.
    async fn send(&self, msg: EncapsulatedMessageWithVerifiedPublicHeader);
}
