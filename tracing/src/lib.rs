mod compressed_appender;
pub mod filter;
pub mod logging;
pub mod metrics;
pub mod tracing;

pub use opentelemetry;
use serde::{Deserialize, Serialize};
use url::Url;

#[macro_export]
macro_rules! increase_counter_u64 {
    ($name:ident, $value:expr $(, $k:ident = $v:expr)* $(,)?) => {{
        let attributes = &[$($crate::metrics::emit::key_value(stringify!($k), $v),)*];
        $crate::metrics::emit::increase_counter_u64(stringify!($name), $value, attributes);
    }};
}

#[macro_export]
macro_rules! metric_counter_f64 {
    ($name:ident, $value:expr $(, $k:ident = $v:expr)* $(,)?) => {{
        let attributes = &[$($crate::metrics::emit::key_value(stringify!($k), $v),)*];
        $crate::metrics::emit::counter_f64(stringify!($name), $value, attributes);
    }};
}

#[macro_export]
macro_rules! metric_gauge_u64 {
    ($name:ident, $value:expr $(, $k:ident = $v:expr)* $(,)?) => {{
        let attributes = &[$($crate::metrics::emit::key_value(stringify!($k), $v),)*];
        $crate::metrics::emit::gauge_u64(stringify!($name), $value, attributes);
    }};
}

#[macro_export]
macro_rules! metric_observable_gauge_u64_set {
    ($name:ident, $value:expr) => {{
        $crate::metrics::emit::observable_gauge_u64_set(stringify!($name), $value);
    }};
}

#[macro_export]
macro_rules! metric_observable_gauge_u64_clear {
    ($name:ident) => {{
        $crate::metrics::emit::observable_gauge_u64_clear(stringify!($name));
    }};
}

#[macro_export]
macro_rules! metric_histogram_u64 {
    ($name:ident, $value:expr $(, $k:ident = $v:expr)* $(,)?) => {{
        let attributes = &[$($crate::metrics::emit::key_value(stringify!($k), $v),)*];
        $crate::metrics::emit::histogram_u64(stringify!($name), $value, attributes);
    }};
}

#[macro_export]
macro_rules! metric_histogram_f64 {
    ($name:ident, $value:expr $(, $k:ident = $v:expr)* $(,)?) => {{
        let attributes = &[$($crate::metrics::emit::key_value(stringify!($k), $v),)*];
        $crate::metrics::emit::histogram_f64(stringify!($name), $value, attributes);
    }};
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub enum OtlpProtocol {
    #[default]
    Grpc,
    Http,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OtlpServiceConfig {
    pub url: Url,
    pub authorization_header: Option<String>,
    pub protocol: OtlpProtocol,
    pub service_name: String,
}
