use lb_cryptarchia_engine::Cryptarchia;
use lb_cryptarchia_sync::HeaderId;
use lb_ledger::Ledger;

pub fn emit_consensus_metrics(consensus: &Cryptarchia<HeaderId>, ledger: &Ledger<HeaderId>) {
    let tip_branch = *consensus.tip_branch();
    let lib_branch = *consensus.lib_branch();

    let height = tip_branch.length();
    let finalized_height = lib_branch.length();
    let current_slot = tip_branch.slot();
    let current_epoch = ledger.config().epoch(current_slot);
    let forks_count = consensus.branches().branches().count();

    lb_tracing::metric_gauge_u64!(consensus_tip_height, height as usize);
    lb_tracing::metric_gauge_u64!(consensus_finalized_height, finalized_height as usize);
    lb_tracing::metric_gauge_u64!(consensus_current_epoch, u32::from(current_epoch));
    lb_tracing::metric_gauge_u64!(consensus_current_slot, u64::from(current_slot));
    lb_tracing::metric_gauge_u64!(consensus_forks_count, forks_count);
}

pub fn emit_block_imported_metric() {
    lb_tracing::increase_counter_u64!(consensus_blocks_imported_total, 1);
}

pub fn emit_block_transactions_metric(tx_count: usize) {
    lb_tracing::increase_counter_u64!(consensus_block_transactions_total, tx_count);
}
