use std::collections::{HashMap, HashSet};

use lb_core::mantle::{NoteId, SignedMantleTx, Utxo};
use lb_key_management_system_service::keys::ZkPublicKey;
use thiserror::Error;

use crate::common::wallet::WalletId;

pub type WalletUtxos = HashMap<WalletId, Vec<Utxo>>;

/// A request to scan one wallet across one or more wallet public keys.
#[derive(Debug, Clone)]
pub struct WalletScanRequest {
    wallet_id: WalletId,
    wallet_pks: HashSet<ZkPublicKey>,
}

impl WalletScanRequest {
    #[must_use]
    pub fn new(
        wallet_id: impl Into<WalletId>,
        wallet_pks: impl IntoIterator<Item = ZkPublicKey>,
    ) -> Self {
        Self {
            wallet_id: wallet_id.into(),
            wallet_pks: wallet_pks.into_iter().collect(),
        }
    }

    #[must_use]
    pub const fn wallet_id(&self) -> &WalletId {
        &self.wallet_id
    }

    #[must_use]
    pub fn wallet_name(&self) -> &str {
        self.wallet_id.as_str()
    }
}

/// Scans chain transactions and maintains wallet UTXOs by note ID.
pub struct WalletChainScanner {
    requests: Vec<WalletScanRequest>,
    wallets_by_pk: HashMap<ZkPublicKey, WalletId>,
    utxos_by_wallet: HashMap<WalletId, HashMap<NoteId, Utxo>>,
}

impl WalletChainScanner {
    pub fn from_requests(requests: &[WalletScanRequest]) -> Result<Self, WalletSyncRequestError> {
        let mut wallet_pks: HashMap<WalletId, HashSet<ZkPublicKey>> = HashMap::new();
        let mut utxos_by_wallet = HashMap::new();

        for request in requests {
            wallet_pks
                .entry(request.wallet_id.clone())
                .or_default()
                .extend(request.wallet_pks.iter().copied());
            utxos_by_wallet
                .entry(request.wallet_id.clone())
                .or_default();
        }

        let wallets_by_pk = wallet_public_keys_by_owner(&wallet_pks)?;
        let requests = wallet_pks
            .into_iter()
            .map(|(wallet_id, wallet_pks)| WalletScanRequest {
                wallet_id,
                wallet_pks,
            })
            .collect();

        Ok(Self {
            requests,
            wallets_by_pk,
            utxos_by_wallet,
        })
    }

    #[must_use]
    pub fn requests(&self) -> &[WalletScanRequest] {
        &self.requests
    }

    #[must_use]
    pub const fn wallet_count(&self) -> usize {
        self.requests.len()
    }

    pub fn seed_wallet_from_cached_snapshot(&mut self, wallet_name: &str, cached_utxos: &[Utxo]) {
        let wallet_utxos = self
            .utxos_by_wallet
            .get_mut(wallet_name)
            .expect("wallet exists in chain scanner");
        wallet_utxos.clear();
        wallet_utxos.extend(cached_utxos.iter().map(|utxo| (utxo.id(), *utxo)));
    }

    pub fn seed_genesis_utxos(&mut self, genesis_utxos: &[Utxo]) {
        for utxo in genesis_utxos.iter().copied() {
            let Some(wallet_name) = self.wallets_by_pk.get(&utxo.note.pk) else {
                continue;
            };
            let Some(utxos_by_note) = self.utxos_by_wallet.get_mut(wallet_name) else {
                continue;
            };

            utxos_by_note.insert(utxo.id(), utxo);
        }
    }

    pub fn scan_transaction(&mut self, tx: &SignedMantleTx) -> WalletChainScan {
        let synced_outputs = self.scan_outputs(tx);
        let synced_spends = self.scan_inputs(tx);

        WalletChainScan {
            synced_outputs,
            synced_spends,
        }
    }

    pub fn wallet_utxos(&self) -> impl Iterator<Item = (&WalletId, Vec<Utxo>)> + '_ {
        self.utxos_by_wallet
            .iter()
            .map(|(wallet_id, utxos_by_note)| {
                (wallet_id, utxos_by_note.values().copied().collect())
            })
    }

    #[must_use]
    pub fn into_wallet_utxos(self) -> WalletUtxos {
        self.utxos_by_wallet
            .into_iter()
            .map(|(wallet_name, utxos_by_note)| {
                (wallet_name, utxos_by_note.into_values().collect())
            })
            .collect()
    }

    fn scan_outputs(&mut self, tx: &SignedMantleTx) -> Vec<WalletSyncedOutput> {
        let mut synced_outputs = Vec::new();

        for transfer in tx.mantle_tx.transfers() {
            for utxo in transfer.outputs.utxos(&transfer) {
                let Some(wallet_name) = self.wallets_by_pk.get(&utxo.note.pk) else {
                    continue;
                };
                let Some(utxos_by_note) = self.utxos_by_wallet.get_mut(wallet_name) else {
                    continue;
                };

                if utxos_by_note.insert(utxo.id(), utxo).is_none() {
                    synced_outputs.push(WalletSyncedOutput {
                        wallet_id: wallet_name.clone(),
                        utxo,
                    });
                }
            }
        }

        synced_outputs
    }

    fn scan_inputs(&mut self, tx: &SignedMantleTx) -> Vec<WalletSyncedSpend> {
        let mut synced_spends = Vec::new();

        for transfer in tx.mantle_tx.transfers() {
            for spent in &transfer.inputs {
                for (wallet_name, utxos_by_note) in &mut self.utxos_by_wallet {
                    if utxos_by_note.remove(spent).is_some() {
                        synced_spends.push(WalletSyncedSpend {
                            wallet_id: wallet_name.clone(),
                            note_id: *spent,
                        });
                    }
                }
            }
        }

        synced_spends
    }
}

#[derive(Debug, Clone)]
pub struct WalletChainScan {
    pub synced_outputs: Vec<WalletSyncedOutput>,
    pub synced_spends: Vec<WalletSyncedSpend>,
}

#[derive(Debug, Clone)]
pub struct WalletSyncedOutput {
    pub wallet_id: WalletId,
    pub utxo: Utxo,
}

#[derive(Debug, Clone)]
pub struct WalletSyncedSpend {
    pub wallet_id: WalletId,
    pub note_id: NoteId,
}

#[derive(Debug, Clone, Eq, Error, PartialEq)]
pub enum WalletSyncRequestError {
    #[error(
        "Multiple wallets have the same public key {public_key:?}: '{first_wallet}' and \
        '{second_wallet}'"
    )]
    DuplicatePublicKey {
        public_key: ZkPublicKey,
        first_wallet: WalletId,
        second_wallet: WalletId,
    },
}

fn wallet_public_keys_by_owner(
    wallet_pks: &HashMap<WalletId, HashSet<ZkPublicKey>>,
) -> Result<HashMap<ZkPublicKey, WalletId>, WalletSyncRequestError> {
    let mut wallets_by_pk: HashMap<ZkPublicKey, WalletId> = HashMap::new();

    for (wallet_name, pks) in wallet_pks {
        for pk in pks {
            if let Some(existing_wallet) = wallets_by_pk.get(pk) {
                return Err(WalletSyncRequestError::DuplicatePublicKey {
                    public_key: *pk,
                    first_wallet: existing_wallet.clone(),
                    second_wallet: wallet_name.clone(),
                });
            }
            wallets_by_pk.insert(*pk, wallet_name.clone());
        }
    }

    Ok(wallets_by_pk)
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::Note;

    use super::*;

    fn pk(value: u8) -> ZkPublicKey {
        ZkPublicKey::new(value.into())
    }

    fn utxo(value: u64, output_index: usize, pk: ZkPublicKey) -> Utxo {
        Utxo::new([output_index as u8; 32], output_index, Note::new(value, pk))
    }

    #[test]
    fn genesis_seed_keeps_outputs_for_all_requested_wallet_keys() {
        let wallet_id = WalletId::from("alice");
        let mut scanner = WalletChainScanner::from_requests(&[WalletScanRequest::new(
            wallet_id.clone(),
            [pk(1), pk(2)],
        )])
        .expect("scan request should be valid");

        scanner.seed_genesis_utxos(&[utxo(10, 0, pk(1)), utxo(20, 1, pk(2)), utxo(30, 2, pk(3))]);

        let wallet_utxos = scanner.into_wallet_utxos();
        let mut values = wallet_utxos
            .get(wallet_id.as_str())
            .expect("wallet should be present")
            .iter()
            .map(|utxo| utxo.note.value)
            .collect::<Vec<_>>();
        values.sort_unstable();

        assert_eq!(values, vec![10, 20]);
    }

    #[test]
    fn cached_snapshot_replaces_existing_scan_state() {
        let wallet_id = WalletId::from("alice");
        let mut scanner = WalletChainScanner::from_requests(&[WalletScanRequest::new(
            wallet_id.clone(),
            [pk(1)],
        )])
        .expect("scan request should be valid");

        scanner.seed_genesis_utxos(&[utxo(10, 0, pk(1))]);
        scanner.seed_wallet_from_cached_snapshot(wallet_id.as_str(), &[utxo(20, 1, pk(1))]);

        let wallet_utxos = scanner.into_wallet_utxos();
        let values = wallet_utxos
            .get(wallet_id.as_str())
            .expect("wallet should be present")
            .iter()
            .map(|utxo| utxo.note.value)
            .collect::<Vec<_>>();

        assert_eq!(values, vec![20]);
    }
}
