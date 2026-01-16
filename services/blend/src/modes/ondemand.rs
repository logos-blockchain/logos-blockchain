use std::{
    fmt::{Debug, Display},
    time::Duration,
};

use overwatch::{
    overwatch::OverwatchHandle,
    services::{AsServiceId, ServiceData, relay::OutboundRelay, status::ServiceStatus},
};
use services_utils::wait_until_services_are_ready;
use tracing::{debug, error, info};

use crate::modes::{Error, LOG_TARGET};

pub struct OnDemandServiceMode<Service, RuntimeServiceId>
where
    Service: ServiceData,
{
    relay: OutboundRelay<Service::Message>,
    overwatch_handle: OverwatchHandle<RuntimeServiceId>,
}

impl<Service, RuntimeServiceId> OnDemandServiceMode<Service, RuntimeServiceId>
where
    Service: ServiceData<Message: Send + 'static>,
    RuntimeServiceId: AsServiceId<Service> + Debug + Display + Send + Sync + 'static,
{
    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: Address this at some point."
    )]
    pub async fn new(overwatch_handle: OverwatchHandle<RuntimeServiceId>) -> Result<Self, Error> {
        let service_id = <RuntimeServiceId as AsServiceId<Service>>::SERVICE_ID;
        info!(target = LOG_TARGET, "Starting service {service_id:}");
        overwatch_handle
            .start_service::<Service>()
            .await
            .map_err(|e| Error::Overwatch(Box::new(e)))?;

        info!(
            target = LOG_TARGET,
            "Waiting until service {service_id:} is ready"
        );
        if let Err(e) = wait_until_services_are_ready!(
            &overwatch_handle,
            Some(Duration::from_secs(60)),
            Service
        )
        .await
        {
            debug!(target: LOG_TARGET, "Service took too long to start. Shutting it down again...");
            kill_service(&overwatch_handle).await;
            return Err(e.into());
        }

        let relay = match overwatch_handle.relay::<Service>().await {
            Ok(relay) => relay,
            Err(e) => {
                kill_service(&overwatch_handle).await;
                return Err(e.into());
            }
        };

        Ok(Self {
            relay,
            overwatch_handle,
        })
    }

    pub async fn handle_inbound_message(&self, message: Service::Message) -> Result<(), Error> {
        self.relay.send(message).await.map_err(|(e, _)| e.into())
    }

    /// Wait until the service is stopped itself within the given timeout.
    /// If it does not stop in time, forcefully kill it.
    ///
    /// Returns `true` if the service stopped itself, `false` if it was killed.
    pub async fn wait_until_stopped_or_kill(self, timeout: Duration) -> bool {
        if self.wait_until_stopped(timeout).await.is_ok() {
            return true;
        }
        kill_service(&self.overwatch_handle).await;
        false
    }

    async fn wait_until_stopped(&self, timeout: Duration) -> Result<(), ServiceStatus> {
        let mut watcher = self.overwatch_handle.status_watcher().await;
        watcher
            .wait_for(ServiceStatus::Stopped, Some(timeout))
            .await
            .map(|_| ())
    }
}

async fn kill_service<Service, RuntimeServiceId>(
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
) where
    RuntimeServiceId: AsServiceId<Service> + Debug + Display + Sync,
{
    info!(
        target = LOG_TARGET,
        "Killing service {}",
        <RuntimeServiceId as AsServiceId<Service>>::SERVICE_ID
    );
    if let Err(e) = overwatch_handle.stop_service::<Service>().await {
        error!(target = LOG_TARGET, "Failed to kill service: {e:}");
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt as _;
    use overwatch::{
        DynError, OpaqueServiceResourcesHandle,
        overwatch::OverwatchRunner,
        services::{
            ServiceCore,
            state::{NoOperator, NoState},
        },
    };
    use tokio::sync::oneshot;
    use tracing::{debug, info};

    use super::*;

    #[test_log::test(test)]
    fn wait_until_stopped() {
        let max_pong_count = 1;
        let app = OverwatchRunner::<Services>::run(settings(max_pong_count), None).unwrap();
        app.runtime().handle().block_on(async {
            let mode =
                OnDemandServiceMode::<PongService, RuntimeServiceId>::new(app.handle().clone())
                    .await
                    .unwrap();

            // Check if the mode has started by sending a Ping message.
            let (reply_sender, reply_receiver) = oneshot::channel();
            mode.handle_inbound_message(PongServiceMessage::Ping(100, reply_sender))
                .await
                .unwrap();
            let reply = reply_receiver.await.unwrap();
            assert_eq!(reply, 100);

            // Now that the service ponged `max_pong_count` times, it should stop itself.
            assert!(
                mode.wait_until_stopped_or_kill(Duration::from_secs(1))
                    .await
            );

            // Check if it can be started again.
            let mode =
                OnDemandServiceMode::<PongService, RuntimeServiceId>::new(app.handle().clone())
                    .await
                    .unwrap();
            let (reply_sender, reply_receiver) = oneshot::channel();
            mode.handle_inbound_message(PongServiceMessage::Ping(100, reply_sender))
                .await
                .unwrap();
            let reply = reply_receiver.await.unwrap();
            assert_eq!(reply, 100);
        });
    }

    #[test_log::test(test)]
    fn kill_after_stop_timeout() {
        let max_pong_count = 2;
        let app = OverwatchRunner::<Services>::run(settings(max_pong_count), None).unwrap();
        app.runtime().handle().block_on(async {
            let mode =
                OnDemandServiceMode::<PongService, RuntimeServiceId>::new(app.handle().clone())
                    .await
                    .unwrap();

            // Check if the mode has started by sending a Ping message.
            let (reply_sender, reply_receiver) = oneshot::channel();
            mode.handle_inbound_message(PongServiceMessage::Ping(100, reply_sender))
                .await
                .unwrap();
            let reply = reply_receiver.await.unwrap();
            assert_eq!(reply, 100);

            // The service shouldn't stop itself since it didn't receive `max_pong_count`
            // pings yet. Expect that it is forcefully killed after the timeout.
            assert!(
                !mode
                    .wait_until_stopped_or_kill(Duration::from_secs(1))
                    .await
            );

            // Check if it can be started again.
            let mode =
                OnDemandServiceMode::<PongService, RuntimeServiceId>::new(app.handle().clone())
                    .await
                    .unwrap();
            let (reply_sender, reply_receiver) = oneshot::channel();
            mode.handle_inbound_message(PongServiceMessage::Ping(100, reply_sender))
                .await
                .unwrap();
            let reply = reply_receiver.await.unwrap();
            assert_eq!(reply, 100);
        });
    }

    #[overwatch::derive_services]
    struct Services {
        pong: PongService,
    }

    struct PongService {
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    }

    impl ServiceData for PongService {
        type Settings = usize;
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = PongServiceMessage;
    }

    enum PongServiceMessage {
        Ping(usize, oneshot::Sender<usize>),
    }

    #[async_trait::async_trait]
    impl ServiceCore<RuntimeServiceId> for PongService {
        fn init(
            service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
            _: Self::State,
        ) -> Result<Self, DynError> {
            Ok(Self {
                service_resources_handle,
            })
        }

        async fn run(mut self) -> Result<(), DynError> {
            let Self {
                service_resources_handle:
                    OpaqueServiceResourcesHandle::<Self, RuntimeServiceId> {
                        ref mut inbound_relay,
                        ref status_updater,
                        ref settings_handle,
                        ..
                    },
                ..
            } = self;

            let max_pong_count = settings_handle.notifier().get_updated_settings();

            let service_id = <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID;
            status_updater.notify_ready();
            info!("Service {service_id} is ready.",);

            for _ in 0..max_pong_count {
                if let Some(PongServiceMessage::Ping(message, reply_sender)) =
                    inbound_relay.next().await
                {
                    debug!("Service {service_id} received message: {message}");
                    if reply_sender.send(message).is_err() {
                        error!("Failed to send response");
                    }
                }
            }

            Ok(())
        }
    }

    fn settings(max_pong_count: usize) -> ServicesServiceSettings {
        ServicesServiceSettings {
            pong: max_pong_count,
        }
    }
}
