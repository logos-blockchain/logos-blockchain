mod settings;
mod swarm;

use futures::future::{AbortHandle, Abortable};
use libp2p::PeerId;
use nomos_blend::{
    message::encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
    scheduling::membership::Membership,
};
use overwatch::overwatch::OverwatchHandle;
use rand::RngCore;
pub use settings::Libp2pBlendBackendSettings;
use swarm::BlendSwarm;
use tokio::sync::mpsc;

use super::BlendBackend;
use crate::edge::settings::BlendConfig;

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
        settings: BlendConfig<Self::Settings>,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
        membership: Membership<PeerId>,
        rng: Rng,
    ) -> Self
    where
        Rng: RngCore + Send + 'static,
    {
        let (swarm_command_sender, swarm_command_receiver) = mpsc::channel(CHANNEL_SIZE);
        let swarm = BlendSwarm::new(
            &settings,
            membership,
            rng,
            swarm_command_receiver,
            settings.backend.protocol_name.clone().into_inner(),
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
