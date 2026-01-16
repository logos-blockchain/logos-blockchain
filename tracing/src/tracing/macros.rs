pub mod prelude {
    use std::collections::HashMap;

    pub use opentelemetry::{
        Context, global,
        trace::{SpanContext, SpanId, TraceFlags, TraceId, TraceState},
    };
    pub use rand;
    pub use tracing::Span;
    pub use tracing_opentelemetry::OpenTelemetrySpanExt;

    // In some places it makes sense to use third party ids such as blob_id or tx_id
    // as a tracing id. This allows to track the time during which the entity is
    // propagated throughout the system.
    //
    // Opentelemetry tracing standard has a specific remote context format which is
    // supported by most tracing software.
    // More information at https://www.w3.org/TR/trace-context/#traceparent-header
    #[must_use]
    pub fn set_remote_context(trace_id: TraceId, span_id: SpanId) -> HashMap<String, String> {
        let mut carrier = HashMap::new();
        carrier.insert(
            "traceparent".to_owned(),
            format!("00-{trace_id}-{span_id}-01"),
        );

        carrier
    }
}

#[macro_export]
macro_rules! info_with_id {
    ($idbytes:expr, $msg:expr $(, $key:ident = $value:expr)*) => {{
        use $crate::tracing::macros::prelude::*;
        use std::convert::TryInto;

        let trace_id = TraceId::from_bytes($idbytes[..16].try_into().unwrap());
        let span_id = SpanId::from_bytes(rand::random::<[u8; 8]>());

        let parent_context = global::get_text_map_propagator(|propagator| {
            propagator.extract(&set_remote_context(trace_id, span_id))
        });

        let current_span = Span::current();
        current_span.set_parent(parent_context);

        tracing::info!(
            trace_id = %trace_id,
            $msg $(, $key = $value)*
        );
    }};
}

#[macro_export]
macro_rules! error_with_id {
    ($idbytes:expr, $msg:expr $(, $key:ident = $value:expr)*) => {{
        use $crate::tracing::macros::prelude::*;
        use std::convert::TryInto;

        let trace_id = TraceId::from_bytes($idbytes[..16].try_into().unwrap());
        let span_id = SpanId::from_bytes(rand::random::<[u8; 8]>());

        let parent_context = global::get_text_map_propagator(|propagator| {
            propagator.extract(&set_remote_context(trace_id, span_id))
        });

        let current_span = Span::current();
        current_span.set_parent(parent_context);

        tracing::error!(
            trace_id = %trace_id,
            $msg $(, $key = $value)*
        );
    }};
}
