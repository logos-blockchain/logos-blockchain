pub mod bodies;
pub mod paths;
#[cfg(feature = "profiling")]
pub mod pprof;
pub mod settings;
pub mod utils;

#[cfg(all(feature = "profiling", target_os = "windows"))]
compile_error!(
    "The `profiling` feature is not supported on Windows since `pprof` is not available."
);
