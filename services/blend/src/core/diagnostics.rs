use lb_blend::proofs::quota::inputs::prove::public::LeaderInputs;
use lb_chain_service::Epoch;
use lb_log_targets::diagnostic::BLEND_REACHABILITY;
use tracing::{debug, error};

use super::LOG_TARGET;
use crate::epoch_info::PolEpochInfo;

pub(super) fn pol_state_matches(
    pol_info: &PolEpochInfo,
    blend_epoch: Epoch,
    blend_leadership_public_inputs: &LeaderInputs,
) -> bool {
    pol_info.epoch == blend_epoch
        && pol_info.state.nonce == blend_leadership_public_inputs.pol_epoch_nonce
        && pol_info.state.aged_utxo_root == blend_leadership_public_inputs.pol_ledger_aged
        && pol_info.state.lottery_0 == blend_leadership_public_inputs.lottery_0
        && pol_info.state.lottery_1 == blend_leadership_public_inputs.lottery_1
}

pub(super) fn log_pol_state_handoff(
    pol_info: &PolEpochInfo,
    blend_epoch: Epoch,
    blend_leadership_public_inputs: &LeaderInputs,
) {
    let state_matches = pol_state_matches(pol_info, blend_epoch, blend_leadership_public_inputs);
    macro_rules! log_handoff {
        ($level:ident) => {
            $level!(
                target: LOG_TARGET,
                diagnostic = BLEND_REACHABILITY,
                event = "blend_pol_state_handoff",
                epoch = u32::from(pol_info.epoch),
                blend_epoch = u32::from(blend_epoch),
                state_matches,
                pol_source_tip_id = %pol_info.state.source.tip_id,
                pol_source_tip_slot = u64::from(pol_info.state.source.tip_slot),
                pol_source_lib_id = %pol_info.state.source.lib_id,
                pol_source_lib_slot = u64::from(pol_info.state.source.lib_slot),
                pol_nonce = ?pol_info.state.nonce,
                blend_nonce = ?blend_leadership_public_inputs.pol_epoch_nonce,
                pol_aged_utxo_root = ?pol_info.state.aged_utxo_root,
                blend_aged_utxo_root = ?blend_leadership_public_inputs.pol_ledger_aged,
                pol_lottery_0 = ?pol_info.state.lottery_0,
                blend_lottery_0 = ?blend_leadership_public_inputs.lottery_0,
                pol_lottery_1 = ?pol_info.state.lottery_1,
                blend_lottery_1 = ?blend_leadership_public_inputs.lottery_1,
                "Compared private ChainLeader PoL state with public Blend epoch state"
            );
        };
    }
    if state_matches {
        log_handoff!(debug);
    } else {
        log_handoff!(error);
    }
}
