use opentelemetry::{
    Context,
    trace::{SpanContext, SpanId, TraceContextExt as _, TraceFlags, TraceId, TraceState},
};

pub mod prelude {
    pub use tracing_opentelemetry::OpenTelemetrySpanExt;
}

// In some places it makes sense to use third party ids such as blob_id or tx_id
// as a tracing id. This allows to track the time during which the entity is
// propagated throughout the system.
//
// Opentelemetry tracing standard has a specific remote context format which is
// supported by most tracing software.
// More information at https://www.w3.org/TR/trace-context/#traceparent-header
#[must_use]
pub fn remote_parent(id: impl AsRef<[u8]>) -> (TraceId, Context) {
    let id = id.as_ref();
    let mut trace_id = [0u8; 16];
    let len = id.len().min(16);
    trace_id[..len].copy_from_slice(&id[..len]);
    let span_id: [u8; 8] = id
        .get(16..24)
        .and_then(|tail| tail.try_into().ok())
        .filter(|tail| tail != &[0u8; 8])
        .unwrap_or([0, 0, 0, 0, 0, 0, 0, 1]);
    let trace_id = TraceId::from_bytes(trace_id);
    let span_context = SpanContext::new(
        trace_id,
        SpanId::from_bytes(span_id),
        TraceFlags::SAMPLED,
        true,
        TraceState::default(),
    );
    (
        trace_id,
        Context::new().with_remote_span_context(span_context),
    )
}

#[macro_export]
macro_rules! event_with_id {
    ($level:expr, $id:expr, $msg:literal $(, $($fields:tt)+)?) => {{
        use $crate::tracing::macros::prelude::OpenTelemetrySpanExt as _;

        let (trace_id, parent) = $crate::tracing::macros::remote_parent(&$id);
        let span = ::tracing::span!($level, $msg, trace_id = %trace_id);
        let _ = span.set_parent(parent);
        let _entered = span.enter();
        ::tracing::event!($level, trace_id = %trace_id, $($($fields)+,)? $msg);
    }};
}

#[macro_export]
macro_rules! info_with_id {
    ($id:expr, $msg:literal $(, $($fields:tt)+)?) => {
        $crate::event_with_id!(::tracing::Level::INFO, $id, $msg $(, $($fields)+)?)
    };
}

#[macro_export]
macro_rules! error_with_id {
    ($id:expr, $msg:literal $(, $($fields:tt)+)?) => {
        $crate::event_with_id!(::tracing::Level::ERROR, $id, $msg $(, $($fields)+)?)
    };
}
