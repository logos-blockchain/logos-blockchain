use std::{fmt::Debug, pin::Pin};

use futures::Stream;
use lb_blend::{
    message::encap::validated::{
        EncapsulatedMessageWithVerifiedPublicHeader, EncapsulatedMessageWithVerifiedSignature,
    },
    scheduling::membership::Membership,
};
use lb_chain_service::Epoch;
use overwatch::overwatch::handle::OverwatchHandle;

use crate::{core::settings::RunningBlendConfig as BlendConfig, message::NetworkInfo};

pub mod libp2p;

pub type BackendEpochInfo<NodeId> = (Membership<NodeId>, Epoch);

/// A trait for blend backends that send messages to the blend network.
#[async_trait::async_trait]
pub trait BlendBackend<NodeId, Rng, RuntimeServiceId> {
    type Settings: Clone + Debug + Send + Sync + 'static;

    fn new(
        service_config: BlendConfig<Self::Settings>,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
        current_epoch_info: BackendEpochInfo<NodeId>,
        rng: Rng,
    ) -> Self;
    fn shutdown(self);
    /// Publish a message to the blend network.
    async fn publish(
        &self,
        msg: EncapsulatedMessageWithVerifiedPublicHeader,
        intended_epoch: Epoch,
    );
    /// Rotate epoch.
    async fn rotate_epoch(&mut self, new_epoch_info: BackendEpochInfo<NodeId>);
    /// Complete the epoch transition.
    async fn complete_epoch_transition(&mut self);
    /// Listen to messages received from the blend network.
    fn listen_to_incoming_messages(
        &mut self,
    ) -> Pin<Box<dyn Stream<Item = (EncapsulatedMessageWithVerifiedSignature, Epoch)> + Send>>;

    /// Return network info about the current blend peers.
    /// Returns `None` if the backend does not support this operation.
    async fn network_info(&self) -> Option<NetworkInfo<NodeId>>;
}
