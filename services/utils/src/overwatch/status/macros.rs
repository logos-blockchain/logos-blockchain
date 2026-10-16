/// Wait for one or more services to become ready while detecting terminal
/// lifecycle states.
///
/// Requires `RuntimeServiceId` to be defined in the current scope. Every
/// dependency must have passed an acknowledged startup boundary before this
/// macro is called. Status watchers expose only the latest state, so `Stopped`
/// and `Failed` are treated as terminal once the wait begins.
///
/// Overwatch processes each command to completion before handling queued
/// status-watcher requests. Its `start_service_sequence` completion waits for
/// each listed service's start acknowledgement, after the service has entered
/// `Starting`. The node's standalone and C-binding entry points await this
/// sequence; on-demand Blend services await `start_service` directly.
///
/// `$timeout` is an `Option<Duration>`. `Some(duration)` bounds the wait for
/// each dependency and reports its observed status on timeout. `None` waits
/// without a deadline. `Ready` succeeds, `Starting` waits, and `Stopped` or
/// `Failed` fail immediately. Multiple dependencies are composed fail-fast.
/// Periodic progress logs do not terminate the wait, and dropping the returned
/// future cancels all waits.
///
/// # Example
///
/// ```rust,ignore
/// wait_until_services_are_ready_with_lifecycle!(
///     &overwatch_handle,
///     None,
///     ServiceA,
///     ServiceB,
/// ).await?;
/// ```
#[macro_export]
macro_rules! wait_until_services_are_ready_with_lifecycle {
    ( $overwatch_handle:expr, $timeout:expr, $( $service_type:ty ),+ ) => {{
        async {
            let overwatch_handle: &::overwatch::overwatch::OverwatchHandle<RuntimeServiceId> = $overwatch_handle;
            let timeout: Option<::std::time::Duration> = $timeout;
            let mut wait_for_futures = Vec::new();

            $(
                let status_watcher = overwatch_handle
                    .status_watcher::<$service_type>()
                    .await
                    .map_err(::overwatch::DynError::from)?;
                let service_id =
                    <RuntimeServiceId as ::overwatch::services::AsServiceId<$service_type>>::SERVICE_ID;
                wait_for_futures.push(
                    $crate::overwatch::status::wait_for_service_to_be_ready(
                        status_watcher,
                        service_id,
                        timeout,
                    ),
                );
            )+

            if let Err(service_status_entry) =
                ::futures::future::try_join_all(wait_for_futures).await
            {
                let error: $crate::overwatch::status::ServiceStatusEntriesError<RuntimeServiceId> =
                    [service_status_entry].into();
                return ::std::result::Result::Err(::overwatch::DynError::from(error));
            }

            ::std::result::Result::<(), ::overwatch::DynError>::Ok(())
        }
    }};
}

pub use wait_until_services_are_ready_with_lifecycle;

#[cfg(test)]
mod tests {
    use std::{
        fmt::{Debug, Display},
        sync::Arc,
        time::Duration,
    };

    use async_trait::async_trait;
    use overwatch::{
        DynError, OpaqueServiceResourcesHandle,
        overwatch::{Overwatch, OverwatchRunner, Shutdown},
        services::{
            AsServiceId, ServiceCore, ServiceData,
            state::{NoOperator, NoState},
            status::ServiceStatus,
        },
    };
    use overwatch_derive::derive_services;
    use tokio::{runtime::Handle, sync::Notify};

    async fn notify_ready_and_wait<Service: ServiceData, RuntimeServiceId>(
        service_resources_handle: &OpaqueServiceResourcesHandle<Service, RuntimeServiceId>,
    ) where
        Service::Message: Send,
        Service::State: Send + Sync,
        Service::Settings: Send + Sync,
        RuntimeServiceId: Send,
    {
        // Notify that the service is ready
        service_resources_handle.status_updater.notify_ready();

        // Await infinitely so Status::Ready can be observed
        std::future::pending::<()>().await;
    }

    struct LightService {
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    }

    impl ServiceData for LightService {
        type Settings = ();
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = ();
    }

    #[async_trait]
    impl ServiceCore<RuntimeServiceId> for LightService {
        fn init(
            service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
            _initial_state: Self::State,
        ) -> Result<Self, DynError> {
            Ok(Self {
                service_resources_handle,
            })
        }

        async fn run(self) -> Result<(), DynError> {
            notify_ready_and_wait::<Self, RuntimeServiceId>(&self.service_resources_handle).await;
            Ok(())
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum HeavyServiceOutcome {
        Ready,
        ReturnError,
        Panic,
    }

    #[derive(Clone, Debug)]
    struct HeavyServiceSettings {
        startup_delay: Duration,
        outcome: HeavyServiceOutcome,
        started: Option<Arc<Notify>>,
    }

    impl HeavyServiceSettings {
        fn ready_after(startup_delay: Duration) -> Self {
            Self {
                startup_delay,
                outcome: HeavyServiceOutcome::Ready,
                started: None,
            }
        }
    }

    struct HeavyService {
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        settings: HeavyServiceSettings,
    }

    impl ServiceData for HeavyService {
        type Settings = HeavyServiceSettings;
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
            let settings = service_resources_handle
                .settings_handle
                .notifier()
                .get_updated_settings();
            Ok(Self {
                service_resources_handle,
                settings,
            })
        }

        async fn run(self) -> Result<(), DynError> {
            if let Some(started) = &self.settings.started {
                started.notify_one();
            }
            tokio::time::sleep(self.settings.startup_delay).await;

            match self.settings.outcome {
                HeavyServiceOutcome::Ready => {
                    notify_ready_and_wait::<Self, RuntimeServiceId>(&self.service_resources_handle)
                        .await;
                    Ok(())
                }
                HeavyServiceOutcome::ReturnError => {
                    Err(DynError::from("mock dependency startup error"))
                }
                HeavyServiceOutcome::Panic => panic!("mock dependency startup panic"),
            }
        }
    }

    struct LifecycleDependantService<Dependency, RuntimeServiceId> {
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _dependency: std::marker::PhantomData<Dependency>,
    }

    impl<Dependency, RuntimeServiceId> ServiceData
        for LifecycleDependantService<Dependency, RuntimeServiceId>
    {
        type Settings = ();
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = ();
    }

    #[async_trait]
    impl<Dependency, RuntimeServiceId> ServiceCore<RuntimeServiceId>
        for LifecycleDependantService<Dependency, RuntimeServiceId>
    where
        RuntimeServiceId:
            Debug + Send + Sync + Display + AsServiceId<Dependency> + AsServiceId<Self> + 'static,
        Dependency: Send + Sync + 'static,
    {
        fn init(
            service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
            _initial_state: Self::State,
        ) -> Result<Self, DynError> {
            Ok(Self {
                service_resources_handle,
                _dependency: std::marker::PhantomData,
            })
        }

        async fn run(self) -> Result<(), DynError> {
            wait_until_services_are_ready_with_lifecycle!(
                &self.service_resources_handle.overwatch_handle,
                None,
                Dependency
            )
            .await?;
            notify_ready_and_wait::<Self, RuntimeServiceId>(&self.service_resources_handle).await;
            Ok(())
        }
    }

    struct NestedGenericService<GenericService, RuntimeServiceId> {
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    }

    impl<GenericService, RuntimeServiceId> ServiceData
        for NestedGenericService<GenericService, RuntimeServiceId>
    {
        type Settings = ();
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = ();
    }

    #[async_trait]
    impl<GenericService, RuntimeServiceId> ServiceCore<RuntimeServiceId>
        for NestedGenericService<GenericService, RuntimeServiceId>
    where
        RuntimeServiceId: Debug + Send + Sync + Display + AsServiceId<GenericService> + 'static,
    {
        fn init(
            service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
            _initial_state: Self::State,
        ) -> Result<Self, DynError> {
            Ok(Self {
                service_resources_handle,
            })
        }

        async fn run(self) -> Result<(), DynError> {
            wait_until_services_are_ready_with_lifecycle!(
                &self.service_resources_handle.overwatch_handle,
                None,
                GenericService
            )
            .await?;
            notify_ready_and_wait::<Self, RuntimeServiceId>(&self.service_resources_handle).await;
            Ok(())
        }
    }

    struct DependantService<GenericService, RuntimeServiceId> {
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    }

    impl<GenericService, RuntimeServiceId> ServiceData
        for DependantService<GenericService, RuntimeServiceId>
    {
        type Settings = ();
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = ();
    }

    #[async_trait]
    impl<GenericService, RuntimeServiceId> ServiceCore<RuntimeServiceId>
        for DependantService<GenericService, RuntimeServiceId>
    where
        RuntimeServiceId: Debug
            + Send
            + Sync
            + Display
            + AsServiceId<GenericService>
            + AsServiceId<HeavyService>
            + AsServiceId<Self>
            + 'static,
    {
        fn init(
            service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
            _initial_state: Self::State,
        ) -> Result<Self, DynError> {
            Ok(Self {
                service_resources_handle,
            })
        }

        async fn run(self) -> Result<(), DynError> {
            wait_until_services_are_ready_with_lifecycle!(
                &self.service_resources_handle.overwatch_handle,
                None,
                GenericService,
                HeavyService
            )
            .await?;
            notify_ready_and_wait::<Self, RuntimeServiceId>(&self.service_resources_handle).await;
            Ok(())
        }
    }

    struct PendingService {
        _service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    }

    impl ServiceData for PendingService {
        type Settings = ();
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = ();
    }

    #[async_trait]
    impl ServiceCore<RuntimeServiceId> for PendingService {
        fn init(
            service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
            _initial_state: Self::State,
        ) -> Result<Self, DynError> {
            Ok(Self {
                _service_resources_handle: service_resources_handle,
            })
        }

        async fn run(self) -> Result<(), DynError> {
            std::future::pending::<Result<(), DynError>>().await
        }
    }

    #[derive_services(panic_policy = Option<Shutdown>)]
    struct App {
        light_service: LightService,
        lifecycle_dependant_service: LifecycleDependantService<HeavyService, RuntimeServiceId>,
        heavy_service: HeavyService,
        pending_service: PendingService,
        nested_generic_service: NestedGenericService<LightService, RuntimeServiceId>,
        dependent_service: DependantService<
            NestedGenericService<LightService, RuntimeServiceId>,
            RuntimeServiceId,
        >,
    }

    fn initialize() -> Overwatch<RuntimeServiceId> {
        initialize_with(
            HeavyServiceSettings::ready_after(Duration::from_secs(2)),
            None,
        )
    }

    fn initialize_with(
        heavy_service: HeavyServiceSettings,
        runtime_handle: Option<Handle>,
    ) -> Overwatch<RuntimeServiceId> {
        OverwatchRunner::<App>::run(app_settings(heavy_service), runtime_handle)
            .expect("Failed to run overwatch")
    }

    fn initialize_with_panic_policy(
        heavy_service: HeavyServiceSettings,
        runtime_handle: Option<Handle>,
        panic_policy: Option<Shutdown>,
    ) -> Overwatch<RuntimeServiceId> {
        OverwatchRunner::<App>::run_with_panic_policy(
            app_settings(heavy_service),
            runtime_handle,
            panic_policy,
        )
        .expect("Failed to run overwatch")
    }

    fn app_settings(heavy_service: HeavyServiceSettings) -> AppServiceSettings {
        AppServiceSettings {
            light_service: (),
            lifecycle_dependant_service: (),
            heavy_service,
            pending_service: (),
            nested_generic_service: (),
            dependent_service: (),
        }
    }

    #[test]
    fn lifecycle_waits_for_all_dependencies() {
        let overwatch = initialize();
        let overwatch_handle = overwatch.handle();
        drop(
            overwatch_handle
                .runtime()
                .block_on(overwatch_handle.start_all_services()),
        );

        // Wait until the dependent service is ready after both dependencies.
        let mut status_watcher = overwatch_handle.runtime().block_on(async {
            overwatch_handle
                .status_watcher::<DependantService<
                    NestedGenericService<LightService, RuntimeServiceId>,
                    RuntimeServiceId,
                >>()
                .await
                .expect("Status watcher should be produced.")
        });

        let dependent_service_status = overwatch_handle.runtime().block_on(async {
            status_watcher
                .wait_for(ServiceStatus::Ready, Some(Duration::from_secs(5)))
                .await
        });

        assert_eq!(
            dependent_service_status,
            Ok(ServiceStatus::Ready),
            "DependentService should be Ready."
        );

        drop(
            overwatch_handle
                .runtime()
                .block_on(overwatch_handle.shutdown()),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_wait_times_out_with_dependency_and_status() {
        let settings = HeavyServiceSettings::ready_after(Duration::from_secs(10));
        let overwatch = initialize_with(settings, Some(Handle::current()));
        let overwatch_handle = overwatch.handle();
        overwatch_handle
            .start_service::<HeavyService>()
            .await
            .expect("dependency should start");

        let error = wait_until_services_are_ready_with_lifecycle!(
            &overwatch_handle,
            Some(Duration::from_secs(1)),
            HeavyService
        )
        .await
        .expect_err("bounded lifecycle wait should time out");
        let message = error.to_string();
        let service_id = <RuntimeServiceId as AsServiceId<HeavyService>>::SERVICE_ID;
        assert!(message.contains(&service_id.to_string()));
        assert!(message.contains("Starting"));

        overwatch_handle
            .shutdown()
            .await
            .expect("Overwatch should shut down");
        overwatch
            .wait_finished()
            .await
            .expect("No service should panic");
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_wait_allows_slow_dependency_and_start_order_race() {
        let started = Arc::new(Notify::new());
        let mut settings = HeavyServiceSettings::ready_after(Duration::from_secs(61));
        settings.started = Some(Arc::clone(&started));
        let overwatch = initialize_with(settings, Some(Handle::current()));
        let handle = overwatch.handle();

        // The lifecycle dependant is first in the registered service order,
        // before its dependency enters Starting. Overwatch processes its
        // queued status-watcher request only after acknowledging the full
        // service-start sequence, by which time HeavyService has entered
        // Starting.
        handle
            .start_all_services()
            .await
            .expect("services should start");
        started.notified().await;

        let dependency_status = handle
            .status_watcher::<HeavyService>()
            .await
            .expect("dependency status watcher should be available");
        let mut dependent_status = handle
            .status_watcher::<LifecycleDependantService<HeavyService, RuntimeServiceId>>()
            .await
            .expect("dependent status watcher should be available");

        let mut readiness = Box::pin(wait_until_services_are_ready_with_lifecycle!(
            &handle,
            None,
            HeavyService
        ));
        tokio::select! {
            biased;
            result = &mut readiness => panic!("dependency completed before the delay: {result:?}"),
            () = tokio::task::yield_now() => {}
        }

        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        assert!(futures::poll!(readiness.as_mut()).is_pending());
        assert_eq!(
            dependency_status.current(),
            ServiceStatus::Starting,
            "dependency should still be Starting beyond the former deadline"
        );
        assert_eq!(
            dependent_status.current(),
            ServiceStatus::Starting,
            "dependent should wait rather than fail before dependency readiness"
        );

        tokio::time::advance(Duration::from_secs(1)).await;
        readiness
            .await
            .expect("slow dependency should become ready");
        assert_eq!(
            dependent_status
                .wait_for(ServiceStatus::Ready, Some(Duration::from_secs(5)))
                .await,
            Ok(ServiceStatus::Ready),
            "dependent should become Ready after its dependency"
        );

        handle.shutdown().await.expect("Overwatch should shut down");
        overwatch
            .wait_finished()
            .await
            .expect("No service should panic");
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_wait_reports_initial_stopped_dependency() {
        let overwatch = initialize_with(
            HeavyServiceSettings::ready_after(Duration::ZERO),
            Some(Handle::current()),
        );
        let handle = overwatch.handle();

        let error = tokio::time::timeout(
            Duration::from_secs(1),
            wait_until_services_are_ready_with_lifecycle!(&handle, None, HeavyService),
        )
        .await
        .expect("initial Stopped status should be handled immediately")
        .expect_err("initial Stopped status is terminal");
        let message = error.to_string();
        let service_id = <RuntimeServiceId as AsServiceId<HeavyService>>::SERVICE_ID;
        assert!(message.contains(&service_id.to_string()));
        assert!(message.contains("Stopped"));

        handle.shutdown().await.expect("Overwatch should shut down");
        overwatch
            .wait_finished()
            .await
            .expect("No service should panic");
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_wait_reports_stopped_before_watcher_attachment() {
        let mut settings = HeavyServiceSettings::ready_after(Duration::ZERO);
        settings.outcome = HeavyServiceOutcome::ReturnError;
        let overwatch =
            initialize_with_panic_policy(settings, Some(Handle::current()), Some(Shutdown));
        let handle = overwatch.handle();

        handle
            .start_service::<HeavyService>()
            .await
            .expect("dependency should start");
        let mut watcher = handle
            .status_watcher::<HeavyService>()
            .await
            .expect("status watcher should be available");
        assert_eq!(
            watcher
                .wait_for(ServiceStatus::Stopped, Some(Duration::from_secs(5)))
                .await,
            Ok(ServiceStatus::Stopped),
            "mock dependency should stop before the lifecycle wait attaches"
        );

        let error = wait_until_services_are_ready_with_lifecycle!(&handle, None, HeavyService)
            .await
            .expect_err("Stopped dependency must fail readiness");
        let message = error.to_string();
        let service_id = <RuntimeServiceId as AsServiceId<HeavyService>>::SERVICE_ID;
        assert!(message.contains(&service_id.to_string()));
        assert!(message.contains("Stopped"));

        handle
            .start_service::<LightService>()
            .await
            .expect("a returned service error must not invoke the panic policy");
        let mut light_watcher = handle
            .status_watcher::<LightService>()
            .await
            .expect("light service status watcher should be available");
        assert_eq!(
            light_watcher
                .wait_for(ServiceStatus::Ready, Some(Duration::from_secs(5)))
                .await,
            Ok(ServiceStatus::Ready),
            "Overwatch should remain running after a service returns Err"
        );

        handle.shutdown().await.expect("Overwatch should shut down");
        overwatch
            .wait_finished()
            .await
            .expect("No service should panic");
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_wait_reports_failed_dependency_before_watcher_attachment() {
        let mut settings = HeavyServiceSettings::ready_after(Duration::ZERO);
        settings.outcome = HeavyServiceOutcome::Panic;
        let overwatch = initialize_with(settings, Some(Handle::current()));
        let handle = overwatch.handle();

        handle
            .start_service::<HeavyService>()
            .await
            .expect("dependency should start");
        let mut watcher = handle
            .status_watcher::<HeavyService>()
            .await
            .expect("dependency status watcher should be available");
        assert_eq!(
            watcher
                .wait_for(ServiceStatus::Failed, Some(Duration::from_secs(5)))
                .await,
            Ok(ServiceStatus::Failed),
            "mock dependency should fail before the lifecycle wait attaches"
        );

        let error = wait_until_services_are_ready_with_lifecycle!(&handle, None, HeavyService)
            .await
            .expect_err("Failed dependency must fail readiness");
        let message = error.to_string();
        let service_id = <RuntimeServiceId as AsServiceId<HeavyService>>::SERVICE_ID;
        assert!(message.contains(&service_id.to_string()));
        assert!(message.contains("Failed"));

        handle.shutdown().await.expect("Overwatch should shut down");
        overwatch
            .wait_finished()
            .await
            .expect("No service should panic");
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_wait_reports_panicked_dependency() {
        let mut settings = HeavyServiceSettings::ready_after(Duration::from_secs(1));
        settings.outcome = HeavyServiceOutcome::Panic;
        let overwatch = initialize_with(settings, Some(Handle::current()));
        let handle = overwatch.handle();

        handle
            .start_service::<HeavyService>()
            .await
            .expect("dependency should start");
        let mut readiness = Box::pin(wait_until_services_are_ready_with_lifecycle!(
            &handle,
            None,
            HeavyService
        ));
        tokio::select! {
            biased;
            result = &mut readiness => panic!("dependency completed before the panic: {result:?}"),
            () = tokio::task::yield_now() => {}
        }
        tokio::time::advance(Duration::from_secs(1)).await;

        let error = readiness
            .await
            .expect_err("Failed dependency must fail readiness");
        let message = error.to_string();
        let service_id = <RuntimeServiceId as AsServiceId<HeavyService>>::SERVICE_ID;
        assert!(message.contains(&service_id.to_string()));
        assert!(message.contains("Failed"));

        handle.shutdown().await.expect("Overwatch should shut down");
        overwatch
            .wait_finished()
            .await
            .expect("No service should panic");
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_wait_fails_fast_when_one_dependency_stops() {
        let mut settings = HeavyServiceSettings::ready_after(Duration::from_secs(1));
        settings.outcome = HeavyServiceOutcome::ReturnError;
        let overwatch = initialize_with(settings, Some(Handle::current()));
        let handle = overwatch.handle();

        handle
            .start_all_services()
            .await
            .expect("services should start");
        let pending_status = handle
            .status_watcher::<PendingService>()
            .await
            .expect("pending service status watcher should be available");
        assert_eq!(pending_status.current(), ServiceStatus::Starting);

        let error = tokio::time::timeout(
            Duration::from_secs(2),
            wait_until_services_are_ready_with_lifecycle!(
                &handle,
                None,
                PendingService,
                HeavyService
            ),
        )
        .await
        .expect("failed dependency should not wait for the pending dependency")
        .expect_err("Stopped dependency must fail readiness");
        let message = error.to_string();
        let service_id = <RuntimeServiceId as AsServiceId<HeavyService>>::SERVICE_ID;
        assert!(message.contains(&service_id.to_string()));
        assert!(message.contains("Stopped"));
        assert_eq!(pending_status.current(), ServiceStatus::Starting);

        handle.shutdown().await.expect("Overwatch should shut down");
        overwatch
            .wait_finished()
            .await
            .expect("No service should panic");
    }
}
