use core::{
    cmp::Ordering,
    fmt::{self, Display, Formatter},
};
use std::num::NonZero;

use lb_binary_codec::{
    bincode::{self, BoundedSerializeOp},
    canonical::{BinaryCodec, BinaryDecode, BinaryEncode, DecodeError},
};

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

#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EpochConfig {
    // The stake distribution is always taken at the beginning of the previous epoch.
    // This parameters controls how many slots to wait for it to be stabilized
    // The value is computed as epoch_stake_distribution_stabilization * int(floor(k / f))
    pub epoch_stake_distribution_stabilization: NonZero<u8>,
    // This parameter controls how many slots we wait after the stake distribution
    // snapshot has stabilized to take the nonce snapshot.
    pub epoch_period_nonce_buffer: NonZero<u8>,
    // This parameter controls how many slots we wait for the nonce snapshot to be considered
    // stabilized
    pub epoch_period_nonce_stabilization: NonZero<u8>,
}

impl EpochConfig {
    #[must_use]
    pub const fn epoch_length(&self, base_period_length: NonZero<u64>) -> u64 {
        epoch_length(
            self.epoch_stake_distribution_stabilization,
            self.epoch_period_nonce_buffer,
            self.epoch_period_nonce_stabilization,
            base_period_length,
        )
    }

    #[must_use]
    pub fn epoch(&self, slot: Slot, base_period_length: NonZero<u64>) -> Epoch {
        (u64::from(slot) / self.epoch_length(base_period_length))
            .try_into()
            .expect("Epoch should build from a correct configuration")
    }

    #[must_use]
    pub fn starting_slot(&self, epoch: &Epoch, base_period_length: NonZero<u64>) -> Slot {
        Slot::from(u64::from(u32::from(*epoch)) * self.epoch_length(base_period_length))
    }

    #[must_use]
    pub fn last_slot(&self, epoch: Epoch, base_period_length: NonZero<u64>) -> Slot {
        Slot::from(u64::from(epoch.into_inner() + 1) * self.epoch_length(base_period_length) - 1)
    }
}

#[must_use]
pub const fn epoch_length(
    epoch_stake_distribution_stabilization: NonZero<u8>,
    epoch_period_nonce_buffer: NonZero<u8>,
    epoch_period_nonce_stabilization: NonZero<u8>,
    base_period_length: NonZero<u64>,
) -> u64 {
    ((epoch_stake_distribution_stabilization.get() as u64)
        .saturating_add(epoch_period_nonce_buffer.get() as u64)
        .saturating_add(epoch_period_nonce_stabilization.get() as u64))
    .saturating_mul(base_period_length.get())
}
