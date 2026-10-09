//! The time of a chain: its slots and epochs, and the eras that lay them out
//! in wall-clock time.

pub mod era;
pub use era::Era;
mod fixtures;

use core::{
    cmp::Ordering,
    fmt::{self, Display, Formatter},
};
use std::time::Duration;

use lb_binary_codec::{
    bincode::{self, BoundedSerializeOp},
    canonical::{BinaryCodec, BinaryDecode, BinaryEncode, DecodeError},
};
use lb_utils::bounded_duration::{MinimalBoundedDuration, SECOND};
use time::OffsetDateTime;

#[derive(
    Clone,
    Debug,
    Default,
    Eq,
    PartialEq,
    Copy,
    Hash,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    BinaryCodec,
)]
pub struct Slot(u64);

impl BoundedSerializeOp for Slot {
    type Bytes = [u8; bincode::BINCODE_U64_SIZE];
}

#[derive(
    Clone, Debug, Eq, PartialEq, Copy, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct Epoch(u32);

impl Epoch {
    #[must_use]
    pub const fn new(inner: u32) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> u32 {
        self.0
    }

    /// Strict epoch addition, panicking if overflow occurred.
    ///
    /// # Panics
    /// This function will always panic on overflow, regardless of whether
    /// overflow checks are enabled.
    #[must_use]
    pub const fn strict_add(self, rhs: Self) -> Self {
        Self(self.0.strict_add(rhs.0))
    }

    /// Strict epoch subtraction, panicking if overflow occurred.
    ///
    /// # Panics
    /// This function will always panic on overflow, regardless of whether
    /// overflow checks are enabled.
    #[must_use]
    pub const fn strict_sub(self, rhs: Self) -> Self {
        Self(self.0.strict_sub(rhs.0))
    }

    #[must_use]
    pub const fn genesis() -> Self {
        Self(0)
    }
}

impl Display for Epoch {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Epoch({})", self.0)
    }
}

impl PartialEq<u32> for Epoch {
    fn eq(&self, other: &u32) -> bool {
        self.0 == *other
    }
}

impl PartialEq<Epoch> for u32 {
    fn eq(&self, other: &Epoch) -> bool {
        *self == other.0
    }
}

impl PartialOrd<u32> for Epoch {
    fn partial_cmp(&self, other: &u32) -> Option<Ordering> {
        self.0.partial_cmp(other)
    }
}

impl PartialOrd<Epoch> for u32 {
    fn partial_cmp(&self, other: &Epoch) -> Option<Ordering> {
        self.partial_cmp(&other.0)
    }
}

impl AsRef<u32> for Epoch {
    fn as_ref(&self) -> &u32 {
        &self.0
    }
}

impl BinaryEncode for Epoch {
    fn encoded_length(&self) -> usize {
        self.as_ref().encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.as_ref().encode_into(out);
    }
}

impl BinaryDecode for Epoch {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, inner) = u32::decode(input, &())?;
        Ok((rest, Self::new(inner)))
    }
}

impl Slot {
    /// The fixed-size canonical representation of a slot.
    pub const CANONICAL_ENCODED_SIZE: usize = 8;

    #[must_use]
    pub const fn new(inner: u64) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn to_le_bytes(&self) -> [u8; 8] {
        self.0.to_le_bytes()
    }

    #[must_use]
    pub const fn to_be_bytes(&self) -> [u8; 8] {
        self.0.to_be_bytes()
    }

    #[must_use]
    pub const fn genesis() -> Self {
        Self(0)
    }

    #[must_use]
    pub fn from_offset_and_config(
        offset_date_time: OffsetDateTime,
        slot_config: SlotConfig,
    ) -> Self {
        // TODO: leap seconds / weird time stuff
        let since_start = offset_date_time - slot_config.genesis_time;
        if since_start.is_negative() {
            // current slot is behind the start time, so return default 0
            Self::genesis()
        } else {
            // since_start is already checked never negative in this case
            // division panics if `slot_duration` is less than a second.
            Self::from(
                (since_start.whole_seconds() as u64)
                    .checked_div(slot_config.slot_duration.as_secs())
                    .expect("slots tick should be at least a second"),
            )
        }
    }

    /// Strict slot addition, panicking if overflow occurred.
    ///
    /// # Panics
    /// This function will always panic on overflow, regardless of whether
    /// overflow checks are enabled.
    #[must_use]
    pub const fn strict_add(self, rhs: Self) -> Self {
        Self(self.0.strict_add(rhs.0))
    }

    #[must_use]
    pub const fn saturating_sub(self, rhs: Self) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }

    #[must_use]
    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        self.0.checked_sub(rhs.0).map(Self)
    }
}

impl From<u32> for Epoch {
    fn from(epoch: u32) -> Self {
        Self(epoch)
    }
}

impl From<Epoch> for u32 {
    fn from(epoch: Epoch) -> Self {
        epoch.0
    }
}

impl TryFrom<u64> for Epoch {
    type Error = <u64 as TryInto<u32>>::Error;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        value.try_into().map(Self)
    }
}

#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for Slot {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        u64::schema()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for Slot {}

impl From<u64> for Slot {
    fn from(slot: u64) -> Self {
        Self(slot)
    }
}

impl From<Slot> for u64 {
    fn from(slot: Slot) -> Self {
        slot.0
    }
}

#[serde_with::serde_as]
#[derive(Copy, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SlotConfig {
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub slot_duration: Duration,
    /// Start of the first epoch
    pub genesis_time: OffsetDateTime,
}
