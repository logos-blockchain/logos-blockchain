pub mod backend;
mod browser_access;
pub mod calls;
mod errors;
pub mod handlers;
mod metrics;
mod openapi;
#[cfg(feature = "profiling")]
mod pprof;
mod queries;
mod responses;
mod routes;
pub mod serializers;
pub mod settings;

#[cfg(all(feature = "profiling", target_os = "windows"))]
compile_error!(
    "The `profiling` feature is not supported on Windows since `pprof` is not available."
);
