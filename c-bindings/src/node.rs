use std::{ffi::c_void, mem::ManuallyDrop};

use lb_node::RuntimeServiceId;
use overwatch::overwatch::{Overwatch, OverwatchHandle};
use tokio::runtime::{Handle, Runtime};

use crate::{
    errors::{OperationStatus, OperationStatusCode},
    logging,
};

// Define an opaque type for the complex Overwatch type
type LogosBlockchainOverwatch = Overwatch<RuntimeServiceId>;

#[repr(C)]
pub struct LogosBlockchainNode {
    // Use opaque pointers instead of the generic types. cbindgen renders these
    // as `void*`, keeping `LogosBlockchainNode` a plain opaque handle in the C
    // API. Typed fields (e.g. `OwnedPointer<Overwatch<RuntimeServiceId>>`) leak
    // internal Rust type names into the generated header and break the C build.
    overwatch: *mut c_void,
    // Keep simple types as-is
    runtime: *mut c_void,
}

impl LogosBlockchainNode {
    pub fn new(overwatch: LogosBlockchainOverwatch, runtime: Runtime) -> Self {
        Self {
            // Box the complex types and convert to opaque pointers
            overwatch: Box::into_raw(Box::new(overwatch)).cast::<c_void>(),
            runtime: Box::into_raw(Box::new(runtime)).cast::<c_void>(),
        }
    }

    // Helper methods to safely access the inner types
    #[must_use]
    pub(crate) const fn get_overwatch_handle(&self) -> &OverwatchHandle<RuntimeServiceId> {
        unsafe {
            self.overwatch
                .cast::<LogosBlockchainOverwatch>()
                .as_ref()
                .expect("A valid `LogosBlockchainOverwatch` not null pointer")
        }
        .handle()
    }

    #[must_use]
    pub(crate) fn get_runtime_handle(&self) -> &Handle {
        unsafe {
            self.runtime
                .cast::<Runtime>()
                .as_ref()
                .expect("A valid `tokio::Runtime` not null pointer")
        }
        .handle()
    }

    /// Gets ownership of the inner [`LogosBlockchainOverwatch`] and [`Runtime`]
    /// instances. Wrapping `self` in [`ManuallyDrop`] prevents `Drop` from
    /// freeing the pointers we just moved into the returned boxes.
    #[must_use]
    pub fn into_parts(self) -> (Box<LogosBlockchainOverwatch>, Box<Runtime>) {
        let this = ManuallyDrop::new(self);
        let overwatch = unsafe { Box::from_raw(this.overwatch.cast::<LogosBlockchainOverwatch>()) };
        let runtime = unsafe { Box::from_raw(this.runtime.cast::<Runtime>()) };
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
        overwatch.blocking_wait_finished();
        OperationStatus::OK
    }
}

// Implement Drop to prevent memory leaks
impl Drop for LogosBlockchainNode {
    fn drop(&mut self) {
        if self.overwatch.is_null() {
            logging::error!(
                "drop",
                "Attempted to drop a null overwatch pointer. This is a bug"
            );
        }
        if self.runtime.is_null() {
            logging::error!(
                "drop",
                "Attempted to drop a null tokio runtime pointer. This is a bug"
            );
        }
        drop(unsafe { Box::from_raw(self.overwatch.cast::<LogosBlockchainOverwatch>()) });
        drop(unsafe { Box::from_raw(self.runtime.cast::<Runtime>()) });
    }
}
