use std::borrow::Cow;

use lb_binary_codec::canonical::{
    CodecExamples, CodecFixture, CodecFixtures, codec_fixtures, decode_fixture_hex,
};

use crate::{
    block::{
        Block, BlockTransactionReferences, Proposal, References,
        v1::fixtures::{BLOCK_HEX, PROPOSAL_HEX, block, proposal},
    },
    mantle::{
        TxHash,
        traits::{Hashable, StorageSize},
    },
};

/// The three real references every reference fixture below is built from.
pub(super) fn three_references() -> BlockTransactionReferences {
    [
        TxHash([0x01u8; 32]).prefix(),
        TxHash([0x02u8; 32]).prefix(),
        TxHash([0x03u8; 32]).prefix(),
    ]
    .into()
}

codec_fixtures!(
    References,
    Self { mempool_transactions: three_references() } => "0300010101010101010101010101010101010202020202020202020202020202020203030303030303030303030303030303"
);

// A proposal encodes as its version's proposal does.
codec_fixtures!(Proposal, encode_only, Self::V1(proposal()) => PROPOSAL_HEX);

impl<Tx> lb_binary_codec::canonical::sealed::Sealed for Block<Tx> {}

// A block encodes as its version's block does.
impl<Tx> CodecExamples for Block<Tx>
where
    Tx: Hashable<Hash = TxHash> + StorageSize,
{
    fn fixtures() -> CodecFixtures<Self> {
        [CodecFixture {
            value: Self::V1(block()),
            bytes: Cow::Owned(decode_fixture_hex(BLOCK_HEX)),
        }]
        .into()
    }
}
