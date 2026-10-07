//! Genesis, identities and funding prepared before any native service settings.
//! These are the current protocol types, not a new cross-client config schema.

use lb_core::{
    block::genesis::GenesisBlock,
    mantle::GenesisTime,
    sdp::{Locator, ServiceType},
};
use lb_key_management_system_service::keys::ZkKey;

use crate::{
    blend::{DEFAULT_BLEND_LISTENING_HOST, keys_from_id, listening_address},
    consensus::{
        BaseConsensusMaterial, ProviderInfo, SdpFundingConfig,
        create_base_consensus_material_with_additional_wallet_outputs_and_sdp_funding_config,
        create_genesis_block, create_genesis_block_with_declarations,
    },
    funding::{fund_wallets, refresh_service_note},
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
    let genesis = create_genesis_block(&consensus.utxos, test_context, genesis_time);
    let mut transfer = genesis.genesis_tx().transfer().operation().clone();
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
    fund_wallets(
        &mut transfer,
        &leader_keys,
        &funding_keys,
        wallets,
        sdp_funding,
    );

    for note in consensus
        .blend_notes
        .iter_mut()
        .chain(&mut consensus.sdp_notes)
    {
        refresh_service_note(note, &transfer);
    }
    consensus.utxos = transfer
        .outputs
        .iter()
        .enumerate()
        .map(|(i, _)| {
            transfer
                .utxo_by_index(i)
                .expect("genesis output must exist")
        })
        .collect();

    let providers = ids
        .iter()
        .zip(blend_ports)
        .enumerate()
        .take(blend_core_nodes)
        .map(|(i, (id, port))| {
            let (provider_sk, zk_sk) = keys_from_id(id);
            ProviderInfo {
                service_type: ServiceType::BlendNetwork,
                provider_sk,
                zk_sk,
                locator: Locator::new_unchecked(listening_address(
                    DEFAULT_BLEND_LISTENING_HOST,
                    *port,
                )),
                note: consensus.blend_notes[i].clone(),
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
