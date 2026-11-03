use std::collections::{HashMap, HashSet};

use lb_core::mantle::transactions::{Distribution, GenesisAllocation, UNITS_PER_LGO};
use lb_key_management_system_service::keys::{ZkKey, ZkPublicKey};

use crate::consensus::{GENESIS_TRANSFER_OUTPUT_LIMIT, SdpFundingConfig, ServiceNote};

/// Adds scenario wallets and adjusts leader stake and SDP funding before
/// genesis is signed. This operates on protocol data, without native node
/// configuration.
pub fn fund_wallets(
    transfer: &mut Distribution,
    leader_keys: &[ZkPublicKey],
    funding_keys: &[ZkPublicKey],
    wallets: &[(ZkKey, u64)],
    sdp_funding: SdpFundingConfig,
) {
    if wallets.is_empty() || leader_keys.is_empty() {
        return;
    }

    let funding_keys = funding_keys.iter().copied().collect();
    fit_sdp_funding_outputs_to_genesis_capacity(
        transfer,
        &funding_keys,
        wallets.len(),
        sdp_funding,
    );
    let total = wallets.iter().map(|(_, value)| *value).sum();
    let stake = leader_stake_amount(total, leader_keys.len());

    for output in transfer.iter_mut() {
        if leader_keys.contains(&output.public_key) {
            output.value = stake;
        }
    }
    for (key, value) in wallets {
        transfer
            .try_push(GenesisAllocation {
                value: *value,
                public_key: key.to_public_key(),
            })
            .expect("wallet account outputs must fit transfer output bounds");
    }
}

/// Funding can move outputs. Refresh references before building provider
/// declarations or handing notes to an adapter.
pub fn refresh_service_note(note: &mut ServiceNote, transfer: &Distribution) {
    let (index, output) = transfer
        .iter()
        .enumerate()
        .find(|(_, output)| output.public_key == note.pk)
        .expect("service note must be present in genesis");
    note.output_index = index;
    note.note = output.note(index);
}

#[must_use]
pub fn leader_stake_amount(total_wallet_funds: u64, n_participants: usize) -> u64 {
    if total_wallet_funds == 0 {
        return 100_000 * UNITS_PER_LGO;
    }

    let n = n_participants.max(1) as u64;
    let scaled = total_wallet_funds
        .saturating_mul(10)
        .saturating_div(n)
        .max(1);
    scaled.max(100_000 * UNITS_PER_LGO)
}

fn fit_sdp_funding_outputs_to_genesis_capacity(
    transfer_op: &mut Distribution,
    funding_keys: &HashSet<ZkPublicKey>,
    additional_wallet_outputs: usize,
    sdp_funding_config: SdpFundingConfig,
) {
    let n_participants = funding_keys.len();
    let current_sdp_outputs = transfer_op
        .iter()
        .filter(|note| funding_keys.contains(&note.public_key))
        .count();
    let non_sdp_outputs = transfer_op.len() - current_sdp_outputs;
    let required_non_sdp_outputs = non_sdp_outputs
        .checked_add(additional_wallet_outputs)
        .expect("genesis output count overflow while fitting SDP funding outputs");
    let available_for_sdp = GENESIS_TRANSFER_OUTPUT_LIMIT
        .checked_sub(required_non_sdp_outputs)
        .unwrap_or_else(|| {
            panic!(
                "genesis transfer output capacity exhausted before SDP funding outputs: limit={GENESIS_TRANSFER_OUTPUT_LIMIT}, non_sdp_outputs={non_sdp_outputs}, additional_wallet_outputs={additional_wallet_outputs}",
            )
        });
    let max_sdp_notes_per_node = available_for_sdp / n_participants;
    let selected_sdp_notes_per_node =
        max_sdp_notes_per_node.min(sdp_funding_config.target_notes_per_node);
    let required_sdp_outputs = n_participants
        .checked_mul(selected_sdp_notes_per_node)
        .expect("SDP funding output count overflow while fitting genesis capacity");

    assert!(
        selected_sdp_notes_per_node > 0,
        "genesis transfer output capacity cannot provide one SDP funding note per node: limit={GENESIS_TRANSFER_OUTPUT_LIMIT}, node_count={n_participants}, non_sdp_outputs={non_sdp_outputs}, additional_wallet_outputs={additional_wallet_outputs}",
    );
    assert!(
        current_sdp_outputs >= required_sdp_outputs,
        "genesis contains too few SDP funding outputs for the selected split: current={current_sdp_outputs}, required={required_sdp_outputs}",
    );

    let mut retained_per_key = HashMap::new();
    if current_sdp_outputs > required_sdp_outputs {
        let retained_notes = transfer_op
            .iter()
            .copied()
            .filter_map(|mut note| {
                if !funding_keys.contains(&note.public_key) {
                    return Some(note);
                }

                let retained = retained_per_key.entry(note.public_key).or_insert(0);
                if *retained >= selected_sdp_notes_per_node {
                    return None;
                }
                note.value = sdp_funding_note_value(
                    sdp_funding_config,
                    *retained,
                    selected_sdp_notes_per_node,
                );
                *retained += 1;
                Some(note)
            })
            .collect::<Vec<_>>();
        *transfer_op = Distribution::try_from(retained_notes)
            .expect("trimmed genesis transfer outputs must fit the output bound");
    } else {
        for note in transfer_op.iter_mut() {
            if funding_keys.contains(&note.public_key) {
                let retained = retained_per_key.entry(note.public_key).or_insert(0);
                note.value = sdp_funding_note_value(
                    sdp_funding_config,
                    *retained,
                    selected_sdp_notes_per_node,
                );
                *retained += 1;
            }
        }
    }
}

fn sdp_funding_note_value(
    sdp_funding_config: SdpFundingConfig,
    note_index: usize,
    note_count: usize,
) -> u64 {
    let note_count = u64::try_from(note_count).expect("SDP funding split count should fit in u64");
    let base_value = sdp_funding_config.total_value_per_node / note_count;
    let remainder = sdp_funding_config.total_value_per_node % note_count;
    base_value
        + u64::from(
            u64::try_from(note_index).expect("SDP note index should fit in u64") < remainder,
        )
}
