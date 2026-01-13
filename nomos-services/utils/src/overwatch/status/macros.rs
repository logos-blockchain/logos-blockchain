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
/// use services_utils::overwatch::status::wait_until_services_are_ready;
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
    ( $overwatch_handle:expr, $timeout:expr, $( $service_type:ty ),+ ) => {
        async {
            let overwatch_handle: &::overwatch::overwatch::OverwatchHandle<RuntimeServiceId> = $overwatch_handle;
            let timeout: Option<::std::time::Duration> = $timeout;
            let mut wait_for_futures: Vec<::std::pin::Pin<Box<dyn ::std::future::Future<Output = ::std::result::Result::<(), $crate::overwatch::status::ServiceStatusEntry::<RuntimeServiceId>>> + Send>>> = Vec::new();

            // Iterate over each service type and create a future to wait for its readiness
            $(
                let wait_for_future = async {
                    if let Err(service_status) = overwatch_handle
                        .status_watcher::<$service_type>()
                        .await
                        .wait_for(::overwatch::services::status::ServiceStatus::Ready, timeout)
                        .await
                    {
                        let service_id = <RuntimeServiceId as ::overwatch::services::AsServiceId<$service_type>>::SERVICE_ID;
                        let service_status_entry = $crate::overwatch::status::ServiceStatusEntry::<RuntimeServiceId>::from_overwatch(service_id, service_status);
                        return Err(service_status_entry);
                    }

                    ::std::result::Result::<(), $crate::overwatch::status::ServiceStatusEntry::<RuntimeServiceId>>::Ok(())
                };
                let pinned_wait_for_future = Box::pin(wait_for_future);
                wait_for_futures.push(pinned_wait_for_future);
            )+

            // Wait for all futures to complete
            let results: Vec<::std::result::Result<(), $crate::overwatch::status::ServiceStatusEntry::<RuntimeServiceId>>> = ::futures::future::join_all(wait_for_futures).await;

            // Filter out any errors from the results
            let errors: Vec<$crate::overwatch::status::ServiceStatusEntry::<RuntimeServiceId>> = results.into_iter().filter_map(|res| res.err()).collect();

            // If any of the services are not ready, return an error with the service status entries
            if !errors.is_empty() {
                let error: $crate::overwatch::status::ServiceStatusEntriesError<RuntimeServiceId> = errors.into();
                return ::std::result::Result::Err(::overwatch::DynError::from(error));
            }

            // If all services are ready, return Ok(())
            ::std::result::Result::<(), ::overwatch::DynError>::Ok(())
        }
    };
}

pub use wait_until_services_are_ready;

#[cfg(test)]
mod tests {
    use std::{
        fmt::{Debug, Display},
        time::Duration,
    };

    use async_trait::async_trait;
    use overwatch::{
        DynError, OpaqueServiceResourcesHandle,
        overwatch::{Overwatch, OverwatchRunner},
        services::{
            AsServiceId, ServiceCore, ServiceData,
            state::{NoOperator, NoState},
            status::ServiceStatus,
        },
    };
    use overwatch_derive::derive_services;

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

    struct HeavyService {
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    }

    impl ServiceData for HeavyService {
        type Settings = ();
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
            Ok(Self {
                service_resources_handle,
            })
        }

        async fn run(self) -> Result<(), DynError> {
            // Simulate some initialisation work
            tokio::time::sleep(Duration::from_secs(2)).await;

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
            wait_until_services_are_ready!(
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
            wait_until_services_are_ready!(
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

    #[derive_services]
    struct App {
        light_service: LightService,
        heavy_service: HeavyService,
        nested_generic_service: NestedGenericService<LightService, RuntimeServiceId>,
        dependent_service: DependantService<
            NestedGenericService<LightService, RuntimeServiceId>,
            RuntimeServiceId,
        >,
    }

    fn initialize() -> Overwatch<RuntimeServiceId> {
        let settings = AppServiceSettings {
            light_service: (),
            heavy_service: (),
            nested_generic_service: (),
            dependent_service: (),
        };
        OverwatchRunner::<App>::run(settings, None).expect("Failed to run overwatch")
    }

    #[test]
    fn test_wait_until_services_are_ready_macro() {
        let overwatch = initialize();
        let overwatch_handle = overwatch.handle();
        drop(
            overwatch_handle
                .runtime()
                .block_on(overwatch_handle.start_all_services()),
        );

        // Wait until ServiceC is ready, which depends on ServiceA and ServiceB
        let dependent_service_status = overwatch_handle.runtime().block_on(async {
            overwatch_handle
                .status_watcher::<DependantService<
                    NestedGenericService<LightService, RuntimeServiceId>,
                    RuntimeServiceId,
                >>()
                .await
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

    #[test]
    fn test_wait_until_services_are_ready_macro_timeout() {
        let overwatch = initialize();
        let overwatch_handle = overwatch.handle();
        drop(
            overwatch_handle
                .runtime()
                .block_on(overwatch_handle.start_all_services()),
        );

        // Wait for a service that will not be ready, expecting a timeout error
        let dependent_service_status = overwatch_handle.runtime().block_on(async {
            overwatch_handle
                .status_watcher::<DependantService<
                    NestedGenericService<LightService, RuntimeServiceId>,
                    RuntimeServiceId,
                >>()
                .await
                .wait_for(ServiceStatus::Ready, Some(Duration::from_secs(1)))
                .await
        });

        assert_eq!(
            dependent_service_status,
            Err(ServiceStatus::Starting),
            "DependantService should still be Starting."
        );

        // Teardown
        drop(
            overwatch_handle
                .runtime()
                .block_on(overwatch_handle.shutdown()),
        );
        overwatch.blocking_wait_finished();
    }
}
