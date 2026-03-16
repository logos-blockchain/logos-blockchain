use std::time::Duration;

use lb_utils::bounded_duration::{MinimalBoundedDuration, SECOND};
use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::types::{
    OffenseKind,
    OffenseKind::{
        BlackListed, DoSConnectionFlood, GossipsubScoreDrop, InvalidBlob, InvalidSig, Other,
        ProtocolViolation, SpamMsg, TooManyDials,
    },
};

/// Configuration for the banning subsystem
#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BanningConfig {
    /// List of white-listed of peer IDs as base58 strings (never ban)
    #[serde(default)]
    pub whitelist: Vec<PeerId>,
    /// List of black-listed of peer IDs as base58 strings (always ban)
    #[serde(default)]
    pub blacklist: Vec<PeerId>,
    /// Named offenses with ban durations in seconds
    #[serde(default)]
    pub offenses: OffenseMap,
    /// The ban expiration check interval in seconds
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    #[serde(default = "default_ban_expiry_check_interval")]
    pub ban_expiry_check_interval: Duration,
}

impl BanningConfig {
    /// Get the ban duration in seconds for a given offense kind
    #[must_use]
    pub const fn ban_duration(&self, kind: OffenseKind) -> Duration {
        match kind {
            SpamMsg => self.offenses.spam_msg,
            InvalidSig => self.offenses.invalid_sig,
            ProtocolViolation => self.offenses.protocol_violation,
            TooManyDials => self.offenses.too_many_dials,
            DoSConnectionFlood => self.offenses.dos_connection_flood,
            InvalidBlob => self.offenses.invalid_blob,
            GossipsubScoreDrop => self.offenses.gossipsub_score_drop,
            Other => self.offenses.other,
            BlackListed => Duration::from_secs(10 * 365 * 24 * 60 * 60), // Ten years
        }
    }
}

impl Default for BanningConfig {
    fn default() -> Self {
        Self {
            whitelist: vec![],
            blacklist: vec![],
            offenses: OffenseMap::default(),
            ban_expiry_check_interval: default_ban_expiry_check_interval(),
        }
    }
}

const fn default_ban_expiry_check_interval() -> Duration {
    Duration::from_secs(5)
}

/// Map of offense types to ban durations in seconds
#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OffenseMap {
    /// Ban duration in seconds for spam messages
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub spam_msg: Duration,
    /// Ban duration in seconds for invalid signatures
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub invalid_sig: Duration,
    /// Ban duration in seconds for protocol violations
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub protocol_violation: Duration,
    /// Ban duration in seconds for too many dials
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub too_many_dials: Duration,
    /// Ban duration in seconds for `DoS` connection floods
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub dos_connection_flood: Duration,
    /// Ban duration in seconds for invalid blobs
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub invalid_blob: Duration,
    /// Ban duration in seconds for gossipsub score drops
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub gossipsub_score_drop: Duration,
    /// Ban duration in seconds for other offenses
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub other: Duration,
}

impl Default for OffenseMap {
    fn default() -> Self {
        Self {
            spam_msg: Duration::from_secs(60 * 2),           // Two minutes
            invalid_sig: Duration::from_secs(60 * 60 * 2),   // Two hours
            protocol_violation: Duration::from_secs(60 * 2), // Two minutes
            too_many_dials: Duration::from_secs(60 * 5),     // Five minutes
            dos_connection_flood: Duration::from_secs(60 * 5), // Five minutes
            invalid_blob: Duration::from_secs(60 * 60 * 2),  // Two hours
            gossipsub_score_drop: Duration::from_secs(60),   // One minute
            other: Duration::from_secs(60),                  // One minute
        }
    }
}
