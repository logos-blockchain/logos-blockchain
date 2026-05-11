pub fn activity_posts_total() {
    lb_tracing::increase_counter_u64!(sdp_activity_posts_total, 1);
}

pub fn activity_tx_failures_total() {
    lb_tracing::increase_counter_u64!(sdp_activity_tx_failures_total, 1);
}

pub fn activity_mempool_failures_total() {
    lb_tracing::increase_counter_u64!(sdp_activity_mempool_failures_total, 1);
}

pub fn activity_success_total() {
    lb_tracing::increase_counter_u64!(sdp_activity_success_total, 1);
}

pub fn declarations_total() {
    lb_tracing::increase_counter_u64!(sdp_declarations_total, 1);
}

pub fn declaration_tx_failures_total() {
    lb_tracing::increase_counter_u64!(sdp_declaration_tx_failures_total, 1);
}

pub fn declaration_mempool_failures_total() {
    lb_tracing::increase_counter_u64!(sdp_declaration_mempool_failures_total, 1);
}

pub fn declaration_success_total() {
    lb_tracing::increase_counter_u64!(sdp_declaration_success_total, 1);
}

pub fn withdrawals_total() {
    lb_tracing::increase_counter_u64!(sdp_withdrawals_total, 1);
}

pub fn withdrawal_validation_failures_total() {
    lb_tracing::increase_counter_u64!(sdp_withdrawal_validation_failures_total, 1);
}

pub fn withdrawal_tx_failures_total() {
    lb_tracing::increase_counter_u64!(sdp_withdrawal_tx_failures_total, 1);
}

pub fn withdrawal_mempool_failures_total() {
    lb_tracing::increase_counter_u64!(sdp_withdrawal_mempool_failures_total, 1);
}

pub fn withdrawal_success_total() {
    lb_tracing::increase_counter_u64!(sdp_withdrawal_success_total, 1);
}
