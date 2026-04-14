use std::sync::LazyLock;

use ark_ff::PrimeField as _;
use bytes::Bytes;
use lb_groth16::{Fr, fr_from_bytes, serde::serde_fr};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_poseidon2::Digest as _;
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};

use crate::crypto::{Hash, ZkHasher};

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
    pub op_id: Hash,
    pub output_index: usize,
    pub note: Note,
}

static NOTE_ID_V1: LazyLock<Fr> = LazyLock::new(|| {
    fr_from_bytes(b"NOTE_ID_V1").expect("BigUint should load from constant string")
});

impl Utxo {
    #[must_use]
    pub const fn new(op_id: Hash, output_index: usize, note: Note) -> Self {
        Self {
            op_id,
            output_index,
            note,
        }
    }

    #[must_use]
    pub fn id(&self) -> NoteId {
        // constants and structure as defined in the Mantle spec:
        // https://www.notion.so/nomos-tech/v1-4-Mantle-Specification-335261aa09df8065a38acff4b25aee82

        let op_id: Fr = Fr::from_le_bytes_mod_order(self.op_id.as_ref());
        let output_index =
            fr_from_bytes(self.output_index.to_le_bytes().as_slice()).expect("usize fits in Fr");
        let note_value: Fr =
            fr_from_bytes(self.note.value.to_le_bytes().as_slice()).expect("u64 fits in Fr");
        let note_pk: Fr = self.note.pk.into();

        NoteId(ZkHasher::digest(&[
            *NOTE_ID_V1,
            op_id,
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
            [0u8; 32],
            0,
            Note::new(100, ZkPublicKey::from(Fr::from(BigUint::from(456u32)))),
        );
        assert_eq!(
            utxo.id(),
            NoteId::from(
                Fr::from_str(
                    "7557997998773395727489806263315711564569794358720487479582958381680367418066"
                )
                .unwrap()
            )
        );
    }
}
