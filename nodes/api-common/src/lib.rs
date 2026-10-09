use time::UtcDateTime;

pub mod bodies;
pub mod metrics;
pub mod paths;
#[cfg(feature = "profiling")]
pub mod pprof;
pub mod queries;
pub mod settings;

#[cfg(all(feature = "profiling", target_os = "windows"))]
compile_error!(
    "The `profiling` feature is not supported on Windows since `pprof` is not available."
);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct TimeInfo {
    /// When the chain started, in RFC 3339.
    #[serde(with = "rfc3339_utc")]
    #[schema(value_type = String, format = DateTime)]
    pub genesis_time: UtcDateTime,
    pub current_slot: u64,
    pub current_epoch: u32,
}

/// A UTC date-time in RFC 3339, which `time` only provides for an
/// `OffsetDateTime`.
mod rfc3339_utc {
    use serde::{Deserializer, Serializer};
    use time::{OffsetDateTime, UtcDateTime};

    pub fn serialize<S>(datetime: &UtcDateTime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        time::serde::rfc3339::serialize(&OffsetDateTime::from(*datetime), serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<UtcDateTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        time::serde::rfc3339::deserialize(deserializer).map(OffsetDateTime::to_utc)
    }
}

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
