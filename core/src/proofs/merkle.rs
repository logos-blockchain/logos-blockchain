use lb_groth16::Fr;
use lb_utxotree::{MerkleNode, MerklePath, TREE_HEIGHT_EXCEPT_ROOT};
use thiserror::Error;

/// Converts a [`MerklePath`] to the witness format expected by the circuit.
pub fn merkle_path_to_witness<T: Copy>(
    path: &MerklePath<T>,
) -> (
    [T; TREE_HEIGHT_EXCEPT_ROOT],
    [bool; TREE_HEIGHT_EXCEPT_ROOT],
) {
    let items = core::array::from_fn(|index| *path[index].item());
    // PoL circuit expects the reverse order for selectors.
    let selectors = core::array::from_fn(|index| {
        matches!(
            path[TREE_HEIGHT_EXCEPT_ROOT - index - 1],
            MerkleNode::Left(_)
        )
    });
    (items, selectors)
}

/// Converts an [`lb_mmr::MerklePath`] to the witness format expected by
/// the circuit: siblings in bottom-top order, selectors reversed.
pub fn mmr_path_to_witness(
    path: &lb_mmr::MerklePath,
) -> Result<
    (
        [Fr; lb_poc::VOUCHER_MERKLE_PATH_LEN],
        [bool; lb_poc::VOUCHER_MERKLE_PATH_LEN],
    ),
    MerklePathWitnessError,
> {
    let actual = path.siblings().len();
    let items = path
        .siblings()
        .try_into()
        .map_err(|_| MerklePathWitnessError::InvalidLength {
            expected: lb_poc::VOUCHER_MERKLE_PATH_LEN,
            actual,
        })?;
    let capacity = 1u64 << (lb_poc::VOUCHER_MERKLE_PATH_LEN as u32);
    let leaf_index = u64::try_from(path.leaf_index()).unwrap_or(u64::MAX);
    if leaf_index >= capacity {
        return Err(MerklePathWitnessError::LeafIndexOutOfRange {
            leaf_index: path.leaf_index(),
            capacity,
        });
    }
    // Selectors: true if sibling is on the left (i.e. leaf is a right child).
    // Circuit expects reversed order.
    let selectors = core::array::from_fn(|index| {
        let height = lb_poc::VOUCHER_MERKLE_PATH_LEN - index;
        !lb_mmr::is_left_child(path.leaf_index(), height)
    });
    Ok((items, selectors))
}

/// An MMR path that cannot be represented by the fixed-height `PoC` circuit.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum MerklePathWitnessError {
    #[error("invalid PoC voucher path length: expected {expected}, got {actual}")]
    InvalidLength { expected: usize, actual: usize },
    #[error("PoC voucher leaf index {leaf_index} is outside tree capacity {capacity}")]
    LeafIndexOutOfRange { leaf_index: usize, capacity: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_merkle_path_to_witness() {
        let path: MerklePath<i32> = core::array::from_fn(|index| {
            if index % 2 == 0 {
                MerkleNode::Left(index as i32)
            } else {
                MerkleNode::Right(index as i32)
            }
        });
        let (items, selectors) = merkle_path_to_witness(&path);
        assert_eq!(items[0..4], [0, 1, 2, 3]);
        assert_eq!(selectors[0..4], [false, true, false, true]);
    }

    #[test]
    fn test_mmr_path_to_witness() {
        let path = lb_mmr::MerklePath::try_new(
            11,
            (1..=lb_poc::VOUCHER_MERKLE_PATH_LEN)
                .map(|index| Fr::from(index as u64))
                .collect(),
        )
        .unwrap();
        let (items, selectors) = mmr_path_to_witness(&path).unwrap();
        assert_eq!(
            items,
            core::array::from_fn(|index| Fr::from((index + 1) as u64))
        );
        // For leaf index 11 (=1101), the selector order starts at height 32.
        assert_eq!(selectors[28..], [true, false, true, true]);
    }

    #[test]
    fn test_mmr_path_to_witness_rejects_wrong_length() {
        let path = lb_mmr::MerklePath::try_new(
            0,
            vec![Fr::from(0u64); lb_poc::VOUCHER_MERKLE_PATH_LEN - 1],
        )
        .unwrap();
        let path: lb_mmr::MerklePath =
            bincode::deserialize(&bincode::serialize(&path).unwrap()).unwrap();
        assert_eq!(
            mmr_path_to_witness(&path),
            Err(MerklePathWitnessError::InvalidLength {
                expected: lb_poc::VOUCHER_MERKLE_PATH_LEN,
                actual: lb_poc::VOUCHER_MERKLE_PATH_LEN - 1,
            })
        );
    }

    #[test]
    fn test_deserialized_mmr_path_rejects_invalid_poc_shape() {
        use crate::{
            mantle::ops::leader_claim::VoucherSecret,
            proofs::leader_claim_proof::{LeaderClaimPrivate, LeaderClaimPublic},
        };

        let path = lb_mmr::MerklePath::try_new(
            0,
            vec![Fr::from(0u64); lb_poc::VOUCHER_MERKLE_PATH_LEN - 1],
        )
        .unwrap();
        let path: lb_mmr::MerklePath =
            bincode::deserialize(&bincode::serialize(&path).unwrap()).unwrap();
        let result = LeaderClaimPrivate::try_new(
            LeaderClaimPublic::new(Fr::from(1u64), Fr::from(2u64), Fr::from(3u64)),
            &path,
            VoucherSecret::from(Fr::from(4u64)),
        );

        assert!(matches!(
            result,
            Err(MerklePathWitnessError::InvalidLength {
                expected: lb_poc::VOUCHER_MERKLE_PATH_LEN,
                actual,
            }) if actual == lb_poc::VOUCHER_MERKLE_PATH_LEN - 1
        ));
    }

    #[test]
    fn test_mmr_path_to_witness_validates_leaf_index() {
        let siblings: Vec<Fr> =
            std::iter::repeat_n(Fr::from(0u64), lb_poc::VOUCHER_MERKLE_PATH_LEN).collect();
        let highest_valid = lb_mmr::MerklePath::try_new(
            (1u64 << lb_poc::VOUCHER_MERKLE_PATH_LEN) as usize - 1,
            siblings.clone(),
        )
        .unwrap();
        assert!(mmr_path_to_witness(&highest_valid).is_ok());

        let out_of_range = lb_mmr::MerklePath::try_new(
            (1u64 << lb_poc::VOUCHER_MERKLE_PATH_LEN) as usize,
            siblings,
        )
        .unwrap();
        assert_eq!(
            mmr_path_to_witness(&out_of_range),
            Err(MerklePathWitnessError::LeafIndexOutOfRange {
                leaf_index: (1u64 << lb_poc::VOUCHER_MERKLE_PATH_LEN) as usize,
                capacity: 1u64 << lb_poc::VOUCHER_MERKLE_PATH_LEN,
            })
        );
    }
}
