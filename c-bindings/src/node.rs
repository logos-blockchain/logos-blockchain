use lb_node::RuntimeServiceId;
use overwatch::overwatch::{Overwatch, OverwatchHandle};
use tokio::runtime::{Handle, Runtime};

use crate::{
    errors::{OperationStatus, OperationStatusCode},
    pointers::OwnedPointer,
};

// Define an opaque type for the complex Overwatch type
type LogosBlockchainOverwatch = Overwatch<RuntimeServiceId>;

#[repr(C)]
pub struct LogosBlockchainNode {
    overwatch: OwnedPointer<LogosBlockchainOverwatch>,
    runtime: OwnedPointer<Runtime>,
}

impl LogosBlockchainNode {
    pub fn new(overwatch: LogosBlockchainOverwatch, runtime: Runtime) -> Self {
        Self {
            overwatch: OwnedPointer::new(overwatch),
            runtime: OwnedPointer::new(runtime),
        }
    }

    /// Gets the [`LogosBlockchainOverwatch`] instance
    pub(crate) fn get_overwatch(&self) -> &LogosBlockchainOverwatch {
        &self.overwatch
    }

    /// Gets an [`OverwatchHandle`] to the [`LogosBlockchainOverwatch`] instance
    #[must_use]
    pub(crate) fn get_overwatch_handle(&self) -> &OverwatchHandle<RuntimeServiceId> {
        self.get_overwatch().handle()
    }

    /// Gets the runtime's [`Handle`]
    #[must_use]
    pub(crate) fn get_runtime_handle(&self) -> &Handle {
        self.runtime.handle()
    }

    /// Gets ownership of the inner [`LogosBlockchainOverwatch`] and [`Runtime`]
    /// instances
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        OwnedPointer<LogosBlockchainOverwatch>,
        OwnedPointer<Runtime>,
    ) {
        let Self { overwatch, runtime } = self;
        (overwatch, runtime)
    }

    /// Shuts down the node and waits for all services to finish
    ///
    /// # Note
    ///
    /// Any raw pointers to [`LogosBlockchainNode`] will be invalidated after
    /// this call.
    pub(crate) fn shutdown(self) -> OperationStatus {
        let (overwatch, runtime) = self.into_parts();
        if let Err(error) = runtime.handle().block_on(overwatch.handle().shutdown()) {
            return OperationStatus::error(
                OperationStatusCode::ShutdownError,
                format!("Failed to shut down node: {error}"),
            );
        }
        overwatch.into_box().blocking_wait_finished();
        OperationStatus::OK
    }
}
