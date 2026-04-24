use lb_common_http_client::ApiBlock;
use lb_core::mantle::{Utxo, gas::MainnetGasConstants, tx_builder::MantleTxBuilder};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_testing_framework::NodeHttpClient;
use lb_wallet::{WalletError, WalletState};
use rpds::{HashTrieMapSync, HashTrieSetSync};

use crate::common::chain::scan_chain_until;

pub fn utxos_for_public_key(utxos: impl IntoIterator<Item = Utxo>, pk: ZkPublicKey) -> Vec<Utxo> {
    utxos
        .into_iter()
        .filter(|utxo| utxo.note.pk == pk)
        .collect()
}

pub fn fund_transfer_builder_from_utxos(
    available_utxos: Vec<Utxo>,
    tx_builder: &MantleTxBuilder,
    sender_public_key: ZkPublicKey,
) -> Result<MantleTxBuilder, WalletError> {
    wallet_state_from_utxos(available_utxos).fund_tx::<MainnetGasConstants>(
        tx_builder,
        sender_public_key,
        [sender_public_key],
    )
}

pub async fn current_utxos_for_public_key(
    client: &NodeHttpClient,
    genesis_utxos: &[Utxo],
    public_key: ZkPublicKey,
) -> Vec<Utxo> {
    let mut owned = utxos_for_public_key(genesis_utxos.iter().copied(), public_key)
        .into_iter()
        .map(|utxo| (utxo.id(), utxo))
        .collect::<std::collections::HashMap<_, _>>();

    let consensus = client
        .consensus_info()
        .await
        .expect("fetching consensus info should succeed");
    if consensus.height == 0 {
        return owned.into_values().collect();
    }

    let mut blocks = Vec::new();
    let mut scanned_blocks = std::collections::HashSet::new();

    let _: Option<()> = scan_chain_until(
        consensus.tip,
        &mut scanned_blocks,
        async |header_id| {
            client
                .block(&header_id)
                .await
                .expect("fetching storage block should succeed")
        },
        |block: &ApiBlock| {
            blocks.push(block.clone());

            None
        },
    )
    .await;

    for block in blocks.into_iter().rev() {
        for tx in &block.transactions {
            for transfer in tx.mantle_tx.transfers() {
                for input in transfer.inputs.as_vec() {
                    owned.remove(input);
                }

                for utxo in transfer.outputs.utxos(&transfer) {
                    if utxo.note.pk == public_key {
                        owned.insert(utxo.id(), utxo);
                    }
                }
            }
        }
    }

    owned.into_values().collect()
}

fn wallet_state_from_utxos(utxos: Vec<Utxo>) -> WalletState {
    let mut utxo_map = HashTrieMapSync::new_sync();
    let mut pk_index = HashTrieMapSync::new_sync();

    for utxo in utxos {
        let note_id = utxo.id();
        let pk = utxo.note.pk;
        utxo_map = utxo_map.insert(note_id, utxo);

        let note_set = pk_index
            .get(&pk)
            .cloned()
            .unwrap_or_else(HashTrieSetSync::new_sync)
            .insert(note_id);
        pk_index = pk_index.insert(pk, note_set);
    }

    WalletState {
        utxos: utxo_map,
        pk_index,
    }
}
