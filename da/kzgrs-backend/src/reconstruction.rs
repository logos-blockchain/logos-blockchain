use lb_kzgrs::BYTES_PER_FIELD_ELEMENT;

use crate::common::{Chunk, share::DaShare};

/// Reconstruct original data from a set of `DaShare`
/// Warning! This does not interpolate so it should not be used on blobs which
/// doesn't represent the original set of data.
#[must_use]
pub fn reconstruct_without_missing_data(shares: &[DaShare]) -> Vec<u8> {
    // pick positions from columns
    let mut data: Vec<((usize, usize), Vec<u8>)> = shares
        .iter()
        .flat_map(|share| {
            share
                .column
                .iter()
                .map(Chunk::as_bytes)
                .enumerate()
                .map(|(row, data)| ((row, share.share_idx as usize), data))
        })
        .collect();
    data.sort_unstable_by_key(|(k, _)| *k);
    data.into_iter()
        .flat_map(|(_, mut data)| {
            assert_eq!(data.len(), BYTES_PER_FIELD_ELEMENT);
            // pop last byte as we fill it empty
            data.pop();
            data.into_iter()
        })
        .collect()
}

#[cfg(test)]
mod test {
    use lb_core::da::DaEncoder as _;
    use lb_kzgrs::Proof;

    use crate::{
        common::{ShareIndex, share::DaShare},
        encoder::{DaEncoder, DaEncoderParams, EncodedData, test::rand_data},
        reconstruction::reconstruct_without_missing_data,
    };

    #[test]
    fn test_reconstruct() {
        let data: Vec<u8> = rand_data(32);
        let params = DaEncoderParams::default_with(4);
        let encoder = DaEncoder::new(params);
        let encoded_data: EncodedData = encoder.encode(&data).unwrap();
        let shares: Vec<DaShare> = encoded_data
            .chunked_data
            .columns()
            .enumerate()
            .map(|(idx, column)| DaShare {
                column,
                share_idx: idx as ShareIndex,
                combined_column_proof: Proof::default(),
                rows_commitments: vec![],
            })
            .collect();
        assert_eq!(data, reconstruct_without_missing_data(&shares));
    }
}
