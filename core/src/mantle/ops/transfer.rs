use std::sync::LazyLock;

use lb_groth16::{Fr, fr_from_bytes, fr_from_bytes_unchecked};
use lb_poseidon2::Digest;
use serde::{Deserialize, Serialize};

use crate::{
    crypto::{Digest as _, HALF_BLAKE_DIGEST_BYTES_SIZE, Hasher, ZkHasher},
    mantle::{
        Note, NoteId, Transaction, TransactionHasher, TxHash, Utxo, encoding::encode_transfer_op,
    },
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransferOp {
    pub inputs: Vec<NoteId>,
    pub outputs: Vec<Note>,
}

static TRANSFER_HASH_V1_FR: LazyLock<Fr> =
    LazyLock::new(|| fr_from_bytes(b"TRANSFER_HASH_V1").expect("Constant should be valid Fr"));

impl TransferOp {
    #[must_use]
    pub const fn new(inputs: Vec<NoteId>, outputs: Vec<Note>) -> Self {
        Self { inputs, outputs }
    }

    #[must_use]
    pub fn as_signing_frs(&self) -> Vec<Fr> {
        // constants and structure as defined in the Mantle spec:
        // <https://www.notion.so/nomos-tech/v1-3-Mantle-Specification-31e261aa09df818f9327ee87e5a6d433#31e261aa09df80aea7cff4eb98d61b6e>
        let encoded_bytes = encode_transfer_op(self);
        let first_blake_hash = Hasher::digest(encoded_bytes);
        let frs = first_blake_hash
            .as_slice()
            .chunks(HALF_BLAKE_DIGEST_BYTES_SIZE)
            .map(fr_from_bytes_unchecked);
        std::iter::once(*TRANSFER_HASH_V1_FR).chain(frs).collect()
    }

    #[must_use]
    pub fn utxo_by_index(&self, index: usize) -> Option<Utxo> {
        self.outputs.get(index).map(|note| Utxo {
            transfer_hash: self.hash(),
            output_index: index,
            note: *note,
        })
    }

    pub fn utxos(&self) -> impl Iterator<Item = Utxo> + '_ {
        let transfer_hash = self.hash();
        self.outputs
            .iter()
            .enumerate()
            .map(move |(index, note)| Utxo {
                transfer_hash,
                output_index: index,
                note: *note,
            })
    }
}

impl Transaction for TransferOp {
    const HASHER: TransactionHasher<Self> =
        |op| <ZkHasher as Digest>::digest(&op.as_signing_frs()).into();
    type Hash = TxHash;

    fn as_signing_frs(&self) -> Vec<Fr> {
        Self::as_signing_frs(self)
    }
}

#[cfg(test)]
mod test {

    use lb_key_management_system_keys::keys::ZkPublicKey;
    use num_bigint::BigUint;

    use super::*;

    #[test]
    fn test_utxo_by_index() {
        let pk0 = ZkPublicKey::from(Fr::from(BigUint::from(0u8)));
        let pk1 = ZkPublicKey::from(Fr::from(BigUint::from(1u8)));
        let pk2 = ZkPublicKey::from(Fr::from(BigUint::from(2u8)));
        let transfer = TransferOp {
            inputs: vec![NoteId(BigUint::from(0u8).into())],
            outputs: vec![
                Note::new(100, pk0),
                Note::new(200, pk1),
                Note::new(300, pk2),
            ],
        };
        assert_eq!(
            transfer.utxo_by_index(0),
            Some(Utxo {
                transfer_hash: transfer.hash(),
                output_index: 0,
                note: Note::new(100, pk0),
            })
        );
        assert_eq!(
            transfer.utxo_by_index(1),
            Some(Utxo {
                transfer_hash: transfer.hash(),
                output_index: 1,
                note: Note::new(200, pk1),
            })
        );
        assert_eq!(
            transfer.utxo_by_index(2),
            Some(Utxo {
                transfer_hash: transfer.hash(),
                output_index: 2,
                note: Note::new(300, pk2),
            })
        );

        assert!(transfer.utxo_by_index(3).is_none());
    }
}
