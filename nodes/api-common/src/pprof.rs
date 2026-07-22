use std::time::Duration;

use axum::{
    Router,
    body::Body,
    extract::Query,
    http::{StatusCode, header},
    response::{IntoResponse as _, Response},
    routing,
};
use lb_log_targets::api;
use pprof::protos::Message as _;
use serde::Deserialize;

const LOG_TARGET: &str = api::http::PPROF;

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum ProfileFormat {
    Svg,
    #[default]
    Proto,
}

#[derive(Deserialize)]
pub struct PprofParams {
    pub seconds: Option<u64>,
    pub frequency: Option<i32>,
    pub format: Option<ProfileFormat>,
}

/// Creates the profiling router with CPU profiling endpoint.
///
/// # Available Endpoints
/// - `/debug/pprof/profile` - CPU profiling with multiple output formats
///   (flamegraphs, protobuf)
///
/// # References
/// - [pprof format specification](https://github.com/google/pprof/blob/main/doc/README.md)
/// - [Go net/http/pprof](https://pkg.go.dev/net/http/pprof)
pub fn create_pprof_router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new().route("/debug/pprof/profile", routing::get(cpu_profile))
}

/// CPU profiling endpoint that samples the call stack to identify performance
/// bottlenecks.
///
/// This endpoint uses statistical sampling to capture CPU usage patterns across
/// the application. The profiler interrupts execution at regular intervals to
/// record the call stack, building a statistical model of where CPU time is
/// spent.
///
/// # Information Provided
/// - **Function call frequency**: Which functions consume the most CPU time
/// - **Call stack relationships**: How functions call each other
///   (caller/callee)
/// - **Hot paths**: The most frequently executed code paths in the application
/// - **Resource bottlenecks**: Functions that may need optimization
///
/// # Output Formats
/// - `svg` (default): Interactive flamegraph for browser viewing
/// - `proto`: Google pprof protobuf format for analysis tools
///
/// # Query Parameters
/// - `seconds` (default: 30): Profiling duration in seconds
/// - `frequency` (default: 1000): Sampling frequency in Hz (samples per second)
/// - `format` (default: proto): Output format (svg|proto)
///
/// # Usage Examples
/// ```bash
/// # Generate flamegraph (viewable in browser)
/// curl http://localhost:8722/debug/pprof/profile > profile.svg
///
/// # Get protobuf for Go pprof tools
/// go tool pprof -http=:8080 "http://127.0.0.1:8722/debug/pprof/profile?seconds=15&format=proto"
/// ```
///
/// # References
/// - [Brendan Gregg's Flamegraph Guide](http://www.brendangregg.com/flamegraphs.html)
/// - [Google pprof documentation](https://github.com/google/pprof/blob/main/doc/README.md)
/// - [pprof crate documentation](https://docs.rs/pprof/latest/pprof/)
pub async fn cpu_profile(Query(params): Query<PprofParams>) -> Response {
    let duration_secs = params.seconds.unwrap_or(30);
    let frequency = params.frequency.unwrap_or(1000);
    let format = params.format.unwrap_or_default();

    match pprof::ProfilerGuard::new(frequency) {
        Ok(guard) => {
            tokio::time::sleep(Duration::from_secs(duration_secs)).await;

            match guard.report().build() {
                Ok(report) => {
                    tracing::debug!(target: LOG_TARGET, "CPU profile report built successfully");

                    match format {
                        ProfileFormat::Svg => generate_flamegraph_svg(&report),
                        ProfileFormat::Proto => generate_protobuf_profile(&report),
                    }
                }
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to build profile report: {e}"),
                )
                    .into_response(),
            }
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to start profiler: {e}"),
        )
            .into_response(),
    }
}

fn generate_flamegraph_svg(report: &pprof::Report) -> Response {
    let mut flamegraph_data = Vec::new();
    match report.flamegraph(&mut flamegraph_data) {
        Ok(()) => {
            tracing::debug!(
                target: LOG_TARGET,
                "Flamegraph generated, size: {} bytes",
                flamegraph_data.len()
            );

            if flamegraph_data.is_empty() {
                tracing::warn!(
                    target: LOG_TARGET,
                    "Generated flamegraph is empty - no samples collected"
                );

                (
                    StatusCode::OK,
                    "No samples collected during profiling period",
                )
                    .into_response()
            } else {
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "image/svg+xml")
                    .body(Body::from(flamegraph_data))
                    .unwrap_or_else(|_| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Failed to build response",
                        )
                            .into_response()
                    })
            }
        }
        Err(e) => {
            tracing::error!(target: LOG_TARGET, "Failed to generate flamegraph: {}", e);

            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to generate flamegraph: {e}"),
            )
                .into_response()
        }
    }
}

fn generate_protobuf_profile(report: &pprof::Report) -> Response {
    match report.pprof() {
        Ok(profile) => {
            let mut data = Vec::new();

            match profile.write_to_vec(&mut data) {
                Ok(()) => {
                    tracing::debug!(
                        target: LOG_TARGET,
                        "Protobuf profile generated, size: {} bytes",
                        data.len()
                    );

                    Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_TYPE, "application/octet-stream")
                        .header(
                            header::CONTENT_DISPOSITION,
                            "attachment; filename=\"profile.pb\"",
                        )
                        .body(Body::from(data))
                        .unwrap_or_else(|_| {
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "Failed to build response",
                            )
                                .into_response()
                        })
                }
                Err(e) => {
                    tracing::error!(
                        target: LOG_TARGET,
                        "Failed to encode protobuf profile: {}",
                        e
                    );

                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to encode protobuf: {e}"),
                    )
                        .into_response()
                }
            }
        }
        Err(e) => {
            tracing::error!(
                target: LOG_TARGET,
                "Failed to generate protobuf profile: {}",
                e
            );

            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to generate protobuf: {e}"),
            )
                .into_response()
        }
    }
}
