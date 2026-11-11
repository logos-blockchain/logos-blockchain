use lb_core::{era::ForkDigest, mantle::transactions::genesis_tx::ChainId};
use lb_node::RuntimeServiceId;
use overwatch::overwatch::{Overwatch, ServicePanic};
use tokio::runtime::{Handle, Runtime};

use crate::{
    errors::{OperationStatus, OperationStatusCode},
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
///
/// The pointer is owned by the caller until it is passed to `shutdown_node`.
/// These rules apply to every function that takes one:
///
/// - It may be used from several threads at the same time.
/// - No call may be in progress on any thread when `shutdown_node` is called,
///   and none may be made afterwards: the handle is freed there.
/// - It must not be used from inside a subscription callback. Such calls fail
///   with a `RuntimeError` status.
pub struct LogosBlockchainNode {
    // Declared before `runtime` so that it is dropped first: the services stop
    // while the runtime they run on is still alive.
    overwatch: LogosBlockchainOverwatch,
    runtime: Runtime,
    // What the API calls work with: the node's services and the constants of
    // the deployment it was started with.
    handle: lb_node::api::calls::Handle,
}

impl LogosBlockchainNode {
    pub fn new(
        overwatch: LogosBlockchainOverwatch,
        runtime: Runtime,
        chain_id: ChainId,
        fork_digests: Vec<ForkDigest>,
    ) -> Self {
        let handle =
            lb_node::api::calls::Handle::new(overwatch.handle().clone(), chain_id, fork_digests);
        Self {
            overwatch,
            runtime,
            handle,
        }
    }

    #[must_use]
    pub(crate) const fn get_overwatch_handle(&self) -> &lb_node::api::calls::Handle {
        &self.handle
    }

    /// Runs a call into the node's API to completion, reporting its failure
    /// as a status that starts with `context`.
    pub(crate) fn call<Output>(
        &self,
        context: &str,
        call: impl Future<Output = Result<Output, lb_node::api::calls::Error>>,
    ) -> StatusResult<Output> {
        self.get_runtime_handle()?
            .block_on(call)
            .map_err(|error| OperationStatus::from_api_error(context, &error))
    }

    /// The handle the node functions block on.
    ///
    /// Fails when the calling thread cannot block (see
    /// [`ensure_blocking_allowed`]) and when the node has stopped (see
    /// [`Self::ensure_running`]).
    pub(crate) fn get_runtime_handle(&self) -> StatusResult<&Handle> {
        ensure_blocking_allowed()?;
        self.ensure_running()?;
        Ok(self.runtime.handle())
    }

    /// Fails when Overwatch is no longer running.
    ///
    /// Overwatch shuts itself down when a service panics. From then on every
    /// request to a service fails, each in its own way; asking Overwatch first
    /// turns all of them into one clear status. The reason it stopped is only
    /// known once it is waited for, which is what `shutdown_node` does.
    fn ensure_running(&self) -> StatusResult<()> {
        if self.is_running() {
            return Ok(());
        }
        Err(OperationStatus::error(
            OperationStatusCode::NodeStopped,
            "The node is no longer running, most likely because one of its services panicked. \
             Call `shutdown_node` to learn why and to release it.",
        ))
    }

    /// Whether Overwatch still answers.
    ///
    /// It has no query for this, so it is asked for something it can always
    /// answer while it runs: the request only fails when the command channel
    /// is closed or the reply is dropped, and both mean it is gone.
    fn is_running(&self) -> bool {
        self.runtime
            .handle()
            .block_on(self.overwatch.handle().retrieve_service_ids())
            .is_ok()
    }

    /// Shuts down the node and waits for all services to finish
    ///
    /// # Note
    ///
    /// Any raw pointers to [`LogosBlockchainNode`] will be invalidated after
    /// this call.
    pub(crate) fn shutdown(self) -> OperationStatus {
        // A failed request is only a problem if Overwatch is still running.
        // If it already stopped on its own there was nothing left to ask for,
        // and the reason is waiting to be collected below.
        if let Err(error) = self
            .runtime
            .handle()
            .block_on(self.overwatch.handle().shutdown())
            && self.is_running()
        {
            return OperationStatus::error(
                OperationStatusCode::ShutdownError,
                format!("Failed to shut down node: {error}"),
            );
        }
        let Self {
            overwatch, runtime, ..
        } = self;
        let exit = overwatch.blocking_wait_finished();
        drop(runtime);
        exit_status(&exit)
    }
}

/// What `shutdown_node` reports for the way Overwatch finished.
///
/// A node that stopped because a service panicked is released like any other,
/// but the caller is told: until this point all it could see was that the
/// node had stopped.
pub fn exit_status(exit: &Result<(), ServicePanic<RuntimeServiceId>>) -> OperationStatus {
    match exit {
        Ok(()) => OperationStatus::OK,
        Err(panic) => OperationStatus::error(
            OperationStatusCode::NodeStopped,
            format!("The node had already stopped: {panic}."),
        ),
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
