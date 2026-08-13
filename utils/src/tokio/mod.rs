pub mod stream;

pub mod task {
    use core::{
        pin::Pin,
        task::{Context, Poll},
    };

    use tokio::{
        runtime::Handle,
        task::{JoinError, JoinHandle},
    };

    /// A [`JoinHandle`] that cancels its task when dropped.
    ///
    /// A plain handle *detaches* on drop, so work started for a consumer that
    /// has since gone away runs to completion anyway. Wrapping the handle ties
    /// the task's lifetime to the interest in its result, which is what lets a
    /// dropped future or stream stop paying for a computation nobody will read.
    /// Awaiting is unchanged; only the drop behaviour differs.
    ///
    /// Cancellation takes effect at the task's next await point, so a task is
    /// stopped *between* the pieces of blocking work it awaits rather than
    /// inside them: a [`spawn_blocking`] closure that has already started
    /// cannot be interrupted, and wrapping its handle only keeps one that has
    /// not started yet from starting at all.
    pub struct CancellableHandle<T>(JoinHandle<T>);

    impl<T> CancellableHandle<T> {
        #[must_use]
        pub const fn new(handle: JoinHandle<T>) -> Self {
            Self(handle)
        }
    }

    impl<T> From<JoinHandle<T>> for CancellableHandle<T> {
        fn from(handle: JoinHandle<T>) -> Self {
            Self::new(handle)
        }
    }

    impl<T> Drop for CancellableHandle<T> {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    impl<T> Future for CancellableHandle<T> {
        type Output = Result<T, JoinError>;

        fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
            Pin::new(&mut self.0).poll(context)
        }
    }

    #[expect(
        unexpected_cfgs,
        reason = "tokio_unstable is supplied externally through RUSTFLAGS"
    )]
    #[expect(clippy::allow_attributes, reason = "cfg-selected spawn implementation")]
    #[allow(clippy::needless_return, reason = "cfg-selected spawn implementation")]
    pub fn spawn<T>(
        name: &'static str,
        future: impl Future<Output = T> + Send + 'static,
    ) -> JoinHandle<T>
    where
        T: Send + 'static,
    {
        #[cfg(all(feature = "tokio-task-names", tokio_unstable))]
        {
            return tokio::task::Builder::new()
                .name(name)
                .spawn(future)
                .unwrap_or_else(|_| panic!("failed to spawn named Tokio task `{name}`"));
        }

        #[cfg(not(all(feature = "tokio-task-names", tokio_unstable)))]
        {
            let _ = name;
            tokio::spawn(future)
        }
    }

    #[expect(
        unexpected_cfgs,
        reason = "tokio_unstable is supplied externally through RUSTFLAGS"
    )]
    #[expect(clippy::allow_attributes, reason = "cfg-selected spawn implementation")]
    #[allow(clippy::needless_return, reason = "cfg-selected spawn implementation")]
    pub fn spawn_blocking<T>(
        name: &'static str,
        function: impl FnOnce() -> T + Send + 'static,
    ) -> JoinHandle<T>
    where
        T: Send + 'static,
    {
        #[cfg(all(feature = "tokio-task-names", tokio_unstable))]
        {
            return tokio::task::Builder::new()
                .name(name)
                .spawn_blocking(function)
                .unwrap_or_else(|_| panic!("failed to spawn named Tokio blocking task `{name}`"));
        }

        #[cfg(not(all(feature = "tokio-task-names", tokio_unstable)))]
        {
            let _ = name;
            tokio::task::spawn_blocking(function)
        }
    }

    #[expect(
        unexpected_cfgs,
        reason = "tokio_unstable is supplied externally through RUSTFLAGS"
    )]
    #[expect(clippy::allow_attributes, reason = "cfg-selected spawn implementation")]
    #[allow(clippy::needless_return, reason = "cfg-selected spawn implementation")]
    pub fn spawn_on<T>(
        runtime: &Handle,
        name: &'static str,
        future: impl Future<Output = T> + Send + 'static,
    ) -> JoinHandle<T>
    where
        T: Send + 'static,
    {
        #[cfg(all(feature = "tokio-task-names", tokio_unstable))]
        {
            return tokio::task::Builder::new()
                .name(name)
                .spawn_on(future, runtime)
                .unwrap_or_else(|_| panic!("failed to spawn named Tokio task `{name}`"));
        }

        #[cfg(not(all(feature = "tokio-task-names", tokio_unstable)))]
        {
            let _ = name;
            runtime.spawn(future)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{spawn, spawn_blocking, spawn_on};

        #[test]
        fn spawn_forms_preserve_join_handle_results() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("test runtime should build");
            let handle = runtime.handle().clone();

            runtime.block_on(async move {
                assert_eq!(spawn("test/async", async { 1 }).await.unwrap(), 1);
                assert_eq!(spawn_blocking("test/blocking", || 2).await.unwrap(), 2);
                assert_eq!(
                    spawn_on(&handle, "test/explicit", async { 3 })
                        .await
                        .unwrap(),
                    3
                );
            });
        }
    }
}
