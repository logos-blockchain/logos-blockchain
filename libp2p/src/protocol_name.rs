use core::ops::{Deref, DerefMut};

use libp2p::StreamProtocol as Libp2pStreamProtocol;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Struct wrapping around a `StreamProtocol` to make it serializable and
/// catch any invalid protocol names already at config reading instead of later
/// on.
pub struct StreamProtocol(
    #[serde(
        serialize_with = "serialize_stream_protocol",
        deserialize_with = "deserialize_stream_protocol"
    )]
    Libp2pStreamProtocol,
);

fn serialize_stream_protocol<S>(
    protocol_name: &Libp2pStreamProtocol,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    protocol_name.as_ref().serialize(serializer)
}

fn deserialize_stream_protocol<'de, D>(deserializer: D) -> Result<Libp2pStreamProtocol, D::Error>
where
    D: Deserializer<'de>,
{
    let protocol_name = String::deserialize(deserializer)?;
    Libp2pStreamProtocol::try_from_owned(protocol_name).map_err(Error::custom)
}

impl StreamProtocol {
    #[must_use]
    pub const fn new(protocol_name: &'static str) -> Self {
        Self(Libp2pStreamProtocol::new(protocol_name))
    }

    #[must_use]
    pub fn into_inner(self) -> Libp2pStreamProtocol {
        self.0
    }
}

impl Deref for StreamProtocol {
    type Target = Libp2pStreamProtocol;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for StreamProtocol {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<StreamProtocol> for Libp2pStreamProtocol {
    fn from(value: StreamProtocol) -> Self {
        value.0
    }
}

impl From<Libp2pStreamProtocol> for StreamProtocol {
    fn from(value: Libp2pStreamProtocol) -> Self {
        Self(value)
    }
}
