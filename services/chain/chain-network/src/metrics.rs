use std::time::Duration;

use lb_chain_service::api::ApiError;
use overwatch::DynError;

use crate::Error;

pub fn consensus_proposals_received_total(origin: &'static str) {
    lb_tracing::increase_counter_u64!(consensus_proposals_received_total, 1, origin = origin);
}

pub fn consensus_proposals_ignored_total(reason: &'static str, origin: &'static str) {
    lb_tracing::increase_counter_u64!(
        consensus_proposals_ignored_total,
        1,
        reason = reason,
        origin = origin
    );
}

pub fn consensus_apply_block_failed_total(reason: &'static str) {
    lb_tracing::increase_counter_u64!(consensus_apply_block_failed_total, 1, reason = reason);
}

pub fn consensus_observe_apply_block_ok(duration: Duration) {
    lb_tracing::metric_histogram_f64!(consensus_apply_block_seconds, duration.as_secs_f64());
}

pub fn consensus_observe_apply_block_err(err: &Error) {
    let reason = match err {
        Error::Cryptarchia(ApiError::ParentMissing { .. }) => "parent_missing",
        _ => "other",
    };
    consensus_apply_block_failed_total(reason);
}

pub fn consensus_observe_proposal_reconstruct_ok(duration: Duration) {
    lb_tracing::metric_histogram_f64!(
        consensus_proposal_reconstruct_seconds,
        duration.as_secs_f64()
    );
}

pub fn consensus_observe_proposal_reconstruct_err(origin: &'static str, err: &Error) {
    let reason = match err {
        Error::MissingMempoolTransactions(_) => "missing_txs",
        Error::Mempool(_) => "mempool",
        Error::InvalidBlock(_) => "invalid_block",
        _ => "other",
    };

    lb_tracing::increase_counter_u64!(
        consensus_proposal_reconstruct_failed_total,
        1,
        reason = reason,
        origin = origin
    );
}

pub fn consensus_observe_proposal_missing_txs(count: usize) {
    lb_tracing::metric_histogram_u64!(consensus_proposal_missing_txs, count as u64);
}

pub fn orphan_blocks_queue_full_total() {
    lb_tracing::increase_counter_u64!(orphan_blocks_queue_full_total, 1);
}

pub fn orphan_blocks_enqueued_total() {
    lb_tracing::increase_counter_u64!(orphan_blocks_enqueued_total, 1);
}

pub fn orphan_blocks_pending(count: usize) {
    lb_tracing::metric_gauge_u64!(orphan_blocks_pending, count as u64);
}

pub fn orphan_observe_parent_fetch_ok(duration: Duration) {
    lb_tracing::metric_histogram_f64!(orphan_blocks_parent_fetch_seconds, duration.as_secs_f64());
}

pub fn orphan_observe_parent_fetch_err() {
    lb_tracing::increase_counter_u64!(orphan_blocks_parent_fetch_failed_total, 1);
}

pub fn orphan_blocks_removed_total() {
    lb_tracing::increase_counter_u64!(orphan_blocks_removed_total, 1);
}

pub fn orphan_blocks_replaced_total() {
    lb_tracing::increase_counter_u64!(orphan_blocks_replaced_total, 1);
}

pub fn orphan_blocks_received_total() {
    lb_tracing::increase_counter_u64!(orphan_blocks_received_total, 1);
}

pub fn orphan_blocks_fetch_failed_total() {
    lb_tracing::increase_counter_u64!(orphan_blocks_fetch_failed_total, 1);
}

/// Enqueue was refused because the block (or its parent) is in the
/// rejected-blocks negative cache.
pub fn orphan_blocks_enqueue_rejected_total() {
    lb_tracing::increase_counter_u64!(orphan_blocks_enqueue_rejected_total, 1);
}

/// A queued orphan was dropped at dequeue time because it had been marked
/// rejected after enqueue.
pub fn orphan_blocks_dequeue_rejected_total() {
    lb_tracing::increase_counter_u64!(orphan_blocks_dequeue_rejected_total, 1);
}

/// A block id was inserted into the rejected-blocks negative cache.
pub fn orphan_blocks_rejected_inserted_total() {
    lb_tracing::increase_counter_u64!(orphan_blocks_rejected_inserted_total, 1);
}

pub fn tip_poll_triggered_total() {
    lb_tracing::increase_counter_u64!(tip_poll_triggered_total, 1);
}

pub fn tip_poll_enqueued_total() {
    lb_tracing::increase_counter_u64!(tip_poll_enqueued_total, 1);
}

pub fn chainsync_observe_request_tip_ok(duration: Duration) {
    lb_tracing::increase_counter_u64!(chainsync_requests_total, 1, kind = "tip", result = "ok");
    lb_tracing::metric_histogram_f64!(chainsync_request_tip_seconds, duration.as_secs_f64());
}

pub fn chainsync_observe_request_tip_err() {
    lb_tracing::increase_counter_u64!(chainsync_requests_total, 1, kind = "tip", result = "err");
}

pub fn chainsync_observe_request_tip<T>(
    duration: Duration,
    result: Result<T, DynError>,
) -> Result<T, DynError> {
    if result.is_ok() {
        chainsync_observe_request_tip_ok(duration);
    } else {
        chainsync_observe_request_tip_err();
    }
    result
}
