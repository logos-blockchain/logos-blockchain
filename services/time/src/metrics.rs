pub fn time_current_slot(slot: u64) {
    lb_tracing::metric_gauge_u64!(time_current_slot, slot);
}

pub fn time_current_epoch(epoch: u32) {
    lb_tracing::metric_gauge_u64!(time_current_epoch, epoch);
}

pub fn time_broadcast_errors() {
    lb_tracing::increase_counter_u64!(time_broadcast_errors_total, 1);
}
