/// Macro call that waits for services to be in `ServiceStatus::Ready` state.
/// It can wait from 1..n services to be ready.
///
/// # Arguments
///
/// Requires `RuntimeServiceId` to be defined in the current scope.
///
/// * `$overwatch_handle`: A reference to the `OverwatchHandle` that manages the
///   services.
/// * `$timeout`: An optional `Duration` that specifies how long to wait for the
///   services to be ready. If `None`, it will wait indefinitely.
/// * `$( $service_type:ident ),+`: A list of service types that should be
///   checked for readiness.
///
/// # Returns
///
/// An async scope that returns:
/// - `Ok(())` if all specified services are ready within the timeout.
/// - `Err(DynError)` if any of the specified services are not ready within the
///   timeout.
///
/// # Example
///
/// ```rust,ignore
/// use std::time::Duration;
/// use overwatch::{DynError, overwatch::OverwatchHandle};
/// use lb_services_utils::overwatch::status::wait_until_services_are_ready;
///
/// // The following types would be defined as part of your Overwatch runtime.
/// struct RuntimeServiceId;
/// struct ServiceA;
/// struct ServiceB;
/// struct ServiceC;
///
/// // Mock function to get an OverwatchHandle
/// fn get_overwatch_handle() -> OverwatchHandle<RuntimeServiceId> {
///     unimplemented!()
/// }
///
/// async fn main() {
///     let overwatch_handle = get_overwatch_handle();
///     let _: Result<(), DynError> = wait_until_services_are_ready!(
///        &overwatch_handle,
///        Some(Duration::from_secs(10)),
///        ServiceA,
///        ServiceB,
///        ServiceC
///     ).await;
/// }
/// ```
///
/// A complete example with working service implementations is available in this
/// module's tests.
#[macro_export]
macro_rules! wait_until_services_are_ready {
    ( $overwatch_handle:expr, $timeout:expr, $( $service_type:ty ),+ ) => {{
        async {
            let overwatch_handle: &::overwatch::overwatch::OverwatchHandle<RuntimeServiceId> = $overwatch_handle;
            let timeout: Option<::std::time::Duration> = $timeout;
            let mut dependencies = Vec::new();

            $(
                let status_watcher = overwatch_handle
                    .status_watcher::<$service_type>()
                    .await
                    .map_err(::overwatch::DynError::from)?;
                let service_id =
                    <RuntimeServiceId as ::overwatch::services::AsServiceId<$service_type>>::SERVICE_ID;
                dependencies.push((service_id, status_watcher));
            )+

            ::overwatch::services::status::wait_for_all_with_policy(
                dependencies,
                ::overwatch::services::status::ServiceStatus::Ready,
                timeout,
                ::overwatch::services::status::WaitPolicy::RestartFriendly,
                ::overwatch::services::status::CompletionPolicy::TargetReachedOnce,
            )
            .await
            .map_err(::overwatch::DynError::from)
        }
    }};
}

pub use wait_until_services_are_ready;

#[cfg(test)]
mod tests {
    use std::{error::Error, sync::Arc, time::Duration};

    use async_trait::async_trait;
    use overwatch::{
        DynError, OpaqueServiceResourcesHandle,
        overwatch::{Overwatch, OverwatchRunner, Shutdown},
        services::{
            AsServiceId, ServiceCore, ServiceData,
            state::{NoOperator, NoState},
            status::{
                DependencyObservation, ServiceStatus, WaitForAllError, WaitForError, WaitPolicy,
            },
        },
    };
    use overwatch_derive::derive_services;
    use tokio::{
        runtime::Handle,
        sync::{Notify, watch},
    };

    struct HeavyService {
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        stop: watch::Sender<bool>,
    }

    impl ServiceData for HeavyService {
        type Settings = watch::Sender<bool>;
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = ();
    }

    #[async_trait]
    impl ServiceCore<RuntimeServiceId> for HeavyService {
        fn init(
            service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
            _initial_state: Self::State,
        ) -> Result<Self, DynError> {
            let stop = service_resources_handle
                .settings_handle
                .notifier()
                .get_updated_settings();
            Ok(Self {
                service_resources_handle,
                stop,
            })
        }

        async fn run(self) -> Result<(), DynError> {
            let mut stop = self.stop.subscribe();
            self.service_resources_handle.status_updater.notify_ready();
            stop.wait_for(|stopped| *stopped)
                .await
                .expect("stop sender should remain available");
            Ok(())
        }
    }

    struct DelayedService {
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        delay: Duration,
    }

    impl ServiceData for DelayedService {
        type Settings = Duration;
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = ();
    }

    #[async_trait]
    impl ServiceCore<RuntimeServiceId> for DelayedService {
        fn init(
            service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
            _initial_state: Self::State,
        ) -> Result<Self, DynError> {
            let delay = service_resources_handle
                .settings_handle
                .notifier()
                .get_updated_settings();
            Ok(Self {
                service_resources_handle,
                delay,
            })
        }

        async fn run(self) -> Result<(), DynError> {
            tokio::time::sleep(self.delay).await;
            self.service_resources_handle.status_updater.notify_ready();
            std::future::pending::<()>().await;
            Ok(())
        }
    }

    struct FailingService {
        _service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        fail_when: Arc<Notify>,
    }

    impl ServiceData for FailingService {
        type Settings = Arc<Notify>;
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = ();
    }

    #[async_trait]
    impl ServiceCore<RuntimeServiceId> for FailingService {
        fn init(
            service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
            _initial_state: Self::State,
        ) -> Result<Self, DynError> {
            let fail_when = service_resources_handle
                .settings_handle
                .notifier()
                .get_updated_settings();
            Ok(Self {
                _service_resources_handle: service_resources_handle,
                fail_when,
            })
        }

        async fn run(self) -> Result<(), DynError> {
            self.fail_when.notified().await;
            panic!("mock dependency startup panic");
        }
    }

    #[derive_services(panic_policy = Option<Shutdown>)]
    struct App {
        restartable_service: HeavyService,
        delayed_service: DelayedService,
        failing_service: FailingService,
    }

    fn initialize(
        stop: watch::Sender<bool>,
        delay: Duration,
        fail_when: Arc<Notify>,
        runtime_handle: Handle,
    ) -> Overwatch<RuntimeServiceId> {
        let settings = AppServiceSettings {
            restartable_service: stop,
            delayed_service: delay,
            failing_service: fail_when,
        };
        OverwatchRunner::<App>::run(settings, Some(runtime_handle))
            .expect("Failed to run overwatch")
    }

    #[tokio::test]
    async fn macro_wait_is_restart_friendly_and_target_reached_once() {
        let (stop_sender, _stop_receiver) = watch::channel(false);
        let overwatch = initialize(
            stop_sender.clone(),
            Duration::from_millis(100),
            Arc::new(Notify::new()),
            Handle::current(),
        );
        let handle = overwatch.handle();

        handle
            .start_service::<HeavyService>()
            .await
            .expect("dependency should start");
        let mut restartable_status = handle
            .status_watcher::<HeavyService>()
            .await
            .expect("restartable service status watcher should be available");
        assert_eq!(
            restartable_status
                .wait_with_policy(
                    ServiceStatus::Ready,
                    Some(Duration::from_secs(5)),
                    WaitPolicy::RestartFriendly,
                )
                .await,
            Ok(ServiceStatus::Ready)
        );
        stop_sender.send_replace(true);
        assert_eq!(
            restartable_status
                .wait_with_policy(
                    ServiceStatus::Stopped,
                    Some(Duration::from_secs(5)),
                    WaitPolicy::RestartFriendly,
                )
                .await,
            Ok(ServiceStatus::Stopped)
        );

        let mut readiness = Box::pin(wait_until_services_are_ready!(
            &handle,
            None,
            HeavyService,
            DelayedService
        ));
        assert!(futures::poll!(readiness.as_mut()).is_pending());
        tokio::task::yield_now().await;
        assert!(futures::poll!(readiness.as_mut()).is_pending());

        stop_sender.send_replace(false);
        handle
            .start_service::<HeavyService>()
            .await
            .expect("stopped dependency should restart");
        assert_eq!(
            restartable_status
                .wait_with_policy(
                    ServiceStatus::Ready,
                    Some(Duration::from_secs(5)),
                    WaitPolicy::RestartFriendly,
                )
                .await,
            Ok(ServiceStatus::Ready)
        );
        assert!(futures::poll!(readiness.as_mut()).is_pending());
        tokio::task::yield_now().await;
        stop_sender.send_replace(true);
        assert_eq!(
            restartable_status
                .wait_with_policy(
                    ServiceStatus::Stopped,
                    Some(Duration::from_secs(5)),
                    WaitPolicy::RestartFriendly,
                )
                .await,
            Ok(ServiceStatus::Stopped)
        );
        assert!(futures::poll!(readiness.as_mut()).is_pending());

        handle
            .start_service::<DelayedService>()
            .await
            .expect("delayed dependency should start");
        tokio::time::timeout(Duration::from_secs(5), &mut readiness)
            .await
            .expect("aggregate wait should finish")
            .expect("both dependencies should have reached Ready");
        assert_eq!(restartable_status.current(), ServiceStatus::Stopped);
        drop(readiness);

        handle.shutdown().await.expect("Overwatch should shut down");
        overwatch
            .wait_finished()
            .await
            .expect("No service should panic");
    }

    #[tokio::test]
    async fn macro_propagates_overwatch_error_through_dyn_error() {
        let (stop_sender, _stop_receiver) = watch::channel(false);
        let fail_when = Arc::new(Notify::new());
        let overwatch = initialize(
            stop_sender,
            Duration::from_secs(1),
            Arc::clone(&fail_when),
            Handle::current(),
        );
        let handle = overwatch.handle();
        handle
            .start_service::<FailingService>()
            .await
            .expect("failing dependency should start");

        let mut readiness = Box::pin(wait_until_services_are_ready!(
            &handle,
            None,
            FailingService
        ));
        assert!(futures::poll!(readiness.as_mut()).is_pending());
        tokio::task::yield_now().await;
        assert!(futures::poll!(readiness.as_mut()).is_pending());
        fail_when.notify_one();

        let error = tokio::time::timeout(Duration::from_secs(5), &mut readiness)
            .await
            .expect("failed dependency should end readiness wait")
            .expect_err("failed dependency must return an error");
        let aggregate = error
            .as_ref()
            .downcast_ref::<WaitForAllError<RuntimeServiceId>>()
            .expect("the Overwatch aggregate error should be preserved");
        assert_eq!(
            aggregate.failed().map(DependencyObservation::id),
            Some(&<RuntimeServiceId as AsServiceId<FailingService>>::SERVICE_ID)
        );
        let expected = WaitForError::ServiceFailed {
            initial_status: ServiceStatus::Starting,
        };
        assert_eq!(aggregate.wait_error(), Some(&expected));
        assert_eq!(
            Error::source(aggregate).and_then(|source| source.downcast_ref::<WaitForError>()),
            Some(&expected)
        );
        drop(readiness);

        handle.shutdown().await.expect("Overwatch should shut down");
        overwatch
            .wait_finished()
            .await
            .expect("No service should panic");
    }
}
