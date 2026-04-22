use lb_groth16::Fr;
use lb_utxotree::{MerkleNode, MerklePath};

/// Converts a [`MerklePath`] to the witness format expected by the circuit.
pub fn merkle_path_to_witness<T: Copy>(path: &MerklePath<T>) -> (Vec<T>, Vec<bool>) {
    path.iter()
        // PoL circuit expects the reverse order for selectors
        .zip(path.iter().rev())
        .map(|(node, rev_node)| {
            (
                *node.item(),
                // 1 if sibling is on the left
                matches!(rev_node, MerkleNode::Left(_)),
            )
        })
        .unzip()
}

/// Converts an [`lb_mmr::MerklePath`] to the witness format expected by
/// the circuit: siblings in bottom-top order, selectors reversed.
pub fn mmr_path_to_witness(path: &lb_mmr::MerklePath) -> (Vec<Fr>, Vec<bool>) {
    let items = path.siblings.clone();
    // Selectors: true if sibling is on the left (i.e. leaf is a right child).
    // Circuit expects reversed order.
    let selectors = (1..=items.len())
        .rev()
        .map(|height| !lb_mmr::is_left_child(path.leaf_index, height))
        .collect();
    (items, selectors)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_merkle_path_to_witness() {
        let path: Vec<MerkleNode<i32>> = vec![
            MerkleNode::Left(1),
            MerkleNode::Right(2),
            MerkleNode::Left(3),
            MerkleNode::Right(4),
        ];
        let (items, selectors) = merkle_path_to_witness(&path);
        // Items should be in forward order.
        assert_eq!(items, vec![1, 2, 3, 4]);
        // Selectors should be in reverse order.
        assert_eq!(selectors, vec![false, true, false, true]);
    }

    #[test]
    fn test_merkle_path_to_witness_empty() {
        let path: Vec<MerkleNode<i32>> = vec![];
        let (items, selectors) = merkle_path_to_witness(&path);
        assert!(items.is_empty());
        assert!(selectors.is_empty());
    }

    #[test]
    fn test_mmr_path_to_witness() {
        let path = lb_mmr::MerklePath {
            leaf_index: 11,
            siblings: vec![
                Fr::from(1u64),
                Fr::from(2u64),
                Fr::from(3u64),
                Fr::from(4u64),
            ],
        };
        let (items, selectors) = mmr_path_to_witness(&path);
        assert_eq!(
            items,
            vec![
                Fr::from(1u64),
                Fr::from(2u64),
                Fr::from(3u64),
                Fr::from(4u64)
            ]
        );
        // For leaf index 11 (=1101),
        // sibling positions are [L, R, L, L] (reverse order).
        assert_eq!(selectors, vec![true, false, true, true]);
    }

    #[test]
    fn test_mmr_path_to_witness_empty() {
        let path = lb_mmr::MerklePath {
            leaf_index: 0,
            siblings: vec![],
        };
        let (items, selectors) = mmr_path_to_witness(&path);
        assert!(items.is_empty());
        assert!(selectors.is_empty());
    }
}
