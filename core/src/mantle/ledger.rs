use std::{collections::HashSet, slice, sync::LazyLock};

use ark_ff::PrimeField as _;
use bytes::Bytes;
use lb_groth16::{Fr, fr_from_bytes, serde::serde_fr};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_poseidon2::Digest as _;
use lb_utxotree::UtxoTree;
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    crypto::{Hash, ZkHasher},
    mantle::ops::OpId,
    sdp::{Declaration, DeclarationId, locked_notes::LockedNotes},
};

pub trait Operation<ValidationContext> {
    type ExecutionContext<'a>
    where
        Self: 'a;
    type Error;
    fn validate(&self, ctx: &ValidationContext) -> Result<(), Self::Error>;
    fn execute(
        &self,
        ctx: Self::ExecutionContext<'_>,
    ) -> Result<Self::ExecutionContext<'_>, Self::Error>;
}

pub type Utxos = UtxoTree<NoteId, Utxo, ZkHasher>;
pub type Declarations = rpds::RedBlackTreeMapSync<DeclarationId, Declaration>;

pub type Value = u64;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum InputsError {
    #[error("Note: {0:?} isn't in the ledger")]
    InexistingNote(NoteId),
    #[error("Locked note: {0:?}")]
    LockedNote(NoteId),
    #[error("Inputs contain try to double spend the same NoteId")]
    DoubleSpend,
    #[error("Sum of input values overflows")]
    InputsOverflow,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum OutputsError {
    #[error("Zero value note")]
    ZeroValueNote,
    #[error("Sum of output values overflows")]
    OutputsOverflow,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LedgerError {
    #[error("Inputs error: {0}")]
    Inputs(#[from] InputsError),
    #[error("Outputs error: {0}")]
    Outputs(#[from] OutputsError),
}

#[derive(Clone, Eq, Debug, PartialEq, Serialize, Deserialize)]
pub struct Outputs(Vec<Note>);

impl Outputs {
    #[must_use]
    pub const fn new(notes: Vec<Note>) -> Self {
        Self(notes)
    }

    pub fn utxos<O: OpId>(&self, op: &O) -> impl Iterator<Item = Utxo> {
        self.0.iter().enumerate().map(move |(index, note)| Utxo {
            op_id: op.op_id(),
            output_index: index,
            note: *note,
        })
    }

    pub fn utxo_by_index<O: OpId>(&self, index: usize, op: &O) -> Option<Utxo> {
        self.0.get(index).map(|note| Utxo {
            op_id: op.op_id(),
            output_index: index,
            note: *note,
        })
    }

    pub fn validate(&self) -> Result<(), OutputsError> {
        // Check that there is no duplicate
        for note in &self.0 {
            if note.value == 0 {
                return Err(OutputsError::ZeroValueNote);
            }
        }
        Ok(())
    }

    pub fn execute<O: OpId>(&self, mut utxos: Utxos, op: &O) -> Utxos {
        for utxo in self.utxos(op) {
            utxos = utxos.insert(utxo.id(), utxo).0;
        }
        utxos
    }

    pub fn amount(&self) -> Result<Value, OutputsError> {
        let mut amount: Value = 0;
        for output in &self.0 {
            amount = amount
                .checked_add(output.value)
                .ok_or(OutputsError::OutputsOverflow)?;
        }
        Ok(amount)
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> slice::Iter<'_, Note> {
        <&Self as IntoIterator>::into_iter(self)
    }
}

impl AsRef<Vec<Note>> for Outputs {
    fn as_ref(&self) -> &Vec<Note> {
        &self.0
    }
}

impl AsMut<Vec<Note>> for Outputs {
    fn as_mut(&mut self) -> &mut Vec<Note> {
        &mut self.0
    }
}

impl<'output> IntoIterator for &'output Outputs {
    type Item = <slice::Iter<'output, Note> as IntoIterator>::Item;
    type IntoIter = slice::Iter<'output, Note>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

#[derive(Clone, Eq, Debug, PartialEq, Hash, Serialize, Deserialize)]
pub struct Inputs(Vec<NoteId>);

impl Inputs {
    #[must_use]
    pub const fn new(note_ids: Vec<NoteId>) -> Self {
        Self(note_ids)
    }

    pub fn validate(&self, locked_notes: &LockedNotes, utxos: &Utxos) -> Result<(), InputsError> {
        // Check that there is no duplicate
        let unique: HashSet<_> = self.0.iter().collect();
        if unique.len() != self.0.len() {
            return Err(InputsError::DoubleSpend);
        }
        // Check each note is spendable
        for input in &self.0 {
            // Check the note isn't locked
            if locked_notes.contains(input) {
                return Err(InputsError::LockedNote(*input));
            }
            // Check the note exist in the ledger
            if !utxos.contains(input) {
                return Err(InputsError::InexistingNote(*input));
            }
        }
        Ok(())
    }

    pub fn execute(&self, mut utxos: Utxos) -> Result<Utxos, InputsError> {
        // Remove notes from the ledger one by one
        for input in &self.0 {
            (utxos, _) = utxos
                .remove(input)
                .map_err(|_| InputsError::InexistingNote(*input))?;
        }
        Ok(utxos)
    }

    pub fn amount(&self, utxos: &Utxos) -> Result<Value, InputsError> {
        let mut amount: Value = 0;
        for input in &self.0 {
            let utxo = utxos
                .get(input)
                .ok_or(InputsError::InexistingNote(*input))?;
            amount = amount
                .checked_add(utxo.note.value)
                .ok_or(InputsError::InputsOverflow)?;
        }
        Ok(amount)
    }

    pub fn get_pk(&self, utxos: &Utxos) -> Result<Vec<ZkPublicKey>, InputsError> {
        let mut pks: Vec<ZkPublicKey> = vec![];
        for input in &self.0 {
            let utxo = utxos
                .get(input)
                .ok_or(InputsError::InexistingNote(*input))?;
            pks.push(utxo.note.pk);
        }
        Ok(pks)
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> slice::Iter<'_, NoteId> {
        <&Self as IntoIterator>::into_iter(self)
    }
}

impl AsRef<Vec<NoteId>> for Inputs {
    fn as_ref(&self) -> &Vec<NoteId> {
        &self.0
    }
}

impl AsMut<Vec<NoteId>> for Inputs {
    fn as_mut(&mut self) -> &mut Vec<NoteId> {
        &mut self.0
    }
}
impl<'input> IntoIterator for &'input Inputs {
    type Item = <slice::Iter<'input, NoteId> as IntoIterator>::Item;
    type IntoIter = slice::Iter<'input, NoteId>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

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
