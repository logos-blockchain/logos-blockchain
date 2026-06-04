use std::collections::{HashMap, HashSet};

use lb_core::mantle::{NoteId, SignedMantleTx, Utxo, ops::Op};
use lb_key_management_system_service::keys::ZkPublicKey;
use thiserror::Error;

use crate::common::wallet::WalletId;

pub type WalletUtxos = HashMap<WalletId, Vec<Utxo>>;

/// Wallet public keys whose chain UTXOs should be tracked as one test wallet.
#[derive(Debug, Clone)]
pub struct TrackedWalletKeys {
    wallet_id: WalletId,
    wallet_pks: HashSet<ZkPublicKey>,
}

impl TrackedWalletKeys {
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

    pub(crate) fn extend_if_same_wallet(&mut self, other: &Self) -> bool {
        if self.wallet_id != other.wallet_id {
            return false;
        }

        self.wallet_pks.extend(other.wallet_pks.iter().copied());
        true
    }
}

/// Wallet UTXO state derived from chain transactions by note ID.
pub struct WalletChainState {
    tracked_wallets: Vec<TrackedWalletKeys>,
    wallets_by_pk: HashMap<ZkPublicKey, WalletId>,
    utxos_by_wallet: HashMap<WalletId, HashMap<NoteId, Utxo>>,
    locked_note_ids: HashSet<NoteId>,
}

impl WalletChainState {
    pub fn from_tracked_wallets(
        tracked_wallets: &[TrackedWalletKeys],
    ) -> Result<Self, TrackedWalletKeysError> {
        let mut wallet_pks: HashMap<WalletId, HashSet<ZkPublicKey>> = HashMap::new();
        let mut utxos_by_wallet = HashMap::new();

        for tracked_wallet in tracked_wallets {
            wallet_pks
                .entry(tracked_wallet.wallet_id.clone())
                .or_default()
                .extend(tracked_wallet.wallet_pks.iter().copied());
            utxos_by_wallet
                .entry(tracked_wallet.wallet_id.clone())
                .or_default();
        }

        let wallets_by_pk = wallet_public_keys_by_owner(&wallet_pks)?;
        let tracked_wallets = wallet_pks
            .into_iter()
            .map(|(wallet_id, wallet_pks)| TrackedWalletKeys {
                wallet_id,
                wallet_pks,
            })
            .collect();

        Ok(Self {
            tracked_wallets,
            wallets_by_pk,
            utxos_by_wallet,
            locked_note_ids: HashSet::new(),
        })
    }

    #[must_use]
    pub fn tracked_wallets(&self) -> &[TrackedWalletKeys] {
        &self.tracked_wallets
    }

    #[must_use]
    pub fn covers_tracked_wallets(&self, tracked_wallets: &[TrackedWalletKeys]) -> bool {
        tracked_wallets.iter().all(|tracked_wallet| {
            self.tracked_wallets.iter().any(|known_wallet| {
                known_wallet.wallet_id == tracked_wallet.wallet_id
                    && tracked_wallet
                        .wallet_pks
                        .is_subset(&known_wallet.wallet_pks)
            })
        })
    }

    #[must_use]
    pub const fn wallet_count(&self) -> usize {
        self.tracked_wallets.len()
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

    pub fn apply_transaction(&mut self, tx: &SignedMantleTx) -> ObservedWalletChanges {
        let mut changes = ObservedWalletChanges::default();

        for op in tx.mantle_tx.ops() {
            self.apply_op(op, &mut changes);
        }

        changes
    }

    pub fn wallet_utxos(&self) -> impl Iterator<Item = (&WalletId, Vec<Utxo>)> + '_ {
        self.utxos_by_wallet
            .iter()
            .map(|(wallet_id, utxos_by_note)| (wallet_id, self.available_utxos(utxos_by_note)))
    }

    #[must_use]
    pub fn into_wallet_utxos(self) -> WalletUtxos {
        let locked_note_ids = self.locked_note_ids;

        self.utxos_by_wallet
            .into_iter()
            .map(|(wallet_name, utxos_by_note)| {
                let available_utxos = utxos_by_note
                    .into_iter()
                    .filter_map(|(note_id, utxo)| {
                        (!locked_note_ids.contains(&note_id)).then_some(utxo)
                    })
                    .collect();

                (wallet_name, available_utxos)
            })
            .collect()
    }

    pub fn from_wallet_utxos(
        tracked_wallets: &[TrackedWalletKeys],
        wallet_utxos: WalletUtxos,
    ) -> Result<Self, TrackedWalletKeysError> {
        let mut state = Self::from_tracked_wallets(tracked_wallets)?;

        for (wallet_id, utxos) in wallet_utxos {
            let Some(utxos_by_note) = state.utxos_by_wallet.get_mut(&wallet_id) else {
                continue;
            };

            utxos_by_note.extend(utxos.into_iter().map(|utxo| (utxo.id(), utxo)));
        }

        Ok(state)
    }

    fn apply_op(&mut self, op: &Op, changes: &mut ObservedWalletChanges) {
        match op {
            Op::Transfer(transfer) => {
                self.apply_spent_note_ids(
                    transfer.inputs.iter().copied(),
                    &mut changes.observed_spends,
                );
                self.apply_owned_outputs(
                    transfer.outputs.utxos(transfer),
                    &mut changes.observed_outputs,
                );
            }
            Op::ChannelDeposit(deposit) => {
                self.apply_spent_note_ids(
                    deposit.inputs.iter().copied(),
                    &mut changes.observed_spends,
                );
            }
            Op::ChannelWithdraw(withdraw) => {
                self.apply_owned_outputs(
                    withdraw.outputs.utxos(withdraw),
                    &mut changes.observed_outputs,
                );
            }
            Op::SDPDeclare(declaration) => self.lock_note(declaration.locked_note_id),
            Op::SDPWithdraw(withdrawal) => self.unlock_note(withdrawal.locked_note_id),
            Op::ChannelConfig(_)
            | Op::ChannelInscribe(_)
            | Op::SDPActive(_)
            | Op::LeaderClaim(_) => {}
        }
    }

    fn apply_owned_outputs(
        &mut self,
        utxos: impl IntoIterator<Item = Utxo>,
        observed_outputs: &mut Vec<WalletObservedOutput>,
    ) {
        for utxo in utxos {
            let Some(wallet_name) = self.wallets_by_pk.get(&utxo.note.pk) else {
                continue;
            };
            let Some(utxos_by_note) = self.utxos_by_wallet.get_mut(wallet_name) else {
                continue;
            };

            if utxos_by_note.insert(utxo.id(), utxo).is_none() {
                observed_outputs.push(WalletObservedOutput {
                    wallet_id: wallet_name.clone(),
                    utxo,
                });
            }
        }
    }

    fn apply_spent_note_ids(
        &mut self,
        note_ids: impl IntoIterator<Item = NoteId>,
        observed_spends: &mut Vec<WalletObservedSpend>,
    ) {
        for note_id in note_ids {
            self.locked_note_ids.remove(&note_id);

            for (wallet_name, utxos_by_note) in &mut self.utxos_by_wallet {
                if utxos_by_note.remove(&note_id).is_some() {
                    observed_spends.push(WalletObservedSpend {
                        wallet_id: wallet_name.clone(),
                        note_id,
                    });
                }
            }
        }
    }

    fn lock_note(&mut self, note_id: NoteId) {
        if self.contains_note(note_id) {
            self.locked_note_ids.insert(note_id);
        }
    }

    fn unlock_note(&mut self, note_id: NoteId) {
        self.locked_note_ids.remove(&note_id);
    }

    fn contains_note(&self, note_id: NoteId) -> bool {
        self.utxos_by_wallet
            .values()
            .any(|utxos_by_note| utxos_by_note.contains_key(&note_id))
    }

    fn available_utxos(&self, utxos_by_note: &HashMap<NoteId, Utxo>) -> Vec<Utxo> {
        utxos_by_note
            .iter()
            .filter_map(|(note_id, utxo)| {
                (!self.locked_note_ids.contains(note_id)).then_some(*utxo)
            })
            .collect()
    }
}

#[derive(Debug, Default, Clone)]
pub struct ObservedWalletChanges {
    pub observed_outputs: Vec<WalletObservedOutput>,
    pub observed_spends: Vec<WalletObservedSpend>,
}

#[derive(Debug, Clone)]
pub struct WalletObservedOutput {
    pub wallet_id: WalletId,
    pub utxo: Utxo,
}

#[derive(Debug, Clone)]
pub struct WalletObservedSpend {
    pub wallet_id: WalletId,
    pub note_id: NoteId,
}

#[derive(Debug, Clone, Eq, Error, PartialEq)]
pub enum TrackedWalletKeysError {
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
) -> Result<HashMap<ZkPublicKey, WalletId>, TrackedWalletKeysError> {
    let mut wallets_by_pk: HashMap<ZkPublicKey, WalletId> = HashMap::new();

    for (wallet_name, pks) in wallet_pks {
        for pk in pks {
            if let Some(existing_wallet) = wallets_by_pk.get(pk) {
                return Err(TrackedWalletKeysError::DuplicatePublicKey {
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
    use lb_core::{
        mantle::{
            MantleTx, Note,
            ledger::{Inputs, Outputs},
            ops::channel::{ChannelId, deposit::DepositOp, withdraw::ChannelWithdrawOp},
        },
        sdp::{DeclarationMessage, Locator, ProviderId, ServiceType, WithdrawMessage},
    };
    use lb_key_management_system_service::keys::Ed25519Key;

    use super::*;

    fn pk(value: u8) -> ZkPublicKey {
        ZkPublicKey::new(value.into())
    }

    fn utxo(value: u64, output_index: usize, pk: ZkPublicKey) -> Utxo {
        Utxo::new([output_index as u8; 32], output_index, Note::new(value, pk))
    }

    fn wallet_utxo_values(chain_state: &WalletChainState, wallet_id: &WalletId) -> Vec<u64> {
        let mut values = chain_state
            .wallet_utxos()
            .find(|(found_wallet_id, _)| *found_wallet_id == wallet_id)
            .map(|(_, utxos)| {
                utxos
                    .into_iter()
                    .map(|utxo| utxo.note.value)
                    .collect::<Vec<_>>()
            })
            .expect("wallet should be present");

        values.sort_unstable();

        values
    }

    fn sdp_declaration(locked_note_id: NoteId) -> DeclarationMessage {
        let provider_key = Ed25519Key::from_bytes(&[42; 32]).public_key();
        let locator: Locator = "/ip4/127.0.0.1/tcp/9100"
            .parse()
            .expect("locator should be valid");

        DeclarationMessage {
            service_type: ServiceType::BlendNetwork,
            locators: locator.into(),
            provider_id: ProviderId::from(provider_key),
            zk_id: pk(9),
            locked_note_id,
        }
    }

    #[test]
    fn genesis_seed_keeps_outputs_for_all_tracked_wallet_keys() {
        let wallet_id = WalletId::from("alice");
        let mut chain_state = WalletChainState::from_tracked_wallets(&[TrackedWalletKeys::new(
            wallet_id.clone(),
            [pk(1), pk(2)],
        )])
        .expect("tracked wallet keys should be valid");

        chain_state.seed_genesis_utxos(&[
            utxo(10, 0, pk(1)),
            utxo(20, 1, pk(2)),
            utxo(30, 2, pk(3)),
        ]);

        let wallet_utxos = chain_state.into_wallet_utxos();
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
    fn sdp_locking_removes_outputs_from_available_wallet_state() {
        let wallet_id = WalletId::from("alice");
        let locked = utxo(10, 0, pk(1));
        let declaration = sdp_declaration(locked.id());
        let mut chain_state = WalletChainState::from_tracked_wallets(&[TrackedWalletKeys::new(
            wallet_id.clone(),
            [pk(1)],
        )])
        .expect("tracked wallet keys should be valid");
        chain_state.seed_genesis_utxos(&[locked]);

        let declare_tx = SignedMantleTx::new_unverified(
            MantleTx([Op::SDPDeclare(declaration.clone())].into()),
            Vec::new(),
        );
        chain_state.apply_transaction(&declare_tx);

        assert!(wallet_utxo_values(&chain_state, &wallet_id).is_empty());

        let withdraw_tx = SignedMantleTx::new_unverified(
            MantleTx(
                [Op::SDPWithdraw(WithdrawMessage {
                    declaration_id: declaration.id(),
                    locked_note_id: locked.id(),
                    nonce: 0,
                })]
                .into(),
            ),
            Vec::new(),
        );
        chain_state.apply_transaction(&withdraw_tx);

        assert_eq!(wallet_utxo_values(&chain_state, &wallet_id), vec![10]);
    }

    #[test]
    fn channel_withdraw_outputs_add_tracked_wallet_outputs() {
        let wallet_id = WalletId::from("alice");
        let mut chain_state = WalletChainState::from_tracked_wallets(&[TrackedWalletKeys::new(
            wallet_id.clone(),
            [pk(1)],
        )])
        .expect("tracked wallet keys should be valid");
        let withdraw = ChannelWithdrawOp {
            channel_id: ChannelId::from([0; 32]),
            outputs: Outputs::new([Note::new(10, pk(1)), Note::new(20, pk(2))]),
            withdraw_nonce: 0,
        };

        let tx = SignedMantleTx::new_unverified(
            MantleTx([Op::ChannelWithdraw(withdraw)].into()),
            Vec::new(),
        );
        let update = chain_state.apply_transaction(&tx);

        assert_eq!(update.observed_outputs.len(), 1);
        assert_eq!(wallet_utxo_values(&chain_state, &wallet_id), vec![10]);
    }

    #[test]
    fn channel_deposit_inputs_remove_tracked_wallet_outputs() {
        let wallet_id = WalletId::from("alice");
        let deposited = utxo(10, 0, pk(1));
        let mut chain_state = WalletChainState::from_tracked_wallets(&[TrackedWalletKeys::new(
            wallet_id.clone(),
            [pk(1)],
        )])
        .expect("tracked wallet keys should be valid");
        chain_state.seed_genesis_utxos(&[deposited]);

        let tx = SignedMantleTx::new_unverified(
            MantleTx(
                [Op::ChannelDeposit(DepositOp {
                    channel_id: ChannelId::from([0; 32]),
                    inputs: Inputs::from([deposited.id()]),
                    metadata: b"deposit".into(),
                })]
                .into(),
            ),
            Vec::new(),
        );

        let update = chain_state.apply_transaction(&tx);
        let wallet_utxos = chain_state.into_wallet_utxos();

        assert_eq!(update.observed_spends.len(), 1);
        assert_eq!(update.observed_spends[0].note_id, deposited.id());
        assert!(
            wallet_utxos
                .get(wallet_id.as_str())
                .expect("wallet should be present")
                .is_empty()
        );
    }
}
