pub mod stream;

pub mod task {
    use tokio::{runtime::Handle, task::JoinHandle};

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
