use serde::{Deserialize, Serialize};

use super::{ChannelId, Ed25519PublicKey};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct SetKeysOp {
    pub channel: ChannelId,
    pub keys: Vec<Ed25519PublicKey>,
}
