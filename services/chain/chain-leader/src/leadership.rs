use lb_core::{
    if_pol_dev_mode,
    mantle::{Utxo, ops::leader_claim::VoucherCm},
    proofs::leader_proof::{Groth16LeaderProof, LeaderPrivate, LeaderPublic},
};
use lb_cryptarchia_engine::{Epoch, Slot};
use lb_key_management_system_keys::keys::{Ed25519Key, UnsecuredZkKey, ZkPublicKey};
use lb_ledger::{EpochState, UtxoTree};
#[cfg(feature = "pol-dev-mode")]
use lb_pol::slot_activation_coefficient;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use tokio::sync::watch::Sender;

use crate::WinningPolInfo;

#[derive(Clone)]
pub struct Leader {
    sk: UnsecuredZkKey,
    config: lb_ledger::Config,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LeaderConfig {
    pub pk: ZkPublicKey,
    pub sk: UnsecuredZkKey,
}

impl Leader {
    pub const fn new(sk: UnsecuredZkKey, config: lb_ledger::Config) -> Self {
        Self { sk, config }
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: Address this at some point"
    )]
    /// Return a leadership proof and signing key if the current slot is a
    /// winning one, and notifies consumers of winning slot info.
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
    ) -> Option<(Groth16LeaderProof, Ed25519Key)> {
        for utxo in utxos {
            let public_inputs = public_inputs_for_slot(epoch_state, slot, latest_tree);

            let note_id = utxo.id().0;
            let secret_key = self.secret_key();

            let winning = if_pol_dev_mode!(
                public_inputs.check_winning_dev(
                    utxo.note.value,
                    note_id,
                    *secret_key.as_fr(),
                    slot_activation_coefficient(),
                ),
                public_inputs.check_winning(utxo.note.value, note_id, *secret_key.as_fr())
            );

            if winning {
                tracing::debug!(
                    "leader for slot {:?}, {:?}/{:?}",
                    slot,
                    utxo.note.value,
                    epoch_state.total_stake()
                );

                let (private_inputs, leader_signing_key) = match self
                    .private_inputs_for_winning_utxo_and_slot(
                        utxo,
                        epoch_state,
                        public_inputs,
                        latest_tree,
                    ) {
                    Ok(result) => result,
                    Err(e) => {
                        tracing::error!(
                            "Failed to build private inputs for winning utxo {:?} for {slot:?}: {e:?}",
                            utxo.id(),
                        );
                        continue;
                    }
                };

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
                    Ok(Ok(proof)) => return Some((proof, leader_signing_key)),
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
        epoch_state: &EpochState,
        public_inputs: LeaderPublic,
        latest_tree: &UtxoTree,
    ) -> Result<(LeaderPrivate, Ed25519Key), PrivateInputsError> {
        let aged_path = if_pol_dev_mode!(Vec::new(), {
            epoch_state
                .utxo_merkle_path(utxo)
                .ok_or(PrivateInputsError::AgedNoteNotFound)?
        });
        let latest_path = if_pol_dev_mode!(Vec::new(), {
            latest_tree
                .path(&utxo.id())
                .ok_or(PrivateInputsError::LatestNoteNotFound)?
        });
        let secret_key = *self.sk.as_fr();
        // Generate a random one-time Ed25519 key for P_LEAD (as per PoL spec)
        let leader_signing_key = Ed25519Key::generate(&mut OsRng);
        let leader_pk = leader_signing_key.public_key();

        Ok((
            LeaderPrivate::new(
                public_inputs,
                *utxo,
                &aged_path,
                &latest_path,
                secret_key,
                &leader_pk,
            ),
            leader_signing_key,
        ))
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
        epoch_state.utxo_merkle_root(),
        latest_tree.root(),
        epoch_state.nonce,
        slot.into(),
        epoch_state.total_stake(),
    )
}

#[derive(thiserror::Error, Debug)]
enum PrivateInputsError {
    #[error("Aged note not found from merkle tree")]
    AgedNoteNotFound,
    #[error("Latest note not found from merkle tree")]
    LatestNoteNotFound,
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

    #[expect(clippy::cognitive_complexity, reason = "TODO: extract inner loop")]
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

                // Note: We discard the signing key here since this is just for pre-computing
                // winning slots. The actual signing key will be generated when building the
                // proof.
                let (leader_private, _signing_key) = match self
                    .leader
                    .private_inputs_for_winning_utxo_and_slot(
                        utxo,
                        epoch_state,
                        public_inputs,
                        &latest_tree,
                    ) {
                    Ok(result) => result,
                    Err(e) => {
                        tracing::error!(
                            "Failed to build private inputs for winning utxo {:?} for {slot:?}: {e:?}",
                            utxo.id(),
                        );
                        continue;
                    }
                };

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

#[cfg(not(feature = "pol-dev-mode"))]
#[cfg(test)]
mod pol_tests {
    use std::{num::NonZero, sync::Arc};

    use lb_core::{
        mantle::ledger::{Note, Tx},
        proofs::leader_proof::LeaderProof as _,
        sdp::{MinStake, ServiceParameters, ServiceType},
    };
    use lb_cryptarchia_engine::EpochConfig;
    use lb_groth16::Fr;
    use lb_ledger::mantle::sdp::{
        Config as SdpConfig, ServiceRewardsParameters, rewards::blend::RewardsParameters,
    };
    use lb_utils::math::NonNegativeF64;
    use tokio::sync::watch;

    use super::*;

    /// Test that [`Leader::build_proof_for`] generates `PoL` which can be
    /// verified successfully.
    #[tokio::test]
    async fn test_build_proof_for() {
        // Create secret key and leader
        let sk = UnsecuredZkKey::new(Fr::from(12345u64));
        let pk = sk.to_public_key();
        let leader = Leader::new(sk, test_config());

        // Create a UTXO
        let utxo = Tx::new(vec![], vec![Note::new(1000u64, pk)])
            .utxo_by_index(0)
            .unwrap();

        // Create aged/latest UTXO trees
        let aged_tree = UtxoTree::new().insert(utxo.id(), utxo).0;
        let latest_tree = UtxoTree::new().insert(utxo.id(), utxo).0;

        // Create EpochState
        let epoch_state = EpochState {
            epoch: 1.into(),
            nonce: Fr::from(999u64),
            utxos: aged_tree.clone(),
            total_stake: utxo.note.value,
        };

        // Create notifier channel (not used in this test)
        let (sender, _receiver) = watch::channel(None);
        let notifier = WinningPoLSlotNotifier::new(&leader, &sender);

        // Find a winning slot by calling `build_proof_for` until it succeeds
        let (proof, winning_slot) = find_winning_slot_and_build_proof(
            (0..1000).map(Slot::from),
            &leader,
            utxo,
            &epoch_state,
            &latest_tree,
            &notifier,
        )
        .await
        .expect("should find a winning slot and build a proof");

        // Verify proof
        let public_inputs = LeaderPublic::new(
            aged_tree.root(),
            latest_tree.root(),
            epoch_state.nonce,
            winning_slot.into(),
            utxo.note.value,
        );
        assert!(
            proof.verify(&public_inputs),
            "proof verification should succeed"
        );
    }

    /// Find a winning slot by calling `build_proof_for` until it succeeds
    async fn find_winning_slot_and_build_proof(
        slots: impl Iterator<Item = Slot>,
        leader: &Leader,
        utxo: Utxo,
        epoch_state: &EpochState,
        latest_tree: &UtxoTree,
        notifier: &WinningPoLSlotNotifier<'_>,
    ) -> Option<(Groth16LeaderProof, Slot)> {
        for slot in slots {
            if let Some((proof, _signing_key)) = leader
                .build_proof_for(&[utxo], latest_tree, epoch_state, slot, notifier)
                .await
            {
                return Some((proof, slot));
            }
        }
        None
    }

    fn test_config() -> lb_ledger::Config {
        lb_ledger::Config {
            epoch_config: EpochConfig {
                epoch_stake_distribution_stabilization: NonZero::new(3u8).unwrap(),
                epoch_period_nonce_buffer: NonZero::new(3).unwrap(),
                epoch_period_nonce_stabilization: NonZero::new(4).unwrap(),
            },
            consensus_config: lb_cryptarchia_engine::Config::new(NonZero::new(5).unwrap(), 0.05),
            sdp_config: SdpConfig {
                service_params: Arc::new(
                    [(
                        ServiceType::BlendNetwork,
                        ServiceParameters {
                            lock_period: 10,
                            inactivity_period: 20,
                            retention_period: 100,
                            timestamp: 0,
                            session_duration: 10,
                        },
                    )]
                    .into(),
                ),
                service_rewards_params: ServiceRewardsParameters {
                    blend: RewardsParameters {
                        rounds_per_session: NonZero::new(10u64).unwrap(),
                        message_frequency_per_round: NonNegativeF64::try_from(1.0).unwrap(),
                        num_blend_layers: NonZero::new(3u64).unwrap(),
                        minimum_network_size: NonZero::new(1u64).unwrap(),
                        data_replication_factor: 0,
                    },
                },
                min_stake: MinStake {
                    threshold: 1,
                    timestamp: 0,
                },
            },
        }
    }
}
