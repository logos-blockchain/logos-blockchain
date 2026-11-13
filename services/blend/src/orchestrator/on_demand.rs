use std::{
    fmt::{Debug, Display},
    time::Duration,
};

use overwatch::{
    overwatch::OverwatchHandle,
    services::{
        AsServiceId, ServiceData,
        relay::OutboundRelay,
        status::{ServiceStatus, WaitPolicy},
    },
};
use tracing::{debug, error, info};

use crate::orchestrator::{Error, LOG_TARGET};

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
        reason = "TODO: address this in a dedicated refactor"
    )]
    pub async fn new(
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    ) -> Result<Self, Error<Service::Message>> {
        let service_id = <RuntimeServiceId as AsServiceId<Service>>::SERVICE_ID;
        info!(target: LOG_TARGET, "Starting service {service_id:}");
        overwatch_handle.start_service::<Service>().await?;

        info!(
            target: LOG_TARGET,
            "Waiting until service {service_id:} is ready"
        );
        let readiness = async {
            let mut status_watcher = overwatch_handle.status_watcher::<Service>().await?;
            status_watcher
                .wait_with_policy(ServiceStatus::Ready, None, WaitPolicy::FailFast)
                .await
                .map(|_| ())
                .map_err(|wait_error| {
                    overwatch::overwatch::Error::Any(
                        format!("Service readiness wait failed: {wait_error}").into(),
                    )
                })
        }
        .await;

        if let Err(e) = readiness {
            debug!(target: LOG_TARGET, "Failed to wait for service readiness. Shutting it down...");
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

    pub async fn handle_inbound_message(
        &self,
        message: Service::Message,
    ) -> Result<(), Error<Service::Message>> {
        self.relay.send(message).await.map_err(Into::into)
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

    /// Wait until the service stops by itself within the given timeout.
    ///
    /// # Errors
    ///
    /// Fails if the service does not stop in time or if the status watcher
    /// cannot be requested.
    async fn wait_until_stopped(
        &self,
        timeout: Duration,
    ) -> Result<(), overwatch::overwatch::Error> {
        let mut status_watcher = self.overwatch_handle.status_watcher().await?;
        status_watcher
            .wait_with_policy(
                ServiceStatus::Stopped,
                Some(timeout),
                WaitPolicy::RestartFriendly,
            )
            .await
            .map(|_| ())
            .map_err(|wait_error| {
                overwatch::overwatch::Error::Any(
                    format!("Service stop wait failed: {wait_error}").into(),
                )
            })
    }
}

async fn kill_service<Service, RuntimeServiceId>(
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
) where
    RuntimeServiceId: AsServiceId<Service> + Debug + Display + Sync,
{
    info!(
        target: LOG_TARGET,
        "Killing service {}",
        <RuntimeServiceId as AsServiceId<Service>>::SERVICE_ID
    );
    if let Err(e) = overwatch_handle.stop_service::<Service>().await {
        error!(target: LOG_TARGET, "Failed to kill service: {e:}");
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt as _;
    use overwatch::{
        DynError, OpaqueServiceResourcesHandle,
        overwatch::{OverwatchRunner, Shutdown},
        services::{
            ServiceCore,
            resources::ServiceResourcesHandle,
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

    mod startup_error {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        use super::{
            DynError, NoOperator, NoState, OnDemandServiceMode, OpaqueServiceResourcesHandle,
            OverwatchRunner, ServiceCore, ServiceData, ServiceStatus, Shutdown,
        };

        #[test_log::test(test)]
        fn on_demand_startup_error_returns_without_hanging() {
            let ran = Arc::new(AtomicBool::new(false));
            let app = OverwatchRunner::<Services>::run(
                ServicesServiceSettings {
                    failing: Arc::clone(&ran),
                },
                None,
            )
            .expect("test runtime should start");

            app.runtime().handle().block_on(async {
                let handle = app.handle();
                let status = handle
                    .status_watcher::<FailingService>()
                    .await
                    .expect("status watcher should be available");
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    OnDemandServiceMode::<FailingService, RuntimeServiceId>::new(handle.clone()),
                )
                .await
                .expect("startup should not hang after the service stops");
                let Err(error) = result else {
                    panic!("startup should fail when the service never becomes ready");
                };

                assert!(ran.load(Ordering::SeqCst), "the service must have run");
                assert_eq!(status.current(), ServiceStatus::Stopped);
                let message = error.to_string();
                assert!(
                    message.contains("Service readiness wait failed"),
                    "{message}"
                );
                assert!(
                    message.contains("service stopped while waiting"),
                    "{message}"
                );
            });
        }

        #[overwatch::derive_services(panic_policy = Shutdown)]
        struct Services {
            failing: FailingService,
        }

        struct FailingService {
            ran: Arc<AtomicBool>,
        }

        impl ServiceData for FailingService {
            type Settings = Arc<AtomicBool>;
            type State = NoState<Self::Settings>;
            type StateOperator = NoOperator<Self::State>;
            type Message = ();
        }

        #[async_trait::async_trait]
        impl ServiceCore<RuntimeServiceId> for FailingService {
            fn init(
                service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
                _: Self::State,
            ) -> Result<Self, DynError> {
                Ok(Self {
                    ran: service_resources_handle
                        .settings_handle
                        .notifier()
                        .get_updated_settings(),
                })
            }

            async fn run(self) -> Result<(), DynError> {
                self.ran.store(true, Ordering::SeqCst);
                Err(std::io::Error::other("startup failed before readiness").into())
            }
        }
    }

    #[overwatch::derive_services(panic_policy = Shutdown)]
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

    #[derive(Debug)]
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
                    ServiceResourcesHandle {
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
