use std::hash::{Hash, Hasher};

use lb_core::da::{
    BlobId,
    blob::{self, metadata::Next},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct BlobInfo {
    id: BlobId,
    metadata: Metadata,
}

impl BlobInfo {
    #[must_use]
    pub const fn new(id: BlobId, metadata: Metadata) -> Self {
        Self { id, metadata }
    }
}

impl blob::info::DispersedBlobInfo for BlobInfo {
    type BlobId = BlobId;

    fn blob_id(&self) -> Self::BlobId {
        self.id
    }

    fn size(&self) -> usize {
        size_of_val(&self.id) + self.metadata.size()
    }
}

impl Hash for BlobInfo {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(<Self as blob::info::DispersedBlobInfo>::blob_id(self).as_ref());
    }
}

impl blob::metadata::Metadata for BlobInfo {
    type AppId = [u8; 32];
    type Index = Index;

    fn metadata(&self) -> (Self::AppId, Self::Index) {
        (self.metadata.app_id, self.metadata.index)
    }
}

#[derive(Copy, Clone, Default, Debug, PartialEq, PartialOrd, Ord, Eq, Serialize, Deserialize)]
pub struct Index([u8; 8]);

impl Index {
    #[must_use]
    pub const fn to_u64(self) -> u64 {
        u64::from_le_bytes(self.0)
    }
}

#[derive(Default, Debug, Copy, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct Metadata {
    app_id: [u8; 32],
    index: Index,
}

impl Metadata {
    #[must_use]
    pub const fn new(app_id: [u8; 32], index: Index) -> Self {
        Self { app_id, index }
    }

    #[expect(
        clippy::missing_const_for_fn,
        reason = "TODO: `std::mem::size_of_val` is not yet stable as a const fn"
    )]
    #[must_use]
    pub fn size(&self) -> usize {
        size_of_val(&self.app_id) + size_of_val(&self.index)
    }
}

impl blob::metadata::Metadata for Metadata {
    type AppId = [u8; 32];
    type Index = Index;

    fn metadata(&self) -> (Self::AppId, Self::Index) {
        (self.app_id, self.index)
    }
}

impl From<u64> for Index {
    fn from(value: u64) -> Self {
        Self(value.to_le_bytes())
    }
}

impl Next for Index {
    fn next(self) -> Self {
        let num = u64::from_le_bytes(self.0);
        let incremented_num = num.wrapping_add(1);
        Self(incremented_num.to_le_bytes())
    }
}

impl AsRef<[u8]> for Index {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use lb_core::da::{DaEncoder as _, blob::Share as _};

    use crate::{
        common::share::DaShare,
        encoder::{
            EncodedData,
            test::{ENCODER, rand_data},
        },
        kzg_keys::VERIFICATION_KEY,
        verifier::DaVerifier,
    };

    fn attest_encoded_data(encoded_data: &EncodedData, verifiers: &[DaVerifier]) -> Vec<bool> {
        let mut attestations = Vec::new();
        let domain_size = encoded_data.extended_data.0[0].len();
        for (i, column) in encoded_data.extended_data.columns().enumerate() {
            let verifier = &verifiers[i];
            let da_share = DaShare {
                column,
                share_idx: i
                    .try_into()
                    .expect("Column index shouldn't overflow the target type"),
                combined_column_proof: encoded_data.combined_column_proofs[i],
                rows_commitments: encoded_data.row_commitments.clone(),
            };
            let (light_share, commitments) = da_share.into_share_and_commitments();
            attestations.push(verifier.verify(&light_share, &commitments, domain_size));
        }
        attestations
    }

    #[test]
    fn test_encoded_data_verification() {
        let encoder = &ENCODER;
        let data = rand_data(8);
        let verifiers: Vec<DaVerifier> =
            std::iter::repeat_with(|| DaVerifier::new(VERIFICATION_KEY.clone()))
                .take(16)
                .collect();

        let encoded_data = encoder.encode(&data).unwrap();

        let attestations = attest_encoded_data(&encoded_data, &verifiers);

        assert!(!attestations.contains(&false));
    }
}
