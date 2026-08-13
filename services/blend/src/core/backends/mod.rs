use std::{fmt::Debug, pin::Pin};

use futures::Stream;
use lb_blend::{
    message::encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
    scheduling::membership::Membership,
};
use lb_chain_service::Epoch;
use overwatch::overwatch::handle::OverwatchHandle;

use crate::{core::settings::RunningBlendConfig as BlendConfig, message::NetworkInfo};

pub mod libp2p;

/// Everything the backend needs to know about an epoch.
///
/// The `PoQ` verifier travels with the membership and the epoch number because
/// the backend verifies the public header of every message it receives — `PoQ`
/// included — before relaying it to the rest of the network. Its inputs are
/// epoch-specific, so a new verifier is handed over on every rotation, and the
/// previous one is kept until [`BlendBackend::complete_epoch_transition`] to
/// serve the messages still arriving for the old epoch.
#[derive(Debug, Clone)]
pub struct BackendEpochInfo<NodeId, ProofsVerifier> {
    pub membership: Membership<NodeId>,
    pub epoch: Epoch,
    pub proofs_verifier: ProofsVerifier,
}

/// A trait for blend backends that send messages to the blend network.
#[async_trait::async_trait]
pub trait BlendBackend<NodeId, Rng, ProofsVerifier, RuntimeServiceId> {
    type Settings: Clone + Debug + Send + Sync + 'static;

    fn new(
        service_config: BlendConfig<Self::Settings>,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
        current_epoch_info: BackendEpochInfo<NodeId, ProofsVerifier>,
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
    async fn rotate_epoch(&mut self, new_epoch_info: BackendEpochInfo<NodeId, ProofsVerifier>);
    /// Complete the epoch transition, dropping the old epoch's state and its
    /// `PoQ` verifier.
    async fn complete_epoch_transition(&mut self);
    /// Listen to messages received from the blend network, along with the epoch
    /// each of them belongs to.
    ///
    /// The backend yields only messages whose whole public header it has
    /// verified, so the service does not have to verify the `PoQ` again.
    fn listen_to_incoming_messages(
        &mut self,
    ) -> Pin<Box<dyn Stream<Item = (EncapsulatedMessageWithVerifiedPublicHeader, Epoch)> + Send>>;

    /// Return network info about the current blend peers.
    /// Returns `None` if the backend does not support this operation.
    async fn network_info(&self) -> Option<NetworkInfo<NodeId>>;
}
