use lb_core::{
    block::BlockNumber,
    crypto::{Digest as _, Hash, Hasher},
    header::EpochStateRoot,
    sdp::ServiceType,
};
use lb_groth16::{Fr, fr_to_bytes};
use strum::IntoEnumIterator as _;

use crate::{
    Config, WINDOW_SIZE, cryptarchia::LedgerState as CryptarchiaLedger,
    mantle::LedgerState as MantleLedger,
};

// Commitment to the settled epoch state, as specified in
// <https://lip.logos.co/blockchain/raw/cryptarchia-v1-protocol.html>.
// It is a snapshot of the roots below, taken at the epoch boundary right after
// the settlement has been applied and before the first block's transactions are
// executed, and repeated unchanged in every header of the epoch.
// It must be called on the settled state, that is once the boundary settlement
// is complete and before the block being applied has contributed its entropy,
// its voucher or anything else of its own.
pub fn get_epoch_state_root(
    cryptarchia: &CryptarchiaLedger,
    mantle: &MantleLedger,
    block_number: BlockNumber,
    config: &Config,
) -> EpochStateRoot {
    let epoch_state = cryptarchia.epoch_state();
    let channels = mantle.channels();

    let mut h = Hasher::new();
    h.update(b"STATE_ROOT_V1");
    h.update(fr_to_bytes(&cryptarchia.latest_utxos().root()));
    h.update(fr_to_bytes(&cryptarchia.aged_utxos().root()));
    h.update(channels.channels_root());
    h.update(channels.channel_notes_root());
    h.update(mantle.locked_notes().root());
    h.update(mantle.sdp.declarations_root());
    h.update(epoch_state.active_declarations.root());
    h.update(config.sdp_config.min_stake.threshold.to_le_bytes());
    // Sorted on the service.
    let mut service_types = ServiceType::iter().collect::<Vec<_>>();
    service_types.sort_unstable_by_key(ServiceType::to_byte);
    for service_type in service_types {
        let inactivity_period = config
            .sdp_config
            .service_params
            .get(&service_type)
            .map_or(0, |params| {
                params.inactivity_period.into_inner().into_inner()
            });
        h.update(inactivity_period.to_le_bytes());
    }
    h.update(fr_to_bytes(&Fr::from(*mantle.vouchers_snapshot_root())));
    h.update(mantle.leaders.nullifiers_root());
    h.update(mantle.leaders.claimable_rewards().to_le_bytes());
    h.update(block_number.to_le_bytes());
    for index in 0..WINDOW_SIZE {
        h.update(
            cryptarchia
                .get_fee_from_index(index)
                .into_inner()
                .to_le_bytes(),
        );
    }
    h.update(cryptarchia.execution_base_fee().into_inner().to_le_bytes());
    h.update(
        cryptarchia
            .average_execution_gas()
            .into_inner()
            .to_le_bytes(),
    );
    h.update(cryptarchia.storage_gas_price().into_inner().to_le_bytes());
    h.update(cryptarchia.storage_gas_ema().into_inner().to_le_bytes());
    h.update(fr_to_bytes(&epoch_state.nonce));
    h.update(fr_to_bytes(&cryptarchia.nonce));
    h.update(epoch_state.total_stake.to_le_bytes());

    EpochStateRoot::from(Hash::from(h.finalize()))
}

#[cfg(test)]
mod tests {
    use lb_core::sdp::ServiceParameters;
    use lb_groth16::Field as _;

    use super::*;
    use crate::{
        LedgerState,
        cryptarchia::tests::{config, utxo},
    };

    fn root_of(state: &LedgerState, block_number: BlockNumber, config: &Config) -> EpochStateRoot {
        get_epoch_state_root(
            &state.cryptarchia_ledger,
            &state.mantle_ledger,
            block_number,
            config,
        )
    }

    #[test]
    fn root_binds_the_state_the_nonce_and_the_parameters() {
        let config = config();
        let state = LedgerState::from_utxos([utxo()], &config);
        let root = root_of(&state, 0, &config);

        // A different note set.
        let other_notes = LedgerState::from_utxos([utxo(), utxo()], &config);
        assert_ne!(root_of(&other_notes, 0, &config), root);

        // A different running nonce.
        let mut other_nonce = state.clone();
        other_nonce.cryptarchia_ledger.nonce += Fr::ONE;
        assert_ne!(root_of(&other_nonce, 0, &config), root);

        // A different block number.
        assert_ne!(root_of(&state, 1, &config), root);

        // A different fee window.
        let mut other_fees = state.clone();
        other_fees
            .cryptarchia_ledger
            .update_fee_window(0, 1u64.into());
        assert_ne!(root_of(&other_fees, 0, &config), root);

        // A different minimum stake.
        let mut other_config = config.clone();
        other_config.sdp_config.min_stake.threshold += 1;
        assert_ne!(root_of(&state, 0, &other_config), root);

        // A different inactivity period.
        let mut other_config = config;
        other_config.sdp_config.service_params = std::sync::Arc::new(
            [(
                ServiceType::BlendNetwork,
                ServiceParameters {
                    inactivity_period: 3.try_into().unwrap(),
                    epoch: 0.into(),
                },
            )]
            .into(),
        );
        assert_ne!(root_of(&state, 0, &other_config), root);
    }
}
