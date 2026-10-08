use std::borrow::Cow;

use lb_binary_codec::canonical::{
    CodecExamples, CodecFixture, CodecFixtures, codec_fixtures, decode_fixture_hex,
};

#[cfg(test)]
use crate::era::EraSchedules;
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

/// A chain of a single era, whose blocks are of version 1, from genesis: what
/// decodes the block and the proposal fixtures.
#[cfg(test)]
pub(super) fn single_era() -> EraSchedules {
    use lb_cryptarchia_engine::era::{BlockVersion, EraEntriesAfterGenesis, EraEntry, EraSchedule};

    EraSchedule::new(
        time::OffsetDateTime::UNIX_EPOCH,
        EraEntry {
            block_version: BlockVersion::V1,
            slot_duration: core::time::Duration::from_secs(1),
            epoch_length_in_slots: core::num::NonZero::new(100).expect("an epoch has slots"),
            parameters: (),
        },
        EraEntriesAfterGenesis::empty(),
    )
    .expect("a single era from genesis is a valid schedule")
}

// A proposal encodes as its version's proposal does.
codec_fixtures!(Proposal, context = single_era(), Self::V1(proposal()) => PROPOSAL_HEX);

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
