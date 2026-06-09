mod compressed_appender;
pub mod filter;
pub mod logging;
pub mod metrics;
pub mod tracing;

pub use opentelemetry;

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
