pub mod channel;
pub mod leader;
pub mod sdp;

use std::collections::HashMap;

use nomos_core::{
    block::BlockNumber,
    mantle::{
        AuthenticatedMantleTx, GasConstants, GenesisTx, NoteId, TxHash, Utxo,
        ops::{Op, OpProof, leader_claim::VoucherCm},
    },
    sdp::{Declaration, DeclarationId, ProviderId, ProviderInfo, ServiceType, SessionNumber},
};
use sdp::{Error as SdpLedgerError, locked_notes::LockedNotes};

use crate::{Balance, Config, EpochState, UtxoTree};

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error(transparent)]
    Channel(#[from] channel::Error),
    #[error(transparent)]
    Leader(#[from] leader::Error),
    #[error("Unsupported operation")]
    UnsupportedOp,
    #[error("Sdp ledger error: {0:?}")]
    Sdp(#[from] SdpLedgerError),
    #[error("Note not found: {0:?}")]
    NoteNotFound(NoteId),
}

/// Tracks mantle ops
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, PartialEq, Debug)]
pub struct LedgerState {
    channels: channel::Channels,
    sdp: sdp::SdpLedger,
    leaders: leader::LeaderState,
}

impl LedgerState {
    #[must_use]
    pub fn new(config: &Config, epoch_state: &EpochState) -> Self {
        Self {
            channels: channel::Channels::new(),
            sdp: sdp::SdpLedger::new()
                .with_blend_service(
                    config.sdp_config.service_rewards_params.blend.clone(),
                    epoch_state,
                )
                .with_da_service(),
            leaders: leader::LeaderState::new(),
        }
    }

    pub fn from_genesis_tx(
        tx: impl GenesisTx,
        config: &Config,
        utxo_tree: &UtxoTree,
        epoch_state: &EpochState,
    ) -> Result<Self, Error> {
        let channels = channel::Channels::from_genesis(tx.genesis_inscription())?;
        let sdp = sdp::SdpLedger::from_genesis(
            &config.sdp_config,
            utxo_tree,
            epoch_state,
            tx.hash(),
            tx.sdp_declarations(),
        )?;

        Ok(Self {
            channels,
            sdp,
            leaders: leader::LeaderState::new(),
        })
    }

    pub fn try_apply_tx<Constants: GasConstants>(
        self,
        current_block_number: BlockNumber,
        config: &Config,
        utxo_tree: &UtxoTree,
        tx: impl AuthenticatedMantleTx,
    ) -> Result<(Self, Balance), Error> {
        let tx_hash = tx.hash();
        let ops = tx.ops_with_proof().map(|(op, proof)| (op, Some(proof)));
        self.try_apply_ops(current_block_number, config, utxo_tree, tx_hash, ops)
    }

    #[must_use]
    pub const fn locked_notes(&self) -> &LockedNotes {
        self.sdp.locked_notes()
    }

    #[must_use]
    pub const fn sdp_ledger(&self) -> &sdp::SdpLedger {
        &self.sdp
    }

    #[must_use]
    pub const fn channels(&self) -> &channel::Channels {
        &self.channels
    }

    #[must_use]
    pub fn active_session_providers(
        &self,
        service_type: ServiceType,
    ) -> Option<HashMap<ProviderId, ProviderInfo>> {
        self.sdp.active_session_providers(service_type)
    }

    #[must_use]
    pub fn active_sessions(&self) -> HashMap<ServiceType, SessionNumber> {
        self.sdp.active_sessions()
    }

    #[must_use]
    pub fn sdp_declarations(&self) -> Vec<(DeclarationId, Declaration)> {
        self.sdp.declarations()
    }

    pub fn try_apply_header(
        mut self,
        epoch_state: &EpochState,
        voucher: VoucherCm,
        config: &Config,
    ) -> Result<(Self, Vec<Utxo>), Error> {
        self.leaders = self.leaders.try_apply_header(epoch_state.epoch, voucher)?;
        let (new_sdp, reward_utxos) = self.sdp.try_apply_header(&config.sdp_config, epoch_state)?;
        self.sdp = new_sdp;
        Ok((self, reward_utxos))
    }

    fn try_apply_ops<'a>(
        mut self,
        _current_block_number: BlockNumber,
        config: &Config,
        utxo_tree: &UtxoTree,
        tx_hash: TxHash,
        ops: impl Iterator<Item = (&'a Op, Option<&'a OpProof>)> + 'a,
    ) -> Result<(Self, Balance), Error> {
        let mut balance = 0;
        for (op, proof) in ops {
            match (op, proof) {
                // The signature for channel ops can be verified before reaching this point,
                // as you only need the signer's public key and tx hash
                // Callers are expected to validate the proof before calling this function.
                (Op::ChannelBlob(op), _) => {
                    self.channels =
                        self.channels
                            .apply_msg(op.channel, &op.parent, op.id(), &op.signer)?;
                }
                (Op::ChannelInscribe(op), _) => {
                    self.channels =
                        self.channels
                            .apply_msg(op.channel_id, &op.parent, op.id(), &op.signer)?;
                }
                (Op::ChannelSetKeys(op), Some(OpProof::Ed25519Sig(sig))) => {
                    self.channels = self.channels.set_keys(op.channel, op, sig, &tx_hash)?;
                }
                (
                    Op::SDPDeclare(op),
                    Some(OpProof::ZkAndEd25519Sigs {
                        zk_sig,
                        ed25519_sig,
                    }),
                ) => {
                    let Some((utxo, _)) = utxo_tree.utxos().get(&op.locked_note_id) else {
                        return Err(Error::NoteNotFound(op.locked_note_id));
                    };
                    self.sdp = self.sdp.apply_declare_msg(
                        op,
                        utxo.note,
                        zk_sig,
                        ed25519_sig,
                        tx_hash,
                        &config.sdp_config,
                    )?;
                }
                (Op::SDPActive(op), Some(OpProof::ZkSig(sig))) => {
                    self.sdp = self
                        .sdp
                        .apply_active_msg(op, sig, tx_hash, &config.sdp_config)?;
                }
                (Op::SDPWithdraw(op), Some(OpProof::ZkSig(sig))) => {
                    self.sdp =
                        self.sdp
                            .apply_withdrawn_msg(op, sig, tx_hash, &config.sdp_config)?;
                }
                (Op::LeaderClaim(op), None) => {
                    // Correct derivation of the voucher nullifier and membership in the merkle tree
                    // can be verified outside of this function since public inputs are already
                    // available. Callers are expected to validate the proof
                    // before calling this function.
                    let leader_balance;
                    (self.leaders, leader_balance) = self.leaders.claim(op)?;
                    balance += leader_balance;
                }
                _ => {
                    return Err(Error::UnsupportedOp);
                }
            }
        }

        Ok((self, balance))
    }
}

#[cfg(test)]
mod tests {
    use key_management_system_keys::keys::{Ed25519Key, Ed25519PublicKey, ZkKey};
    use nomos_core::mantle::{
        MantleTx, SignedMantleTx, Transaction as _,
        gas::MainnetGasConstants,
        ledger::Tx as LedgerTx,
        ops::channel::{
            ChannelId, MsgId, blob::BlobOp, inscribe::InscriptionOp, set_keys::SetKeysOp,
        },
    };

    use super::*;
    use crate::cryptarchia::tests::{config, genesis_state, utxo};

    fn create_test_keys() -> (Ed25519Key, Ed25519PublicKey) {
        create_test_keys_with_seed(0)
    }

    fn create_test_keys_with_seed(seed: u8) -> (Ed25519Key, Ed25519PublicKey) {
        let signing_key = Ed25519Key::from_bytes(&[seed; 32]);
        let verifying_key = signing_key.public_key();
        (signing_key, verifying_key)
    }

    fn create_signed_tx(op: Op, signing_key: &Ed25519Key) -> SignedMantleTx {
        create_multi_signed_tx(vec![op], vec![signing_key])
    }

    fn create_multi_signed_tx(ops: Vec<Op>, signing_keys: Vec<&Ed25519Key>) -> SignedMantleTx {
        let ledger_tx = LedgerTx::new(vec![], vec![]);
        let mantle_tx = MantleTx {
            ops: ops.clone(),
            ledger_tx,
            execution_gas_price: 1,
            storage_gas_price: 1,
        };

        let tx_hash = mantle_tx.hash();
        let ops_proofs = signing_keys
            .into_iter()
            .zip(ops)
            .map(|(key, _)| {
                OpProof::Ed25519Sig(key.sign_payload(tx_hash.as_signing_bytes().as_ref()))
            })
            .collect();

        let ledger_tx_proof = ZkKey::multi_sign(&[], &tx_hash.0).unwrap();

        SignedMantleTx::new(mantle_tx, ops_proofs, ledger_tx_proof)
            .expect("Test transaction should have valid signatures")
    }

    #[test]
    fn test_channel_blob_operation() {
        let cryptarchia_state = genesis_state(&[utxo()]);
        let test_config = config();
        let ledger_state = LedgerState::new(&test_config, cryptarchia_state.epoch_state());
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([1; 32]);

        let blob_op = BlobOp {
            channel: channel_id,
            session: 0u64,
            blob: [42; 32],
            blob_size: 1024,
            da_storage_gas_price: 10,
            parent: MsgId::root(),
            signer: verifying_key,
        };

        let tx = create_signed_tx(Op::ChannelBlob(blob_op), &signing_key);
        let result = ledger_state.try_apply_tx::<MainnetGasConstants>(
            0,
            &test_config,
            cryptarchia_state.latest_utxos(),
            tx,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_channel_inscribe_operation() {
        let cryptarchia_state = genesis_state(&[utxo()]);
        let test_config = config();
        let ledger_state = LedgerState::new(&test_config, cryptarchia_state.epoch_state());
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([2; 32]);

        let inscribe_op = InscriptionOp {
            channel_id,
            inscription: vec![1, 2, 3, 4],
            parent: MsgId::root(),
            signer: verifying_key,
        };

        let tx = create_signed_tx(Op::ChannelInscribe(inscribe_op), &signing_key);
        let result = ledger_state.try_apply_tx::<MainnetGasConstants>(
            0,
            &test_config,
            cryptarchia_state.latest_utxos(),
            tx,
        );
        assert!(result.is_ok());

        let (new_state, _) = result.unwrap();
        assert!(new_state.channels.channels.contains_key(&channel_id));
    }

    #[test]
    fn test_channel_set_keys_operation() {
        let cryptarchia_state = genesis_state(&[utxo()]);
        let test_config = config();
        let ledger_state = LedgerState::new(&test_config, cryptarchia_state.epoch_state());
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([3; 32]);

        let set_keys_op = SetKeysOp {
            channel: channel_id,
            keys: vec![verifying_key],
        };

        let tx = create_signed_tx(Op::ChannelSetKeys(set_keys_op), &signing_key);
        let result = ledger_state.try_apply_tx::<MainnetGasConstants>(
            0,
            &test_config,
            cryptarchia_state.latest_utxos(),
            tx,
        );
        assert!(result.is_ok());

        let (new_state, _) = result.unwrap();
        assert!(new_state.channels.channels.contains_key(&channel_id));
        assert_eq!(
            new_state.channels.channels.get(&channel_id).unwrap().keys,
            vec![verifying_key].into()
        );
    }

    #[test]
    fn test_invalid_parent_error() {
        let cryptarchia_state = genesis_state(&[utxo()]);
        let test_config = config();
        let mut ledger_state = LedgerState::new(&test_config, cryptarchia_state.epoch_state());
        let (signing_key, verifying_key) = create_test_keys();
        let channel_id = ChannelId::from([5; 32]);

        // First, create a channel with one message
        let first_blob = BlobOp {
            channel: channel_id,
            session: 0u64,
            blob: [1; 32],
            blob_size: 512,
            da_storage_gas_price: 5,
            parent: MsgId::root(),
            signer: verifying_key,
        };

        let first_tx = create_signed_tx(Op::ChannelBlob(first_blob), &signing_key);
        ledger_state = ledger_state
            .try_apply_tx::<MainnetGasConstants>(
                0,
                &test_config,
                cryptarchia_state.latest_utxos(),
                first_tx,
            )
            .unwrap()
            .0;

        // Now try to add a message with wrong parent
        let wrong_parent = MsgId::from([99; 32]);
        let second_blob = BlobOp {
            channel: channel_id,
            session: 0u64,
            blob: [2; 32],
            blob_size: 512,
            da_storage_gas_price: 5,
            parent: wrong_parent,
            signer: verifying_key,
        };

        let second_tx = create_signed_tx(Op::ChannelBlob(second_blob), &signing_key);
        let result = ledger_state.clone().try_apply_tx::<MainnetGasConstants>(
            0,
            &test_config,
            cryptarchia_state.latest_utxos(),
            second_tx,
        );
        assert!(matches!(
            result,
            Err(Error::Channel(channel::Error::InvalidParent { .. }))
        ));

        // Writing into an empty channel with a parent != MsgId::root() should also fail
        let empty_channel_id = ChannelId::from([8; 32]);
        let empty_blob = BlobOp {
            channel: empty_channel_id,
            session: 0u64,
            blob: [3; 32],
            blob_size: 512,
            da_storage_gas_price: 5,
            parent: MsgId::from([1; 32]), // non-root parent
            signer: verifying_key,
        };

        let empty_tx = create_signed_tx(Op::ChannelBlob(empty_blob), &signing_key);
        let empty_result = ledger_state.try_apply_tx::<MainnetGasConstants>(
            0,
            &test_config,
            cryptarchia_state.latest_utxos(),
            empty_tx,
        );
        assert!(matches!(
            empty_result,
            Err(Error::Channel(channel::Error::InvalidParent { .. }))
        ));
    }

    #[test]
    fn test_unauthorized_signer_error() {
        let cryptarchia_state = genesis_state(&[utxo()]);
        let test_config = config();
        let mut ledger_state = LedgerState::new(&test_config, cryptarchia_state.epoch_state());
        let (signing_key, verifying_key) = create_test_keys();
        let (unauthorized_signing_key, unauthorized_verifying_key) = create_test_keys_with_seed(3);
        let channel_id = ChannelId::from([6; 32]);

        // First, create a channel with authorized signer
        let first_blob = BlobOp {
            channel: channel_id,
            session: 0u64,
            blob: [1; 32],
            blob_size: 512,
            da_storage_gas_price: 5,
            parent: MsgId::root(),
            signer: verifying_key,
        };

        let correct_parent = first_blob.id();
        let first_tx = create_signed_tx(Op::ChannelBlob(first_blob), &signing_key);
        ledger_state = ledger_state
            .try_apply_tx::<MainnetGasConstants>(
                0,
                &test_config,
                cryptarchia_state.latest_utxos(),
                first_tx,
            )
            .unwrap()
            .0;

        // Now try to add a message with unauthorized signer
        let second_blob = BlobOp {
            channel: channel_id,
            session: 0u64,
            blob: [2; 32],
            blob_size: 512,
            da_storage_gas_price: 5,
            parent: correct_parent,
            signer: unauthorized_verifying_key,
        };

        let second_tx = create_signed_tx(Op::ChannelBlob(second_blob), &unauthorized_signing_key);
        let result = ledger_state.try_apply_tx::<MainnetGasConstants>(
            0,
            &test_config,
            cryptarchia_state.latest_utxos(),
            second_tx,
        );
        assert!(matches!(
            result,
            Err(Error::Channel(channel::Error::UnauthorizedSigner { .. }))
        ));
    }

    #[test]
    fn test_empty_keys_error() {
        let cryptarchia_state = genesis_state(&[utxo()]);
        let test_config = config();
        let ledger_state = LedgerState::new(&test_config, cryptarchia_state.epoch_state());
        let (signing_key, _) = create_test_keys();
        let channel_id = ChannelId::from([7; 32]);

        let set_keys_op = SetKeysOp {
            channel: channel_id,
            keys: vec![],
        };

        let tx = create_signed_tx(Op::ChannelSetKeys(set_keys_op), &signing_key);
        let result = ledger_state.try_apply_tx::<MainnetGasConstants>(
            0,
            &test_config,
            cryptarchia_state.latest_utxos(),
            tx,
        );
        assert_eq!(
            result,
            Err(Error::Channel(channel::Error::EmptyKeys { channel_id }))
        );
    }

    #[test]
    fn test_multiple_operations_in_transaction() {
        let cryptarchia_state = genesis_state(&[utxo()]);
        let test_config = config();
        // Create channel 1 by posting a blob
        // Create channel 2 by posting an inscription
        // Change the keys for channel 1
        // Post another blob in channel 1
        let ledger_state = LedgerState::new(&test_config, cryptarchia_state.epoch_state());
        let (sk1, vk1) = create_test_keys_with_seed(1);
        let (sk2, vk2) = create_test_keys_with_seed(2);
        let (_, vk3) = create_test_keys_with_seed(3);
        let (sk4, vk4) = create_test_keys_with_seed(4);

        let channel1 = ChannelId::from([10; 32]);
        let channel2 = ChannelId::from([20; 32]);

        let blob_op = BlobOp {
            channel: channel1,
            session: 0u64,
            blob: [42; 32],
            blob_size: 1024,
            da_storage_gas_price: 10,
            parent: MsgId::root(),
            signer: vk1,
        };

        let inscribe_op = InscriptionOp {
            channel_id: channel2,
            inscription: vec![1, 2, 3, 4],
            parent: MsgId::root(),
            signer: vk2,
        };

        let set_keys_op = SetKeysOp {
            channel: channel1,
            keys: vec![vk3, vk4],
        };

        let blob_op2 = BlobOp {
            channel: channel1,
            session: 0u64,
            blob: [43; 32],
            blob_size: 2048,
            da_storage_gas_price: 20,
            parent: blob_op.id(),
            signer: vk4,
        };

        let ops = vec![
            Op::ChannelBlob(blob_op),
            Op::ChannelInscribe(inscribe_op),
            Op::ChannelSetKeys(set_keys_op),
            Op::ChannelBlob(blob_op2.clone()),
        ];
        let tx = create_multi_signed_tx(ops, vec![&sk1, &sk2, &sk1, &sk4]);

        let result = ledger_state
            .try_apply_tx::<MainnetGasConstants>(
                0,
                &test_config,
                cryptarchia_state.latest_utxos(),
                tx,
            )
            .unwrap()
            .0;

        assert!(result.channels.channels.contains_key(&channel1));
        assert!(result.channels.channels.contains_key(&channel2));
        assert_eq!(
            result.channels.channels.get(&channel1).unwrap().tip,
            blob_op2.id()
        );
    }

    // TODO: Update this test to work with the new SDP API
    // This test needs to be rewritten to use the new SDP ledger API which no longer
    // exposes get_declaration() or uses declaration_id() methods.
    // #[test]
    // #[expect(clippy::, reason = "Test function.")]
    #[test]
    fn _test_sdp_withdraw_operation() {
        // This test has been disabled pending API updates
    }
}
