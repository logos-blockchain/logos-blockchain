use std::time::Instant;

pub fn storage_request_failed() {
    lb_tracing::increase_counter_u64!(storage_request_failed_total, 1);
}

pub fn storage_observe_request_ok(started_at: Instant) {
    lb_tracing::metric_histogram_f64!(
        storage_request_duration_seconds,
        started_at.elapsed().as_secs_f64()
    );
}
