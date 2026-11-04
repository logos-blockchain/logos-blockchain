//! Genesis, identities and funding prepared before any native service settings.
//! These are the current protocol types, not a new cross-client config schema.

use lb_core::{
    block::genesis::GenesisBlock,
    mantle::{GenesisTime, transactions::Distribution},
    sdp::{Locator, ServiceType},
};
use lb_key_management_system_service::keys::ZkKey;

use crate::{
    blend::{DEFAULT_BLEND_LISTENING_HOST, keys_from_id, listening_address},
    consensus::{
        BLEND_DECLARATION_AMOUNT, BaseConsensusMaterial, ProviderInfo, SdpFundingConfig,
        create_base_consensus_material_with_additional_wallet_outputs_and_sdp_funding_config,
        create_genesis_block_with_declarations,
    },
    funding::{fill_leader_stake, fund_wallets, refresh_service_note},
};

/// The network's initial state and the secrets needed by its participants.
/// Native API, storage, tracing and service settings are constructed
/// separately.
#[derive(Clone)]
pub struct PreparedNetwork {
    pub(crate) ids: Vec<[u8; 32]>,
    pub(crate) blend_ports: Vec<u16>,
    pub consensus: BaseConsensusMaterial,
    pub genesis: GenesisBlock,
}

impl PreparedNetwork {
    #[must_use]
    pub fn node_ids(&self) -> &[[u8; 32]] {
        &self.ids
    }
}

#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "Existing generation inputs also reserve space for wallets supplied later."
)]
pub fn prepare_network(
    ids: &[[u8; 32]],
    blend_ports: &[u16],
    blend_core_nodes: usize,
    reserved_wallet_outputs: usize,
    wallets: &[(ZkKey, u64)],
    sdp_funding: SdpFundingConfig,
    test_context: Option<&str>,
    genesis_time: GenesisTime,
) -> PreparedNetwork {
    assert_eq!(ids.len(), blend_ports.len(), "each node needs a Blend port");
    assert!(
        blend_core_nodes <= ids.len(),
        "Blend provider count exceeds node count"
    );
    let mut consensus =
        create_base_consensus_material_with_additional_wallet_outputs_and_sdp_funding_config(
            ids,
            reserved_wallet_outputs.max(wallets.len()),
            sdp_funding,
        );
    let mut transfer = Distribution::try_from(consensus.allocations.clone())
        .expect("the allocations fit the genesis transfer outputs");
    let leader_keys = consensus
        .regular_note_keys
        .iter()
        .map(ZkKey::to_public_key)
        .collect::<Vec<_>>();
    let funding_keys = consensus
        .sdp_notes
        .iter()
        .map(|note| note.pk)
        .collect::<Vec<_>>();
    fund_wallets(&mut transfer, &funding_keys, wallets, sdp_funding);

    let declared = BLEND_DECLARATION_AMOUNT
        * u64::try_from(blend_core_nodes).expect("the provider count fits in u64");
    fill_leader_stake(&mut transfer, &leader_keys, declared);

    for note in &mut consensus.sdp_notes {
        refresh_service_note(note, &transfer);
    }
    consensus.allocations = transfer.as_slice().to_vec();

    let providers = ids
        .iter()
        .zip(blend_ports)
        .take(blend_core_nodes)
        .map(|(id, port)| {
            let (provider_sk, zk_sk) = keys_from_id(id);
            ProviderInfo {
                service_type: ServiceType::BlendNetwork,
                provider_sk,
                zk_sk,
                locator: Locator::new_unchecked(listening_address(
                    DEFAULT_BLEND_LISTENING_HOST,
                    *port,
                )),
                amount: BLEND_DECLARATION_AMOUNT,
            }
        })
        .collect();
    let genesis =
        create_genesis_block_with_declarations(transfer, providers, test_context, genesis_time);

    PreparedNetwork {
        ids: ids.to_vec(),
        blend_ports: blend_ports.to_vec(),
        consensus,
        genesis,
    }
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::{
        OpRef,
        traits::{GenesisTx as _, MantleTx as _},
    };
    use lb_ledger::GENESIS_STAKE_SUPPLY;

    use super::*;

    fn genesis_supply(network: &PreparedNetwork) -> u128 {
        let tx = network.genesis.genesis_tx();
        let distributed = tx
            .distribution()
            .iter()
            .map(|allocation| u128::from(allocation.value))
            .sum::<u128>();
        let declared = tx
            .op_refs()
            .into_iter()
            .filter_map(|op| match op {
                OpRef::SDPDeclare(declaration) => Some(u128::from(declaration.amount)),
                _ => None,
            })
            .sum::<u128>();
        distributed + declared
    }

    fn prepare(wallets: &[(ZkKey, u64)]) -> PreparedNetwork {
        prepare_network(
            &[[1; 32], [2; 32], [3; 32]],
            &[3000, 3001, 3002],
            2,
            0,
            wallets,
            SdpFundingConfig::default(),
            None,
            GenesisTime::new(1000),
        )
    }

    // A test chain follows the supply rule of Mantle Ledger Initialization.
    #[test]
    fn a_prepared_network_holds_the_genesis_supply() {
        assert_eq!(
            genesis_supply(&prepare(&[])),
            u128::from(GENESIS_STAKE_SUPPLY)
        );
    }
}
