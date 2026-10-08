//! The canonical encoding of the sections an era declares, which its digest is
//! over: how many there are, then each, by increasing identifier, as its
//! identifier, its version and its layout in that version.

use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};

use crate::{
    EraChanges, SectionParameters, blend::BlendParameters, blocks::BlocksParameters,
    cryptarchia::CryptarchiaParameters, time::TimeParameters,
};

impl BinaryEncode for EraChanges {
    fn encoded_length(&self) -> usize {
        let Self {
            blend,
            blocks,
            cryptarchia,
            time,
        } = self;

        0u8.encoded_length()
            + blend.as_ref().map_or(0, section_length)
            + blocks.as_ref().map_or(0, section_length)
            + cryptarchia.as_ref().map_or(0, section_length)
            + time.as_ref().map_or(0, section_length)
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            blend,
            blocks,
            cryptarchia,
            time,
        } = self;

        let declared = [
            blend.is_some(),
            blocks.is_some(),
            cryptarchia.is_some(),
            time.is_some(),
        ];
        u8::try_from(declared.into_iter().filter(|&declared| declared).count())
            .expect("an era has four sections")
            .encode_into(out);
        if let Some(blend) = blend {
            encode_section(blend, out);
        }
        if let Some(blocks) = blocks {
            encode_section(blocks, out);
        }
        if let Some(cryptarchia) = cryptarchia {
            encode_section(cryptarchia, out);
        }
        if let Some(time) = time {
            encode_section(time, out);
        }
    }
}

fn section_length<Parameters>(parameters: &Parameters) -> usize
where
    Parameters: SectionParameters,
{
    Parameters::SECTION.id().encoded_length() + parameters.encoded_length()
}

fn encode_section<Parameters>(parameters: &Parameters, out: &mut Vec<u8>)
where
    Parameters: SectionParameters,
{
    Parameters::SECTION.id().encode_into(out);
    parameters.encode_into(out);
}

/// Every section, as the genesis era declares them.
fn genesis_fixture() -> EraChanges {
    EraChanges {
        blend: Some(BlendParameters::V1(
            crate::blend::v1::codec::fixture_settings(),
        )),
        blocks: Some(BlocksParameters::V1),
        cryptarchia: Some(CryptarchiaParameters::V1(
            crate::cryptarchia::v1::codec::fixture_settings(),
        )),
        time: Some(TimeParameters::V1(
            crate::time::v1::codec::fixture_settings(),
        )),
    }
}

/// The encoding of [`genesis_fixture`]. A function, since `&str` constants
/// cannot be concatenated at compile time.
fn genesis_fixture_hex() -> String {
    [
        "04",
        "00 0100",
        crate::blend::v1::codec::SETTINGS_HEX,
        "01 0100",
        "02 0100",
        crate::cryptarchia::v1::codec::SETTINGS_HEX,
        "03 0100",
        crate::time::v1::codec::SETTINGS_HEX,
    ]
    .concat()
}

/// A single section, as an era after genesis that changes only it declares.
const fn time_change_fixture() -> EraChanges {
    EraChanges {
        blend: None,
        blocks: None,
        cryptarchia: None,
        time: Some(TimeParameters::V1(
            crate::time::v1::codec::fixture_settings(),
        )),
    }
}

codec_fixtures!(
    EraChanges,
    encode_only,
    genesis_fixture() => &genesis_fixture_hex(),
    time_change_fixture() => &format!("01 03 0100 {}", crate::time::v1::codec::SETTINGS_HEX)
);
