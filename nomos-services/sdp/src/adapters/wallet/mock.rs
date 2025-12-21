use std::convert::Infallible;

use key_management_system_keys::keys::{Ed25519Key, ZkKey, ZkPublicKey};
use nomos_core::{
    mantle::{NoteId, Op, OpProof, SignedMantleTx, Transaction as _, tx_builder::MantleTxBuilder},
    sdp::{ActiveMessage, DeclarationMessage, WithdrawMessage},
};

use crate::adapters::wallet::SdpWalletAdapter;

pub struct MockWalletAdapter;

impl SdpWalletAdapter for MockWalletAdapter {
    type Error = Infallible;

    fn new() -> Self {
        Self {}
    }
    fn declare_tx(
        &self,
        tx_builder: MantleTxBuilder,
        declaration: Box<DeclarationMessage>,
    ) -> Result<SignedMantleTx, Self::Error> {
        // todo: this is for mock, we need signing key in production
        let signing_key = Ed25519Key::from_bytes(&[0u8; 32]);
        let zk_key = ZkKey::zero();

        let declare_op = Op::SDPDeclare(*declaration);
        let mantle_tx = tx_builder.push_op(declare_op).build();
        let tx_hash = mantle_tx.hash();

        let ed25519_sig = signing_key.sign_payload(&tx_hash.as_signing_bytes());
        let zk_sig = zk_key.sign_payload(tx_hash.as_ref()).unwrap();

        Ok(SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::ZkAndEd25519Sigs {
                zk_sig,
                ed25519_sig,
            }],
            ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap(),
        )
        .expect("Transaction with valid signature should be valid"))
    }

    fn withdraw_tx(
        &self,
        tx_builder: MantleTxBuilder,
        withdrawn_message: WithdrawMessage,
        zk_id: ZkPublicKey,
        _locked_note_id: NoteId,
    ) -> Result<SignedMantleTx, Self::Error> {
        // todo: this is for mock, we need signing key in production
        let zk_sk = ZkKey::zero();
        let locked_note_sk = ZkKey::zero();
        assert_eq!(zk_sk.to_public_key(), zk_id);

        // Build the Op
        let withdraw_op = Op::SDPWithdraw(withdrawn_message);
        let mantle_tx = tx_builder.push_op(withdraw_op).build();
        let tx_hash = mantle_tx.hash();

        // From spec: ZkSignature_verify(txhash, signature, [locked_note.pk,
        // declare_info.zk_id])
        let zk_signature = ZkKey::multi_sign(&[locked_note_sk, zk_sk], tx_hash.as_ref()).unwrap();

        Ok(SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::ZkSig(zk_signature)],
            ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap(),
        )
        .expect("Transaction with valid signature should be valid"))
    }

    fn active_tx(
        &self,
        tx_builder: MantleTxBuilder,
        active_message: ActiveMessage,
        zk_id: ZkPublicKey,
    ) -> Result<SignedMantleTx, Self::Error> {
        // todo: this is for mock, we need signing key in production
        let zk_sk = ZkKey::zero();
        assert_eq!(zk_sk.to_public_key(), zk_id);

        let active_op = Op::SDPActive(active_message);
        let mantle_tx = tx_builder.push_op(active_op).build();
        let tx_hash = mantle_tx.hash();

        let zk_signature = zk_sk.sign_payload(tx_hash.as_ref()).unwrap();

        Ok(SignedMantleTx::new(
            mantle_tx,
            vec![OpProof::ZkSig(zk_signature)],
            ZkKey::multi_sign(&[], tx_hash.as_ref()).unwrap(),
        )
        .expect("Transaction with valid signature should be valid"))
    }
}
