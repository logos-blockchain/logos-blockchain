use std::collections::HashSet;

use bytes::Bytes;

use crate::{
    mantle::{
        VerificationError, ops::channel::ChannelId, transactions::OperationVerificationHelper,
    },
    proofs::channel_multi_sig_proof::ChannelMultiSigProof,
};

pub fn verify_channel_multi_sig(
    channel_id: &ChannelId,
    proof: &ChannelMultiSigProof,
    tx_hash_bytes: &Bytes,
    helper: &dyn OperationVerificationHelper,
    op_index: usize,
) -> Result<(), VerificationError> {
    let transfer_threshold = helper.get_channel_transfer_threshold(channel_id)?;

    let signatures = proof.signatures();
    let signatures_len = signatures.len();
    if signatures_len != transfer_threshold as usize {
        return Err(VerificationError::ChannelMultiSigProofNotEnoughSignatures {
            op_index,
            actual: signatures_len,
            required: transfer_threshold,
        });
    }

    let indices_set = signatures
        .iter()
        .map(|signature| signature.channel_key_index)
        .collect::<HashSet<_>>();
    let indices_set_len = indices_set.len();
    if indices_set_len != signatures_len {
        return Err(VerificationError::ChannelMultiSigProofDuplicateIndices { op_index });
    }

    for (i, signature) in signatures.iter().enumerate() {
        let public_key =
            helper.get_key_from_channel_at_index(channel_id, &signature.channel_key_index)?;
        if let Err(_error) = public_key.verify(tx_hash_bytes, &signature.signature) {
            return Err(VerificationError::ChannelMultiSigProofInvalidSignature {
                op_index,
                signature_index: i,
            });
        }
    }

    Ok(())
}

#[cfg(test)]
pub mod test_utils {
    use lb_key_management_system_keys::keys::Ed25519Key;

    use crate::{
        mantle::{TxHash, ops::channel::ChannelKeyIndex},
        proofs::channel_multi_sig_proof::{
            ChannelMultiSigProof, IndexedSignature, IndexedSignatures,
        },
    };

    #[must_use]
    pub fn create_channel_multi_sig_proof(
        tx_hash: &TxHash,
        signing_keys: &[&Ed25519Key],
    ) -> ChannelMultiSigProof {
        let signatures: IndexedSignatures = signing_keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                IndexedSignature::new(
                    index as ChannelKeyIndex,
                    key.sign_payload(tx_hash.as_signing_bytes().as_ref()),
                )
            })
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        ChannelMultiSigProof::try_new(signatures).unwrap()
    }
}

// `verify_channel_multi_sig` is the responsible of the distinction between
// `NotEnoughSignatures`, `DuplicateIndices`, and `InvalidSignature` errors.
// Callers (e.g. `ChannelWithdrawOp::validate`) currently collapse all three
// into `Error::InvalidSignature`, so this is the only place that can still
// assert the precise reason a proof was rejected.
#[cfg(test)]
mod tests {
    use lb_key_management_system_keys::keys::Ed25519Key;

    use super::{test_utils::create_channel_multi_sig_proof, verify_channel_multi_sig};
    use crate::mantle::{
        VerificationError,
        channel::Channels,
        ops::channel::ChannelId,
        transactions::{
            TxHash, tx_list::signed_ops::test_utils::make_channel_state,
            verification_helper::test_utils::TestOperationVerificationHelper,
        },
    };

    #[test]
    fn rejects_missing_channel() {
        let channel_id = ChannelId::from([1u8; 32]);
        let tx_hash = TxHash::from([9u8; 32]);
        let tx_hash_bytes = tx_hash.as_signing_bytes();
        let key0 = Ed25519Key::from_bytes(&[0; 32]);
        let proof = create_channel_multi_sig_proof(&tx_hash, &[&key0]);

        let helper = TestOperationVerificationHelper::new(Channels::new(), []);

        let result = verify_channel_multi_sig(&channel_id, &proof, &tx_hash_bytes, &helper, 0);

        assert_eq!(
            result,
            Err(VerificationError::ChannelNotFound { channel_id })
        );
    }

    #[test]
    fn rejects_not_enough_signatures() {
        let channel_id = ChannelId::from([2u8; 32]);
        let tx_hash = TxHash::from([9u8; 32]);
        let tx_hash_bytes = tx_hash.as_signing_bytes();
        let key0 = Ed25519Key::from_bytes(&[0; 32]);
        let proof = create_channel_multi_sig_proof(&tx_hash, &[&key0]);

        let channels = {
            let mut channels = Channels::new();
            channels
                .channels
                .insert_mut(channel_id, make_channel_state(2, None));
            channels
        };
        let helper =
            TestOperationVerificationHelper::new(channels, [((channel_id, 0), key0.public_key())]);

        let result = verify_channel_multi_sig(&channel_id, &proof, &tx_hash_bytes, &helper, 0);

        assert_eq!(
            result,
            Err(VerificationError::ChannelMultiSigProofNotEnoughSignatures {
                op_index: 0,
                actual: 1,
                required: 2,
            })
        );
    }

    #[test]
    fn rejects_missing_key() {
        let channel_id = ChannelId::from([3u8; 32]);
        let tx_hash = TxHash::from([9u8; 32]);
        let tx_hash_bytes = tx_hash.as_signing_bytes();
        let key0 = Ed25519Key::from_bytes(&[0; 32]);
        let proof = create_channel_multi_sig_proof(&tx_hash, &[&key0]);

        let channels = {
            let mut channels = Channels::new();
            channels
                .channels
                .insert_mut(channel_id, make_channel_state(1, None));
            channels
        };
        let helper = TestOperationVerificationHelper::new(channels, []);

        let result = verify_channel_multi_sig(&channel_id, &proof, &tx_hash_bytes, &helper, 0);

        assert_eq!(
            result,
            Err(VerificationError::KeyNotFound {
                channel_id,
                key_index: 0,
            })
        );
    }

    #[test]
    fn rejects_invalid_signature() {
        let channel_id = ChannelId::from([4u8; 32]);
        let tx_hash = TxHash::from([9u8; 32]);
        let tx_hash_bytes = tx_hash.as_signing_bytes();
        let expected_key = Ed25519Key::from_bytes(&[0; 32]);
        let wrong_key = Ed25519Key::from_bytes(&[9; 32]);
        let proof = create_channel_multi_sig_proof(&tx_hash, &[&wrong_key]);

        let channels = {
            let mut channels = Channels::new();
            channels
                .channels
                .insert_mut(channel_id, make_channel_state(1, None));
            channels
        };
        let helper = TestOperationVerificationHelper::new(
            channels,
            [((channel_id, 0), expected_key.public_key())],
        );

        let result = verify_channel_multi_sig(&channel_id, &proof, &tx_hash_bytes, &helper, 0);

        assert_eq!(
            result,
            Err(VerificationError::ChannelMultiSigProofInvalidSignature {
                op_index: 0,
                signature_index: 0,
            })
        );
    }
}
