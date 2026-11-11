use std::time::{Duration, Instant};

use axum::{
    extract::{MatchedPath, Request},
    http::StatusCode,
    middleware::Next,
    response::Response,
};

pub async fn http_metrics_middleware(request: Request, next: Next) -> Response {
    let start = Instant::now();
    let method = request.method().as_str().to_owned();
    let endpoint = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| request.uri().path(), MatchedPath::as_str)
        .to_owned();

    log_request_start(&method, &endpoint);

    let response = next.run(request).await;
    let duration = start.elapsed();

    log_request_completion(&method, &endpoint, &response, duration);

    response
}

fn log_request_start(method: &str, endpoint: &str) {
    lb_tracing::increase_counter_u64!(http_requests_total, 1, method = method, endpoint = endpoint);
}

fn log_request_completion(method: &str, endpoint: &str, response: &Response, duration: Duration) {
    let status = response.status().as_u16();
    let status_class = get_status_class(response.status());

    if response.status() != StatusCode::OK {
        log_request_failure(method, endpoint, status);
        return;
    }

    lb_tracing::metric_histogram_f64!(
        http_request_duration_seconds,
        duration.as_secs_f64(),
        method = method,
        endpoint = endpoint,
        status = status,
        status_class = status_class
    );
}

fn get_status_class(status: StatusCode) -> &'static str {
    if status.is_success() {
        "2xx"
    } else if status.is_redirection() {
        "3xx"
    } else if status.is_client_error() {
        "4xx"
    } else if status.is_server_error() {
        "5xx"
    } else {
        "unknown"
    }
}

fn log_request_failure(method: &str, endpoint: &str, status: u16) {
    lb_tracing::increase_counter_u64!(
        http_requests_failed_total,
        1,
        method = method,
        endpoint = endpoint,
        status = status
    );
}
