use std::time::Duration;

use lb_node::RuntimeServiceId;
use overwatch::{
    overwatch::OverwatchHandle,
    services::{AsServiceId, status::ServiceStatus},
};

use crate::{OperationStatus, OperationStatusCode, StatusResult};

const READY_TIMEOUT: Duration = Duration::from_millis(100);

pub async fn ensure_ready<Service>(
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
) -> StatusResult<()>
where
    RuntimeServiceId: AsServiceId<Service>,
{
    let mut status_watcher =
        overwatch_handle
            .status_watcher::<Service>()
            .await
            .map_err(|error| {
                let name = <RuntimeServiceId as AsServiceId<Service>>::SERVICE_ID;
                OperationStatus::error(
                    OperationStatusCode::ServiceError,
                    format!("Failed to request {name} service status watcher: {error}"),
                )
            })?;

    status_watcher
        .wait_for(ServiceStatus::Ready, Some(READY_TIMEOUT))
        .await
        .map(|_| ())
        .map_err(|status| {
            let name = <RuntimeServiceId as AsServiceId<Service>>::SERVICE_ID;
            OperationStatus::error(
                OperationStatusCode::ServiceError,
                format!("Service {name} is not ready: {status}"),
            )
        })
}
