use crate::utils::merkle::{MerkleNode, MerklePath};

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
}
