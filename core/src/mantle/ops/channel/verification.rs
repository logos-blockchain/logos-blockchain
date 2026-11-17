#[cfg(test)]
pub mod test_utils {
    use lb_key_management_system_keys::keys::Ed25519Key;

    use crate::{
        mantle::{TxHash, ops::channel::ChannelKeyIndex},
        proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignatures},
    };

    #[must_use]
    pub fn create_channel_multi_sig_proof(
        tx_hash: &TxHash,
        signing_keys: &[&Ed25519Key],
    ) -> ChannelMultiSigProof {
        let signatures = IndexedSignatures::try_from_iter(signing_keys.iter().enumerate().map(
            |(index, key)| {
                (
                    index as ChannelKeyIndex,
                    key.sign_payload(tx_hash.as_signing_bytes()),
                )
            },
        ))
        .unwrap();
        ChannelMultiSigProof::new(signatures)
    }
}
