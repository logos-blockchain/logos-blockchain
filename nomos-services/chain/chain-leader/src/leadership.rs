use cryptarchia_engine::{Epoch, Slot};
use key_management_system_keys::keys::{Ed25519Key, UnsecuredZkKey, ZkPublicKey};
use nomos_core::{
    mantle::{Utxo, ops::leader_claim::VoucherCm},
    proofs::leader_proof::{Groth16LeaderProof, LeaderPrivate, LeaderPublic},
};
use nomos_ledger::{EpochState, UtxoTree};
use serde::{Deserialize, Serialize};
use tokio::sync::watch::Sender;

use crate::WinningPolInfo;

#[derive(Clone)]
pub struct Leader {
    sk: UnsecuredZkKey,
    config: nomos_ledger::Config,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LeaderConfig {
    pub pk: ZkPublicKey,
    pub sk: UnsecuredZkKey,
}

impl Leader {
    pub const fn new(sk: UnsecuredZkKey, config: nomos_ledger::Config) -> Self {
        Self { sk, config }
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: Address this at some point"
    )]
    /// Return a leadership proof if the current slot is a winning one, and
    /// notifies consumers of winning slot info.
    ///
    /// If the slot is not a winning one, it returns `None` and no consumer is
    /// notified.
    pub async fn build_proof_for(
        &self,
        utxos: &[Utxo],
        latest_tree: &UtxoTree,
        epoch_state: &EpochState,
        slot: Slot,
        winning_pol_info_notifier: &WinningPoLSlotNotifier<'_>,
    ) -> Option<Groth16LeaderProof> {
        for utxo in utxos {
            let public_inputs = public_inputs_for_slot(epoch_state, slot, latest_tree);

            let note_id = utxo.id().0;
            let secret_key = self.secret_key();

            #[cfg(feature = "pol-dev-mode")]
            let winning = public_inputs.check_winning_dev(
                utxo.note.value,
                note_id,
                *secret_key.as_fr(),
                self.config.consensus_config.active_slot_coeff,
            );
            #[cfg(not(feature = "pol-dev-mode"))]
            let winning =
                public_inputs.check_winning(utxo.note.value, note_id, *secret_key.as_fr());

            if winning {
                tracing::debug!(
                    "leader for slot {:?}, {:?}/{:?}",
                    slot,
                    utxo.note.value,
                    epoch_state.total_stake()
                );

                let private_inputs = self.private_inputs_for_winning_utxo_and_slot(
                    utxo,
                    epoch_state,
                    public_inputs,
                    latest_tree,
                );

                winning_pol_info_notifier.notify_about_winning_slot(
                    private_inputs.clone(),
                    epoch_state.epoch,
                    slot,
                );

                let res = tokio::task::spawn_blocking(move || {
                    Groth16LeaderProof::prove(
                        private_inputs,
                        VoucherCm::default(), // TODO: use actual voucher commitment
                    )
                })
                .await;
                match res {
                    Ok(Ok(proof)) => return Some(proof),
                    Ok(Err(e)) => {
                        tracing::error!("Failed to build proof: {:?}", e);
                    }
                    Err(e) => {
                        tracing::error!("Failed to wait thread to build proof: {:?}", e);
                    }
                }
            } else {
                tracing::trace!(
                    "Not a leader for slot {:?}, {:?}/{:?}",
                    slot,
                    utxo.note.value,
                    epoch_state.total_stake()
                );
            }
        }

        None
    }

    fn private_inputs_for_winning_utxo_and_slot(
        &self,
        utxo: &Utxo,
        // TODO: Use aged tree to compute `aged_path`
        _epoch_state: &EpochState,
        public_inputs: LeaderPublic,
        // TODO: Use latest tree to compute `latest_path`
        _latest_tree: &UtxoTree,
    ) -> LeaderPrivate {
        // TODO: Get the actual witness paths and leader key
        let aged_path = Vec::new(); // Placeholder for aged path, aged UTXO tree is included in `EpochState`.
        let latest_path = Vec::new();
        let secret_key = *self.sk.as_fr();
        let leader_signing_key = Ed25519Key::from_bytes(&[0; 32]);
        let leader_pk = leader_signing_key.public_key(); // TODO: get actual leader public key

        LeaderPrivate::new(
            public_inputs,
            *utxo,
            &aged_path,
            &latest_path,
            secret_key,
            &leader_pk,
        )
    }

    fn secret_key(&self) -> UnsecuredZkKey {
        self.sk.clone()
    }
}

fn public_inputs_for_slot(
    epoch_state: &EpochState,
    slot: Slot,
    latest_tree: &UtxoTree,
) -> LeaderPublic {
    LeaderPublic::new(
        epoch_state.utxos.root(),
        latest_tree.root(),
        epoch_state.nonce,
        slot.into(),
        epoch_state.total_stake(),
    )
}

/// Process every tick and reacts to the very first one received and the first
/// one of every new epoch.
///
/// Reacting to a tick means pre-calculating the winning slots for the epoch and
/// notifying all consumers via the provided sender channel.
pub struct WinningPoLSlotNotifier<'service> {
    leader: &'service Leader,
    sender: &'service Sender<Option<WinningPolInfo>>,
    /// Keeps track of the last processed epoch, if any, and for it the first
    /// winning slot that was pre-computed, if any.
    last_processed_epoch_and_found_first_winning_slot: Option<(Epoch, Option<Slot>)>,
}

impl<'service> WinningPoLSlotNotifier<'service> {
    pub(super) const fn new(
        leader: &'service Leader,
        sender: &'service Sender<Option<WinningPolInfo>>,
    ) -> Self {
        Self {
            leader,
            sender,
            last_processed_epoch_and_found_first_winning_slot: None,
        }
    }

    /// It processes a new unprocessed epoch, and sends over the channel the
    /// first identified winning slot for this epoch, if any.
    pub(super) fn process_epoch(&mut self, utxos: &[Utxo], epoch_state: &EpochState) {
        if let Some((last_processed_epoch, _)) =
            self.last_processed_epoch_and_found_first_winning_slot
        {
            if last_processed_epoch == epoch_state.epoch {
                tracing::trace!("Skipping already processed epoch.");
                return;
            } else if last_processed_epoch > epoch_state.epoch {
                tracing::error!(
                    "Received an epoch smaller than the last process one. This is invalid."
                );
                return;
            }
        }
        tracing::debug!("Processing new epoch: {:?}", epoch_state.epoch);

        self.check_epoch_winning_utxos(utxos, epoch_state);
    }

    fn check_epoch_winning_utxos(&mut self, utxos: &[Utxo], epoch_state: &EpochState) {
        let slots_per_epoch = self.leader.config.epoch_length();
        let epoch_starting_slot: u64 = self
            .leader
            .config
            .epoch_config
            .starting_slot(&epoch_state.epoch, self.leader.config.base_period_length())
            .into();
        // Not used to check if a slot wins the lottery.
        let latest_tree = UtxoTree::new();

        let mut first_winning_slot: Option<Slot> = None;
        for utxo in utxos {
            let note_id = utxo.id().0;

            for offset in 0..slots_per_epoch {
                let slot = epoch_starting_slot
                    .checked_add(offset)
                    .expect("Slot calculation overflow.");
                let secret_key = self.leader.secret_key();

                let public_inputs = public_inputs_for_slot(epoch_state, slot.into(), &latest_tree);
                if !public_inputs.check_winning(utxo.note.value, note_id, *secret_key.as_fr()) {
                    continue;
                }
                tracing::debug!("Found winning utxo with ID {:?} for slot {slot}", utxo.id());

                let leader_private = self.leader.private_inputs_for_winning_utxo_and_slot(
                    utxo,
                    epoch_state,
                    public_inputs,
                    &latest_tree,
                );

                if let Err(err) = self.sender.send(Some((leader_private, epoch_state.epoch))) {
                    tracing::error!(
                        "Failed to send pre-calculated PoL winning slots to receivers. Error: {err:?}"
                    );
                } else {
                    // We stop the iteration as soon as the first winning slot for this epoch is
                    // found and was successfully communicated to consumers.
                    first_winning_slot = Some(slot.into());
                    break;
                }
            }
        }
        self.last_processed_epoch_and_found_first_winning_slot =
            Some((epoch_state.epoch, first_winning_slot));
    }

    /// Send the information about a winning slot to consumers.
    ///
    /// No check is performed on whether the slot is actually a winning one.
    pub(super) fn notify_about_winning_slot(
        &self,
        private_inputs: LeaderPrivate,
        epoch: Epoch,
        slot: Slot,
    ) {
        // If we are trying to notify about the first winning slot that we already
        // pre-computed, ignore it.
        if let Some((_, Some(first_epoch_winning_slot))) =
            self.last_processed_epoch_and_found_first_winning_slot
            && first_epoch_winning_slot == slot
        {
            tracing::warn!(
                "Skipping notifying about winning slot {slot:?} because it was already processed"
            );
            return;
        }

        if let Err(err) = self.sender.send(Some((private_inputs, epoch))) {
            tracing::error!(
                "Failed to send pre-calculated PoL winning slots to receivers. Error: {err:?}"
            );
        }
    }
}
