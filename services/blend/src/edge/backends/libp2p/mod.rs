mod settings;
mod swarm;

use futures::future::{AbortHandle, Abortable};
use lb_blend::{
    message::encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
    scheduling::membership::Membership,
};
use lb_key_management_system_service::keys::UnsecuredEd25519Key;
use lb_libp2p::ed25519::Keypair;
use libp2p::PeerId;
use overwatch::overwatch::OverwatchHandle;
use rand::RngCore;
pub use settings::Libp2pBlendBackendSettings;
use swarm::BlendSwarm;
use tokio::sync::mpsc;

use super::BlendBackend;

const LOG_TARGET: &str = "blend::service::edge::backend::libp2p";

#[cfg(test)]
mod tests;

pub struct Libp2pBlendBackend {
    swarm_task_abort_handle: AbortHandle,
    swarm_command_sender: mpsc::Sender<swarm::Command>,
}

const CHANNEL_SIZE: usize = 64;

#[async_trait::async_trait]
impl<RuntimeServiceId> BlendBackend<PeerId, RuntimeServiceId> for Libp2pBlendBackend {
    type Settings = Libp2pBlendBackendSettings;

    fn new<Rng>(
        settings: Self::Settings,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
        membership: Membership<PeerId>,
        rng: Rng,
        non_ephemeral_signing_key: UnsecuredEd25519Key,
    ) -> Self
    where
        Rng: RngCore + Send + 'static,
    {
        let (swarm_command_sender, swarm_command_receiver) = mpsc::channel(CHANNEL_SIZE);
        let swarm_identity = {
            let mut non_ephemeral_signing_key_bytes = non_ephemeral_signing_key.to_bytes();
            Keypair::try_from_bytes(&mut non_ephemeral_signing_key_bytes[..])
                .expect("Cryptographic secret key should be a valid Ed25519 private key.")
        };
        let swarm = BlendSwarm::new(
            settings,
            membership,
            rng,
            swarm_command_receiver,
            swarm_identity.into(),
        );

        let (swarm_task_abort_handle, swarm_task_abort_registration) = AbortHandle::new_pair();
        overwatch_handle
            .runtime()
            .spawn(Abortable::new(swarm.run(), swarm_task_abort_registration));

        Self {
            swarm_task_abort_handle,
            swarm_command_sender,
        }
    }

    fn shutdown(self) {
        drop(self);
    }

    async fn send(&self, msg: EncapsulatedMessageWithVerifiedPublicHeader) {
        if let Err(e) = self
            .swarm_command_sender
            .send(swarm::Command::SendMessage(msg))
            .await
        {
            tracing::error!(target: LOG_TARGET, "Failed to send command to Swarm: {e}");
        }
    }
}

impl Drop for Libp2pBlendBackend {
    fn drop(&mut self) {
        self.swarm_task_abort_handle.abort();
    }
}
