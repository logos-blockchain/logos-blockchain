use std::fmt::Debug;

use lb_core::{
    mantle::{Utxo, ops::leader_claim::VoucherCm},
    proofs::leader_proof::{Groth16LeaderProof, LeaderPrivate, LeaderPublic},
};
use lb_cryptarchia_engine::{Epoch, Slot};
use lb_key_management_system_service::{
    backend::preload::KeyId,
    keys::{Ed25519Key, UnsecuredZkKey},
};
use lb_ledger::{EpochState, UtxoTree};
use lb_wallet_service::UtxoWithKeyId;
use rand::rngs::OsRng;
use tokio::sync::watch::Sender;

use crate::{WinningPolInfo, kms::KmsAdapter};

#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: Address this at some point"
)]
/// Return a leadership proof and signing key if the current slot is a
/// winning one, and notifies consumers of winning slot info.
///
/// If the slot is not a winning one, it returns `None` and no consumer is
/// notified.
pub async fn build_proof_for<RuntimeServiceId>(
    utxos: &[UtxoWithKeyId],
    latest_tree: &UtxoTree,
    epoch_state: &EpochState,
    slot: Slot,
    winning_pol_info_notifier: &WinningPoLSlotNotifier<'_>,
    kms: &(impl KmsAdapter<RuntimeServiceId, KeyId = KeyId> + Sync),
) -> Option<(Groth16LeaderProof, Ed25519Key)> {
    for UtxoWithKeyId { utxo, key_id } in utxos {
        let secret_key = kms.get_leader_key(key_id.clone()).await;
        let public_inputs = public_inputs_for_slot(epoch_state, slot, latest_tree);
        let winning = check_winning(utxo, &public_inputs, &secret_key);
        if winning {
            tracing::debug!(
                "leader for slot {:?}, {:?}/{:?}",
                slot,
                utxo.note.value,
                epoch_state.total_stake()
            );

            let (private_inputs, leader_signing_key) =
                match private_inputs_for_winning_utxo_and_slot(
                    utxo,
                    &secret_key,
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

/// Check if the given note is owned by the leader and wins the lottery with
/// the given public inputs.
fn check_winning(utxo: &Utxo, public_inputs: &LeaderPublic, secret_key: &UnsecuredZkKey) -> bool {
    utxo.note.pk == secret_key.to_public_key()
        && public_inputs.check_winning(utxo.note.value, utxo.id().0, *secret_key.as_fr())
}

fn private_inputs_for_winning_utxo_and_slot(
    utxo: &Utxo,
    secret_key: &UnsecuredZkKey,
    epoch_state: &EpochState,
    public_inputs: LeaderPublic,
    latest_tree: &UtxoTree,
) -> Result<(LeaderPrivate, Ed25519Key), PrivateInputsError> {
    let aged_path = epoch_state
        .utxo_merkle_path(utxo)
        .ok_or(PrivateInputsError::AgedNoteNotFound)?;
    let latest_path = latest_tree
        .path(&utxo.id())
        .ok_or(PrivateInputsError::LatestNoteNotFound)?;
    let secret_key = *secret_key.as_fr();
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
    ledger_config: &'service lb_ledger::Config,
    sender: &'service Sender<Option<WinningPolInfo>>,
    /// Keeps track of the last processed epoch, if any, and for it the first
    /// winning slot that was pre-computed, if any.
    last_processed_epoch_and_found_first_winning_slot: Option<(Epoch, Option<Slot>)>,
}

impl<'service> WinningPoLSlotNotifier<'service> {
    pub(super) const fn new(
        ledger_config: &'service lb_ledger::Config,
        sender: &'service Sender<Option<WinningPolInfo>>,
    ) -> Self {
        Self {
            ledger_config,
            sender,
            last_processed_epoch_and_found_first_winning_slot: None,
        }
    }

    /// It processes a new unprocessed epoch, and sends over the channel the
    /// first identified winning slot for this epoch, if any.
    pub(super) async fn process_epoch<RuntimeServiceId>(
        &mut self,
        utxos: &[UtxoWithKeyId],
        epoch_state: &EpochState,
        kms: &(impl KmsAdapter<RuntimeServiceId, KeyId = KeyId> + Sync),
    ) {
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

        self.check_epoch_winning_utxos(utxos, epoch_state, kms)
            .await;
    }

    #[expect(clippy::cognitive_complexity, reason = "TODO: extract inner loop")]
    async fn check_epoch_winning_utxos<RuntimeServiceId>(
        &mut self,
        utxos: &[UtxoWithKeyId],
        epoch_state: &EpochState,
        kms: &(impl KmsAdapter<RuntimeServiceId, KeyId = KeyId> + Sync),
    ) {
        let slots_per_epoch = self.ledger_config.epoch_length();
        let epoch_starting_slot: u64 = self
            .ledger_config
            .epoch_config
            .starting_slot(&epoch_state.epoch, self.ledger_config.base_period_length())
            .into();
        // Not used to check if a slot wins the lottery.
        let latest_tree = UtxoTree::new();

        let mut first_winning_slot: Option<Slot> = None;
        for UtxoWithKeyId { utxo, key_id } in utxos {
            for offset in 0..slots_per_epoch {
                let slot = epoch_starting_slot
                    .checked_add(offset)
                    .expect("Slot calculation overflow.");

                let secret_key = kms.get_leader_key(key_id.clone()).await;

                let public_inputs = public_inputs_for_slot(epoch_state, slot.into(), &latest_tree);
                if !check_winning(utxo, &public_inputs, &secret_key) {
                    continue;
                }
                tracing::debug!("Found winning utxo with ID {:?} for slot {slot}", utxo.id());

                // Note: We discard the signing key here since this is just for pre-computing
                // winning slots. The actual signing key will be generated when building the
                // proof.
                let (leader_private, _signing_key) = match private_inputs_for_winning_utxo_and_slot(
                    utxo,
                    &secret_key,
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

#[cfg(test)]
mod pol_tests {
    use std::{num::NonZero, slice, sync::Arc};

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
        let kms = DummyKms;
        let key_id = KeyId::from("0");
        let sk = kms.get_leader_key(key_id.clone()).await;
        let pk = sk.to_public_key();
        let config = test_config();

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
        let notifier = WinningPoLSlotNotifier::new(&config, &sender);

        // Find a winning slot by calling `build_proof_for` until it succeeds
        let (proof, winning_slot) = find_winning_slot_and_build_proof(
            (0..1000).map(Slot::from),
            UtxoWithKeyId { utxo, key_id },
            &epoch_state,
            &latest_tree,
            &notifier,
            &kms,
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
        utxo: UtxoWithKeyId,
        epoch_state: &EpochState,
        latest_tree: &UtxoTree,
        notifier: &WinningPoLSlotNotifier<'_>,
        kms: &(impl KmsAdapter<(), KeyId = KeyId> + Sync),
    ) -> Option<(Groth16LeaderProof, Slot)> {
        for slot in slots {
            if let Some((proof, _signing_key)) = build_proof_for(
                slice::from_ref(&utxo),
                latest_tree,
                epoch_state,
                slot,
                notifier,
                kms,
            )
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

    struct DummyKms;

    #[async_trait::async_trait]
    impl KmsAdapter<()> for DummyKms {
        type KeyId = KeyId;

        async fn get_leader_key(&self, _: Self::KeyId) -> UnsecuredZkKey {
            UnsecuredZkKey::new(Fr::from(0u64))
        }
    }
}
