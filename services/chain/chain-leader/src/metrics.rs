pub fn consensus_proposals_created_local() {
    lb_tracing::increase_counter_u64!(consensus_proposals_created_total, 1, origin = "local");
}

pub fn consensus_proposals_create_failed() {
    lb_tracing::increase_counter_u64!(
        consensus_proposals_create_failed_total,
        1,
        reason = "propose_block"
    );
}
