use axum::{
    Json, Router,
    body::Body,
    http::{StatusCode, header},
    response::{IntoResponse as _, Response},
    routing,
};
use lb_log_targets::api;
use serde::Serialize;
use tikv_jemalloc_ctl::{epoch, stats};

const LOG_TARGET: &str = api::http::PPROF;

/// Routes for inspecting jemalloc heap usage.
///
/// - `/debug/pprof/heap`: live heap profile in pprof format, by allocation call
///   stack. Requires jemalloc profiling to be active (`prof:true,
///   prof_active:true` in `MALLOC_CONF`, the default of the `heap-profiling`
///   node build).
/// - `/debug/memory/allocator`: jemalloc totals in bytes.
///
/// ```bash
/// curl -s localhost:8080/debug/pprof/heap > heap-t0.pb.gz
/// # later
/// curl -s localhost:8080/debug/pprof/heap > heap-t1.pb.gz
/// go tool pprof -top -base heap-t0.pb.gz logos-blockchain-node heap-t1.pb.gz
/// ```
pub fn create_heap_profile_router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/debug/pprof/heap", routing::get(heap_profile))
        .route("/debug/memory/allocator", routing::get(allocator_stats))
}

pub async fn heap_profile() -> Response {
    let Some(prof_ctl) = jemalloc_pprof::PROF_CTL.as_ref() else {
        return (
            StatusCode::NOT_FOUND,
            "jemalloc heap profiling is not available; start with MALLOC_CONF=prof:true",
        )
            .into_response();
    };
    let mut prof_ctl = prof_ctl.lock().await;
    if !prof_ctl.activated() {
        return (
            StatusCode::CONFLICT,
            "jemalloc heap profiling is not active; start with MALLOC_CONF=prof:true,prof_active:true",
        )
            .into_response();
    }

    match prof_ctl.dump_pprof() {
        Ok(profile) => {
            tracing::debug!(
                target: LOG_TARGET,
                "Heap profile generated, size: {} bytes",
                profile.len()
            );
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .header(
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"heap.pb.gz\"",
                )
                .body(Body::from(profile))
                .unwrap_or_else(|_| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to build response",
                    )
                        .into_response()
                })
        }
        Err(e) => {
            tracing::error!(target: LOG_TARGET, "Failed to dump heap profile: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to dump heap profile: {e}"),
            )
                .into_response()
        }
    }
}

/// jemalloc totals in bytes. `allocated` is live application data; the gap
/// between it and `resident` is allocator overhead and fragmentation.
#[derive(Serialize)]
pub struct AllocatorStats {
    pub allocated: usize,
    pub active: usize,
    pub metadata: usize,
    pub resident: usize,
    pub mapped: usize,
    pub retained: usize,
}

fn read_allocator_stats() -> Result<AllocatorStats, tikv_jemalloc_ctl::Error> {
    // jemalloc caches its statistics; advancing the epoch refreshes them.
    epoch::advance()?;
    Ok(AllocatorStats {
        allocated: stats::allocated::read()?,
        active: stats::active::read()?,
        metadata: stats::metadata::read()?,
        resident: stats::resident::read()?,
        mapped: stats::mapped::read()?,
        retained: stats::retained::read()?,
    })
}

pub async fn allocator_stats() -> Response {
    match read_allocator_stats() {
        Ok(stats) => Json(stats).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to read jemalloc stats: {e}"),
        )
            .into_response(),
    }
}
