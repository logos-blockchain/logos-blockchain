use std::{
    ffi::{CStr, CString, c_char, c_void},
    mem::ManuallyDrop,
    ptr,
};

use lb_core::mantle::transactions::genesis_tx::ChainId;
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
    // The chain ID of the deployment this node was started with. It is fixed
    // for the node's lifetime, so it is captured here at construction instead
    // of being queried from a running service. Owned by this struct; freed on
    // drop. Null when the chain ID cannot be represented as a C string, which
    // `get_chain_id` reports as an error rather than failing node start.
    chain_id: *mut c_char,
}

impl LogosBlockchainNode {
    pub fn new(overwatch: LogosBlockchainOverwatch, runtime: Runtime, chain_id: &ChainId) -> Self {
        // A `ChainId` is only bounded and UTF-8, so nothing stops it from
        // carrying an interior NUL that no C string can hold. That is a broken
        // deployment rather than a reason to refuse to run, so the node starts
        // either way and `get_chain_id` is the one that reports the problem.
        let chain_id = CString::new(<_ as AsRef<str>>::as_ref(chain_id)).map_or_else(
            |error| {
                logging::error!(
                    "new",
                    "Chain ID {chain_id} cannot be represented as a C string: {error}. \
                     `get_chain_id` will fail for this node."
                );
                ptr::null_mut()
            },
            CString::into_raw,
        );

        Self {
            // Box the complex types and convert to opaque pointers
            overwatch: Box::into_raw(Box::new(overwatch)).cast::<c_void>(),
            runtime: Box::into_raw(Box::new(runtime)).cast::<c_void>(),
            chain_id,
        }
    }

    /// The chain ID of the deployment this node runs, borrowed from the node,
    /// or `None` when it is not representable as a C string.
    #[must_use]
    pub(crate) fn chain_id(&self) -> Option<&CStr> {
        (!self.chain_id.is_null()).then(|| unsafe { CStr::from_ptr(self.chain_id) })
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
    /// freeing the pointers we just moved into the returned boxes. The chain
    /// ID is not part of the returned pair, so it is released here.
    #[must_use]
    pub fn into_parts(self) -> (Box<LogosBlockchainOverwatch>, Box<Runtime>) {
        let this = ManuallyDrop::new(self);
        let overwatch = unsafe { Box::from_raw(this.overwatch.cast::<LogosBlockchainOverwatch>()) };
        let runtime = unsafe { Box::from_raw(this.runtime.cast::<Runtime>()) };
        if !this.chain_id.is_null() {
            drop(unsafe { CString::from_raw(this.chain_id) });
        }
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
        // A null chain ID is a represented state, not a bug: see `new`.
        if !self.chain_id.is_null() {
            drop(unsafe { CString::from_raw(self.chain_id) });
        }
    }
}
