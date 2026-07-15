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
    helper: &impl OperationVerificationHelper,
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
        if let Err(_error) = public_key.verify(tx_hash_bytes.as_ref(), &signature.signature) {
            return Err(VerificationError::ChannelMultiSigProofInvalidSignature {
                op_index,
                signature_index: i,
            });
        }
    }

    Ok(())
}
