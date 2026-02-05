use std::fmt::{Debug, Display};

use lb_core::{
    mantle::Utxo,
    proofs::leader_proof::{Groth16LeaderProof, LeaderPrivate, LeaderPublic},
};
use lb_cryptarchia_engine::{Epoch, Slot};
use lb_key_management_system_service::{
    backend::preload::KeyId, keys::Ed25519Key,
    operators::zk::leader::BuildPrivateInputsWithLeaderKey,
};
use lb_ledger::{EpochState, UtxoTree};
use lb_wallet_service::{UtxoWithKeyId, api::WalletApi};
use overwatch::services::AsServiceId;
use rand::rngs::OsRng;
use tokio::sync::{oneshot, watch::Sender};

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
pub async fn build_proof_for<Wallet, RuntimeServiceId>(
    utxos: &[UtxoWithKeyId],
    latest_tree: &UtxoTree,
    epoch_state: &EpochState,
    slot: Slot,
    winning_pol_info_notifier: &WinningPoLSlotNotifier<'_>,
    wallet: &WalletApi<Wallet, RuntimeServiceId>,
    kms: &(impl KmsAdapter<RuntimeServiceId, KeyId = KeyId> + Sync),
) -> Option<(Groth16LeaderProof, Ed25519Key)>
where
    Wallet: lb_wallet_service::api::WalletServiceData,
    RuntimeServiceId: Debug + Display + Sync + AsServiceId<Wallet>,
{
    for UtxoWithKeyId { utxo, key_id } in utxos {
        let public_inputs = public_inputs_for_slot(epoch_state, slot, latest_tree);
        let winning = kms
            .check_winning_with_key(key_id.clone(), utxo, &public_inputs)
            .await;
        if winning {
            tracing::debug!(
                "leader for slot {:?}, {:?}/{:?}",
                slot,
                utxo.note.value,
                epoch_state.total_stake()
            );

            let voucher_cm = match wallet.generate_new_voucher().await {
                Ok(voucher_cm) => voucher_cm,
                Err(e) => {
                    tracing::error!("Failed to generate voucher: {e:?}");
                    continue;
                }
            };

            let private_inputs_result = kms
                .build_private_inputs_for_winning_utxo_and_slot(
                    key_id.clone(),
                    utxo,
                    epoch_state,
                    public_inputs,
                    latest_tree,
                )
                .await;
            let (private_inputs, leader_signing_key) = match private_inputs_result {
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
                Groth16LeaderProof::prove(private_inputs, voucher_cm)
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

pub fn operator_for_private_inputs_arguments_for_winning_utxo_and_slot(
    utxo: &Utxo,
    epoch_state: &EpochState,
    public_inputs: LeaderPublic,
    latest_tree: &UtxoTree,
) -> Result<
    (
        BuildPrivateInputsWithLeaderKey,
        oneshot::Receiver<LeaderPrivate>,
        Ed25519Key,
    ),
    PrivateInputsError,
> {
    let (sender, receiver) = oneshot::channel();
    let aged_path = epoch_state
        .utxo_merkle_path(utxo)
        .ok_or(PrivateInputsError::AgedNoteNotFound)?;
    let latest_path = latest_tree
        .path(&utxo.id())
        .ok_or(PrivateInputsError::LatestNoteNotFound)?;
    // Generate a random one-time Ed25519 key for P_LEAD (as per PoL spec)
    let leader_signing_key = Ed25519Key::generate(&mut OsRng);
    let leader_pk = leader_signing_key.public_key();

    Ok((
        BuildPrivateInputsWithLeaderKey::new(
            sender,
            *utxo,
            public_inputs,
            aged_path,
            latest_path,
            leader_pk,
        ),
        receiver,
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
pub enum PrivateInputsError {
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
                let public_inputs = public_inputs_for_slot(epoch_state, slot.into(), &latest_tree);
                let winning = kms
                    .check_winning_with_key(key_id.clone(), utxo, &public_inputs)
                    .await;
                if !winning {
                    continue;
                }
                tracing::debug!("Found winning utxo with ID {:?} for slot {slot}", utxo.id());

                // Note: We discard the signing key here since this is just for pre-computing
                // winning slots. The actual signing key will be generated when building the
                // proof.
                let private_inputs_result = kms
                    .build_private_inputs_for_winning_utxo_and_slot(
                        key_id.clone(),
                        utxo,
                        epoch_state,
                        public_inputs,
                        &latest_tree,
                    )
                    .await;
                let (leader_private, _signing_key) = match private_inputs_result {
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
    use core::fmt;
    use std::{fmt::Formatter, num::NonZero, slice, sync::Arc};

    use lb_core::{
        mantle::{
            ledger::{Note, Tx},
            ops::leader_claim::VoucherCm,
        },
        proofs::leader_proof::{LeaderProof as _, check_winning},
        sdp::{MinStake, ServiceParameters, ServiceType},
    };
    use lb_cryptarchia_engine::EpochConfig;
    use lb_groth16::{Fr, fr_from_bytes_unchecked};
    use lb_key_management_system_service::keys::{UnsecuredZkKey, ZkKey};
    use lb_ledger::mantle::sdp::{
        Config as SdpConfig, ServiceRewardsParameters, rewards::blend::RewardsParameters,
    };
    use lb_utils::math::NonNegativeF64;
    use lb_wallet_service::{WalletMsg, WalletServiceSettings, api::WalletServiceData};
    use overwatch::services::{
        ServiceData,
        relay::OutboundRelay,
        state::{NoOperator, NoState},
    };
    use tokio::sync::{mpsc, watch};

    use super::*;

    /// Test that [`Leader::build_proof_for`] generates `PoL` which can be
    /// verified successfully.
    #[tokio::test]
    async fn test_build_proof_for() {
        // Create secret key and leader
        let kms = DummyKms;
        let key_id = KeyId::from("0");
        let sk = UnsecuredZkKey::new(Fr::from(0u64));
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

        // Create dummy wallet service
        let wallet = DummyWallet::spawn();

        // Find a winning slot by calling `build_proof_for` until it succeeds
        let (proof, winning_slot) = find_winning_slot_and_build_proof(
            (0..1000).map(Slot::from),
            UtxoWithKeyId { utxo, key_id },
            &epoch_state,
            &latest_tree,
            &notifier,
            &wallet,
            &kms,
        )
        .await
        .expect("should find a winning slot and build a proof");
        assert_eq!(proof.voucher_cm(), &dummy_voucher_cm());

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
        wallet: &WalletApi<DummyWallet, TestRuntimeServiceId>,
        kms: &(impl KmsAdapter<TestRuntimeServiceId, KeyId = KeyId> + Sync),
    ) -> Option<(Groth16LeaderProof, Slot)> {
        for slot in slots {
            if let Some((proof, _signing_key)) = build_proof_for(
                slice::from_ref(&utxo),
                latest_tree,
                epoch_state,
                slot,
                notifier,
                wallet,
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
                        activity_threshold_sensitivity: 1,
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
    impl KmsAdapter<TestRuntimeServiceId> for DummyKms {
        type KeyId = KeyId;

        async fn check_winning_with_key(
            &self,
            _: Self::KeyId,
            utxo: &Utxo,
            leader_public: &LeaderPublic,
        ) -> bool {
            let sk = ZkKey::new(Fr::from(0u64));
            check_winning(*utxo, *leader_public, &sk.to_public_key(), Fr::from(0u64))
        }

        async fn build_private_inputs_for_winning_utxo_and_slot(
            &self,
            _: Self::KeyId,
            utxo: &Utxo,
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
            // Generate a random one-time Ed25519 key for P_LEAD (as per PoL spec)
            let leader_signing_key = Ed25519Key::generate(&mut OsRng);
            let leader_pk = leader_signing_key.public_key();
            let leader_private = LeaderPrivate::new(
                public_inputs,
                *utxo,
                &aged_path,
                &latest_path,
                Fr::from(0u64),
                &leader_pk,
            );
            Ok((leader_private, leader_signing_key))
        }
    }

    struct DummyWallet;

    impl ServiceData for DummyWallet {
        type Settings = WalletServiceSettings;
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = WalletMsg;
    }

    impl WalletServiceData for DummyWallet {
        type Kms = ();
        type Cryptarchia = ();
        type Tx = ();
        type Storage = ();
    }

    impl DummyWallet {
        fn spawn() -> WalletApi<Self, TestRuntimeServiceId> {
            let (msg_sender, mut msg_receiver) = mpsc::channel(10);

            tokio::spawn(async move {
                while let Some(msg) = msg_receiver.recv().await {
                    if let WalletMsg::GenerateNewVoucherSecret { resp_tx } = msg {
                        let _ = resp_tx.send(dummy_voucher_cm());
                    }
                }
            });

            WalletApi::<Self, TestRuntimeServiceId>::new(OutboundRelay::new(msg_sender))
        }
    }

    const DUMMY_VOUCHER_CM_BYTES: [u8; 32] = [99u8; 32];

    fn dummy_voucher_cm() -> VoucherCm {
        fr_from_bytes_unchecked(&DUMMY_VOUCHER_CM_BYTES).into()
    }

    #[derive(Debug)]
    struct TestRuntimeServiceId;

    impl AsServiceId<DummyWallet> for TestRuntimeServiceId {
        const SERVICE_ID: Self = Self;
    }

    impl Display for TestRuntimeServiceId {
        fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
            write!(f, "TestRuntimeServiceId")
        }
    }
}
