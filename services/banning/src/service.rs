use std::{
    fmt::{Debug, Display},
    time::SystemTime,
};

use async_trait::async_trait;
use overwatch::{
    OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, ServiceState},
    },
};
use tokio::sync::broadcast;
use tracing::{debug, info};

use crate::{
    config::BanningConfig,
    store::BanStore,
    types::{BanStatus, BanningEvent, BanningRequest},
};

// This is sufficiently large so no banning events are dropped in the
// subscribers for realistic scenarios.
const BROADCAST_CHANNEL_SIZE: usize = 2000;

/// The banning service.
pub struct BanningService<RuntimeServiceId> {
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    state: BanningState,
}

impl<RuntimeServiceId> ServiceData for BanningService<RuntimeServiceId> {
    type Settings = BanningConfig;
    type State = BanningState;
    type StateOperator = NoOperator<Self::State>;
    type Message = BanningRequest;
}

/// The banning service state.
#[derive(Debug, Clone)]
pub struct BanningState {
    pub(crate) store: BanStore,
    pub(crate) events: broadcast::Sender<BanningEvent>,
}

impl ServiceState for BanningState {
    type Settings = BanningConfig;
    type Error = overwatch::DynError;

    fn from_settings(settings: &Self::Settings) -> Result<Self, Self::Error> {
        let store = BanStore::new(settings);
        let events = broadcast::channel(BROADCAST_CHANNEL_SIZE).0;
        Ok(Self { store, events })
    }
}

#[async_trait]
impl<RuntimeServiceId> ServiceCore<RuntimeServiceId> for BanningService<RuntimeServiceId>
where
    RuntimeServiceId: AsServiceId<Self> + Clone + Display + Send + Sync + 'static + Debug,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        initial_state: Self::State,
    ) -> Result<Self, overwatch::DynError> {
        info!(
            "[Banning service] Service '{}' is initialized.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );
        Ok(Self {
            service_resources_handle,
            state: initial_state,
        })
    }

    async fn run(mut self) -> Result<(), overwatch::DynError> {
        self.service_resources_handle.status_updater.notify_ready();
        info!(
            "[Banning service] Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        let mut ticker = tokio::time::interval(self.state.store.get_ban_expiry_check_interval());
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let unbanned = self.state.store.update_ban_state();
                    for peer_id in unbanned {
                        let _unused = self.state.events.send(BanningEvent::Unbanned { peer_id });
                    }
                }

                inbound = self.service_resources_handle.inbound_relay.recv() => {
                    if let Some(msg) = inbound {
                        match msg {
                            BanningRequest::BanPeer { violation, reply } => {
                                let ban_result = self.state.store.ban_peer(
                                    violation.peer_id,
                                    violation.offense_kind,
                                    violation.context.clone(),
                                );
                                if let BanStatus::Banned {
                                    banned_until,
                                    offense,
                                    context,
                                } = ban_result.clone()
                                {
                                    let _unused = self.state.events.send(BanningEvent::Banned {
                                        peer_id: violation.peer_id,
                                        banned_until,
                                        offense,
                                        context: context.clone(),
                                    });
                                    debug!(
                                        "[Banning service][ReportViolation] Peer '{}' banned for '{:?}' seconds due to '{:?}'",
                                        violation.peer_id,
                                        banned_until.duration_since(SystemTime::now()).unwrap_or_default().as_secs(),
                                        offense
                                    );
                                } else {
                                    debug!(
                                        "[Banning service][ReportViolation] Peer '{}' cannot be banned",
                                        violation.peer_id,
                                    );
                                }
                                if let Some(sender) = reply {
                                    let _unused = sender.send(ban_result);
                                }
                            }

                            BanningRequest::GetBanState { peer_id, reply } => {
                                let state = self.state.store.get_ban_state(&peer_id);
                                let _unused = reply.send(state);
                            }

                            BanningRequest::UnbanPeer { peer_id , reply} => {
                                if self.state.store.unban_peer(&peer_id) {
                                    let _unused = self.state.events.send(BanningEvent::Unbanned { peer_id });
                                    let _unused = reply.send(true);
                                    debug!(
                                        "[Banning service][Unban] Peer '{}' unbanned",
                                        peer_id,
                                    );
                                } else {
                                    let _unused = reply.send(false);
                                    debug!(
                                        "[Banning service][Unban] Peer '{}' was not banned",
                                        peer_id,
                                    );
                                }
                            }

                            BanningRequest::ListActiveBans { reply } => {
                                let active = self.state.store.get_active_bans();
                                let _unused = reply.send(active);
                            }

                            BanningRequest::Subscribe { reply } => {
                                let rx = self.state.events.subscribe();
                                let _unused = reply.send(rx);
                                debug!("[Banning service][Subscribe] Event channel subscribed");
                            }
                        }

                    } else {
                        // Relay sender dropped -> service is being stopped.
                        info!(
                            "[Banning service] {} inbound relay closed; shutting down service.",
                            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
                        );
                        break Ok(());
                    }
                }
            }
        }
    }
}
