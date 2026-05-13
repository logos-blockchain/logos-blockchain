use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::{
    codec::{DeserializeOp as _, SerializeOp as _},
    crypto::Hash,
    mantle::{TxHash, Value, ops::channel::ChannelId},
};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Events(Vec<Event>);

impl Events {
    #[must_use]
    pub const fn new() -> Self {
        Self(Vec::new())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub tx_hash: Option<TxHash>,
    pub op_id: Option<Hash>,
    pub payload: Payload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Payload {
    Deposit {
        channel_id: ChannelId,
        amount: Value,
        metadata: Vec<u8>,
    },
}

impl TryFrom<Bytes> for Events {
    type Error = crate::codec::Error;

    fn try_from(bytes: Bytes) -> Result<Self, Self::Error> {
        Self::from_bytes(&bytes)
    }
}

impl TryFrom<Events> for Bytes {
    type Error = crate::codec::Error;

    fn try_from(events: Events) -> Result<Self, Self::Error> {
        events.to_bytes()
    }
}
