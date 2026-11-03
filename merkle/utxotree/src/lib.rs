use std::marker::PhantomData;

use ark_ff::AdditiveGroup;
use lb_dynamic_merkle::{MerkleHasher, empty_subtree_root};
use lb_poseidon2::{Digest, Fr};

/// [`MerkleHasher`] bridge compressing field-element (`Fr`) nodes with a
/// [`Digest`] hasher.
pub struct UtxoMerkleHasher<Hash>(PhantomData<Hash>);

impl<Hash> MerkleHasher for UtxoMerkleHasher<Hash>
where
    Hash: Digest,
{
    type Hash = Fr;

    const EMPTY_VALUE: Fr = <Fr as AdditiveGroup>::ZERO;

    fn compress(left: &Fr, right: &Fr) -> Fr {
        <Hash as Digest>::compress(&[*left, *right])
    }

    empty_subtree_root!(Fr);
}
