use std::sync::LazyLock;

use bytes::Bytes;
use lb_groth16::{Fr, fr_from_bytes, serde::serde_fr};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_poseidon2::Digest as _;
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};

use crate::{crypto::ZkHasher, mantle::tx::TxHash};

pub type Value = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NoteId(#[serde(with = "serde_fr")] pub Fr);

impl NoteId {
    #[must_use]
    pub const fn as_fr(&self) -> &Fr {
        &self.0
    }

    #[must_use]
    pub fn as_bytes(&self) -> Bytes {
        self.0.0.0.iter().flat_map(|b| b.to_le_bytes()).collect()
    }
}

impl AsRef<Fr> for NoteId {
    fn as_ref(&self) -> &Fr {
        &self.0
    }
}

impl From<Fr> for NoteId {
    fn from(n: Fr) -> Self {
        Self(n)
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Serialize, Deserialize)]
pub struct Note {
    pub value: Value,
    pub pk: ZkPublicKey,
}

impl Note {
    #[must_use]
    pub const fn new(value: Value, pk: ZkPublicKey) -> Self {
        Self { value, pk }
    }

    #[must_use]
    pub fn as_fr_components(&self) -> [Fr; 2] {
        [BigUint::from(self.value).into(), *self.pk.as_fr()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Utxo {
    pub transfer_hash: TxHash,
    pub output_index: usize,
    pub note: Note,
}

static NOTE_ID_V1: LazyLock<Fr> = LazyLock::new(|| {
    fr_from_bytes(b"NOTE_ID_V1").expect("BigUint should load from constant string")
});

impl Utxo {
    #[must_use]
    pub const fn new(transfer_hash: TxHash, output_index: usize, note: Note) -> Self {
        Self {
            transfer_hash,
            output_index,
            note,
        }
    }

    #[must_use]
    pub fn id(&self) -> NoteId {
        // constants and structure as defined in the Mantle spec:
        // https://www.notion.so/nomos-tech/v1-3-Mantle-Specification-31e261aa09df818f9327ee87e5a6d433#31e261aa09df80aea7cff4eb98d61b6e

        let transfer_hash: Fr = *self.transfer_hash.as_ref();
        let output_index =
            fr_from_bytes(self.output_index.to_le_bytes().as_slice()).expect("usize fits in Fr");
        let note_value: Fr =
            fr_from_bytes(self.note.value.to_le_bytes().as_slice()).expect("u64 fits in Fr");
        let note_pk: Fr = self.note.pk.into();

        NoteId(ZkHasher::digest(&[
            *NOTE_ID_V1,
            transfer_hash,
            output_index,
            note_value,
            note_pk,
        ]))
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr as _;

    use super::*;

    /// Test that [`NoteId`] is derived correctly with known values.
    ///
    /// NOTE: This test must be updated if the [`NoteId`] derivation changes.
    #[test]
    fn test_note_id() {
        let utxo = Utxo::new(
            TxHash::from(Fr::from(BigUint::from(123u32))),
            0,
            Note::new(100, ZkPublicKey::from(Fr::from(BigUint::from(456u32)))),
        );
        assert_eq!(
            utxo.id(),
            NoteId::from(
                Fr::from_str(
                    "7000453536948078697982837270969513402421497654766692285707895413806329167703"
                )
                .unwrap()
            )
        );
    }
}
