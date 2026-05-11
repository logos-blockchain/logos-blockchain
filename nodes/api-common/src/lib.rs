pub mod bodies;
pub mod metrics;
pub mod paths;
#[cfg(feature = "profiling")]
pub mod pprof;
pub mod settings;

#[cfg(all(feature = "profiling", target_os = "windows"))]
compile_error!(
    "The `profiling` feature is not supported on Windows since `pprof` is not available."
);

/// This maximum blocks stream chunk size is a happy medium between performance
/// and memory use
pub const MAX_BLOCKS_STREAM_CHUNK_SIZE: usize = 1_000;
/// This is a safe default chunk size for streaming blocks, allowing for
/// efficient delivery without overburdening the server or client.
pub const DEFAULT_BLOCKS_STREAM_CHUNK_SIZE: usize = 100;
/// 200 years worth of blocks if 1 is produced every 10s
pub const MAX_BLOCKS_STREAM_BLOCKS: usize = 630_720_000;
/// This is a safe default number of blocks to present the canonical chain
/// at the tip but not too much to overburden a client.
pub const DEFAULT_NUMBER_OF_BLOCKS_TO_STREAM: usize = 100;
