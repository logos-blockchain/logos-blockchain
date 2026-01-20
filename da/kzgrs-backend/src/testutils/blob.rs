use lb_core::da::DaEncoder as _;

use crate::{common::share::DaShare, testutils::encoder::get_encoder};

#[must_use]
pub fn get_default_da_blob_data() -> Vec<u8> {
    vec![
        49u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8,
        0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8,
    ]
}

pub fn get_da_share(data: Option<Vec<u8>>) -> DaShare {
    let encoder = get_encoder();

    let data = data.unwrap_or_else(get_default_da_blob_data);
    let encoded_data = encoder.encode(&data).unwrap();
    let columns: Vec<_> = encoded_data.extended_data.columns().collect();

    let index = 0;
    DaShare {
        column: columns[index].clone(),
        share_idx: index
            .try_into()
            .expect("Column index shouldn't overflow the target type"),
        combined_column_proof: encoded_data.combined_column_proofs[index],
        rows_commitments: encoded_data.row_commitments.clone(),
    }
}
