use std::ffi::{CStr, CString};

use lb_core::mantle::transactions::genesis_tx::ChainId;
use lb_node::RuntimeServiceId;
use overwatch::overwatch::{Overwatch, OverwatchHandle};
use tokio::runtime::{Handle, Runtime};

use crate::{
    errors::{OperationStatus, OperationStatusCode},
    logging,
    result::StatusResult,
};

type LogosBlockchainOverwatch = Overwatch<RuntimeServiceId>;

/// A running node.
///
/// C only ever holds a pointer to this, handed out by `start_lb_node` and
/// taken back by `shutdown_node`. It is deliberately not `#[repr(C)]`, so
/// `cbindgen` emits it as an opaque type: C code cannot copy it, build one of
/// its own or reach into its fields, all of which would leave it holding
/// pointers the node frees on shutdown.
pub struct LogosBlockchainNode {
    // Declared before `runtime` so that it is dropped first: the services stop
    // while the runtime they run on is still alive.
    overwatch: LogosBlockchainOverwatch,
    runtime: Runtime,
    // The chain ID of the deployment this node was started with. It is fixed
    // for the node's lifetime, so it is captured here at construction instead
    // of being queried from a running service. `None` when the chain ID cannot
    // be represented as a C string, which `get_chain_id` reports as an error
    // rather than failing node start.
    chain_id: Option<CString>,
}

impl LogosBlockchainNode {
    pub fn new(overwatch: LogosBlockchainOverwatch, runtime: Runtime, chain_id: &ChainId) -> Self {
        // A `ChainId` is only bounded and UTF-8, so nothing stops it from
        // carrying an interior NUL that no C string can hold. That is a broken
        // deployment rather than a reason to refuse to run, so the node starts
        // either way and `get_chain_id` is the one that reports the problem.
        let chain_id = CString::new(<_ as AsRef<str>>::as_ref(chain_id))
            .inspect_err(|error| {
                logging::error!(
                    "new",
                    "Chain ID {chain_id} cannot be represented as a C string: {error}. \
                     `get_chain_id` will fail for this node."
                );
            })
            .ok();

        Self {
            overwatch,
            runtime,
            chain_id,
        }
    }

    /// The chain ID of the deployment this node runs, borrowed from the node,
    /// or `None` when it is not representable as a C string.
    #[must_use]
    pub(crate) fn chain_id(&self) -> Option<&CStr> {
        self.chain_id.as_deref()
    }

    #[must_use]
    pub(crate) const fn get_overwatch_handle(&self) -> &OverwatchHandle<RuntimeServiceId> {
        self.overwatch.handle()
    }

    /// The handle the node functions block on.
    ///
    /// Fails when the calling thread cannot block: see
    /// [`ensure_blocking_allowed`].
    pub(crate) fn get_runtime_handle(&self) -> StatusResult<&Handle> {
        ensure_blocking_allowed()?;
        Ok(self.runtime.handle())
    }

    /// Shuts down the node and waits for all services to finish
    ///
    /// # Note
    ///
    /// Any raw pointers to [`LogosBlockchainNode`] will be invalidated after
    /// this call.
    pub(crate) fn shutdown(self) -> OperationStatus {
        let Self {
            overwatch, runtime, ..
        } = self;
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

/// Fails when the calling thread belongs to an async runtime.
///
/// Every node function is a synchronous wrapper that blocks on the node's
/// runtime, and blocking on a thread that is itself driving async tasks
/// panics. That is the thread subscription callbacks run on, so this is what
/// turns a call made from inside a callback into an error.
pub fn ensure_blocking_allowed() -> StatusResult<()> {
    if Handle::try_current().is_ok() {
        return Err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            "This function blocks and cannot be called from an async runtime thread, such as \
             from inside a subscription callback.",
        ));
    }
    Ok(())
}
