pub mod share;

use std::io::Cursor;

use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use blake2::{
    Blake2b, Digest,
    digest::{Update as _, VariableOutput as _, consts::U32},
};
use lb_kzgrs::Commitment;
#[cfg(feature = "parallel")]
use rayon::iter::{IntoParallelRefIterator as _, ParallelIterator};
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeSeq as _};

#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize, Hash)]
pub struct Chunk(pub Vec<u8>);
#[derive(Clone)]
pub struct Row(pub Vec<Chunk>);
#[derive(Debug, Clone, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct Column(pub Vec<Chunk>);
#[derive(Clone)]
pub struct ChunksMatrix(pub Vec<Row>);
pub type ShareIndex = u16;

impl Chunk {
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
    #[must_use]
    pub fn as_bytes(&self) -> Vec<u8> {
        self.0.clone()
    }
    #[must_use]
    pub const fn empty() -> Self {
        Self(vec![])
    }
}

impl From<&[u8]> for Chunk {
    fn from(value: &[u8]) -> Self {
        Self(value.to_vec())
    }
}

impl Row {
    pub fn iter(&self) -> impl Iterator<Item = &Chunk> {
        self.0.iter()
    }
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn as_bytes(&self) -> Vec<u8> {
        self.0.iter().flat_map(Chunk::as_bytes).collect()
    }
}

impl Column {
    pub fn iter(&self) -> impl Iterator<Item = &Chunk> {
        self.0.iter()
    }
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn as_bytes(&self) -> Vec<u8> {
        self.0.iter().flat_map(Chunk::as_bytes).collect()
    }
}

impl FromIterator<Chunk> for Row {
    fn from_iter<T: IntoIterator<Item = Chunk>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl FromIterator<Chunk> for Column {
    fn from_iter<T: IntoIterator<Item = Chunk>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl AsRef<[Chunk]> for Row {
    fn as_ref(&self) -> &[Chunk] {
        &self.0
    }
}

impl AsRef<[Chunk]> for Column {
    fn as_ref(&self) -> &[Chunk] {
        &self.0
    }
}

impl ChunksMatrix {
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn rows(&self) -> impl Iterator<Item = &Row> + '_ {
        self.0.iter()
    }
    #[cfg(feature = "parallel")]
    #[must_use]
    pub fn par_rows(&self) -> impl ParallelIterator<Item = &Row> + '_ {
        self.0.par_iter()
    }
    pub fn columns(&self) -> impl Iterator<Item = Column> + '_ {
        let size = self.0.first().map_or(0, |r| r.0.len());
        (0..size).map(|i| {
            self.0
                .iter()
                .map(|row| row.0.get(i).cloned().unwrap_or_else(Chunk::empty))
                .collect::<Column>()
        })
    }

    #[must_use]
    pub fn transposed(&self) -> Self {
        Self(self.columns().map(|c| Row(c.0)).collect())
    }

    pub fn iter(&self) -> impl Iterator<Item = &Row> {
        self.0.iter()
    }
}

impl FromIterator<Row> for ChunksMatrix {
    fn from_iter<T: IntoIterator<Item = Row>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

#[must_use]
pub fn hash_commitment<const HASH_SIZE: usize>(commitment: &Commitment) -> [u8; HASH_SIZE] {
    let mut hasher = blake2::Blake2bVar::new(HASH_SIZE)
        .unwrap_or_else(|e| panic!("Blake2b should work for size {HASH_SIZE}, {e}"));
    hasher.update(commitment_to_bytes(commitment).as_ref());
    hasher
        .finalize_boxed()
        .to_vec()
        .try_into()
        .unwrap_or_else(|_| panic!("Size is guaranteed by constant {HASH_SIZE:?}"))
}

#[must_use]
pub fn build_blob_id(rows_commitments: &[Commitment]) -> [u8; 32] {
    let mut hasher = Blake2b::<U32>::new();
    for c in rows_commitments {
        Digest::update(&mut hasher, commitment_to_bytes(c));
    }
    hasher.finalize().into()
}

#[must_use]
pub fn commitment_to_bytes(commitment: &Commitment) -> Vec<u8> {
    let mut buff = Cursor::new(vec![]);
    commitment
        .serialize_uncompressed(&mut buff)
        .expect("Serialization of commitment should work");
    buff.into_inner()
}

pub fn serialize_canonical<S, T>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: CanonicalSerialize,
{
    let mut bytes = Vec::new();
    value
        .serialize_compressed(&mut bytes)
        .map_err(serde::ser::Error::custom)?;
    serializer.serialize_bytes(&bytes)
}

pub fn deserialize_canonical<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: CanonicalDeserialize,
{
    let bytes: Vec<u8> = Deserialize::deserialize(deserializer)?;
    let mut cursor = Cursor::new(bytes);
    T::deserialize_compressed(&mut cursor).map_err(serde::de::Error::custom)
}

pub fn serialize_vec_canonical<S, T>(values: &[T], serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: CanonicalSerialize,
{
    let mut container = serializer.serialize_seq(Some(values.len()))?;
    for value in values {
        let mut bytes = Vec::new();
        value
            .serialize_compressed(&mut bytes)
            .map_err(serde::ser::Error::custom)?;
        container.serialize_element(&bytes)?;
    }
    container.end()
}

pub fn deserialize_vec_canonical<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: CanonicalDeserialize,
{
    let bytes_vecs: Vec<Vec<u8>> = Deserialize::deserialize(deserializer)?;
    bytes_vecs
        .iter()
        .map(|bytes| {
            let mut cursor = Cursor::new(bytes);
            T::deserialize_compressed(&mut cursor).map_err(serde::de::Error::custom)
        })
        .collect()
}
