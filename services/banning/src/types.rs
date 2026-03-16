use std::time::SystemTime;

use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, oneshot};

/// Subsystem reporting the offense
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Subsystem {
    Libp2p,
    DA,
    Blend,
    Other(&'static str),
}

/// Kind of offense committed by a peer
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum OffenseKind {
    SpamMsg,
    InvalidSig,
    ProtocolViolation,
    TooManyDials,
    DoSConnectionFlood,
    InvalidBlob,
    GossipsubScoreDrop,
    Other,
    BlackListed,
}

/// Current ban status of a peer - if banned it includes the ban expiration time
/// and offense kind
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum BanStatus {
    #[default]
    NotBanned,
    Banned {
        banned_until: SystemTime,
        offense: OffenseKind,
        context: Option<String>,
    },
}

/// Report a ban-able offense/violation by a peer
#[derive(Clone, Debug)]
pub struct Violation {
    /// The peer who committed the offense
    pub peer_id: PeerId,
    /// The subsystem reporting the offense
    pub subsystem: Subsystem,
    /// The kind of offense committed
    pub offense_kind: OffenseKind,
    /// Optional context for the offense
    pub context: Option<String>,
}

/// Requests that can be made to the banning subsystem
#[derive(Debug)]
pub enum BanningRequest {
    BanPeer {
        violation: Violation,
        reply: Option<oneshot::Sender<BanStatus>>,
    },
    GetBanState {
        peer_id: PeerId,
        reply: oneshot::Sender<BanStatus>,
    },
    UnbanPeer {
        peer_id: PeerId,
        reply: oneshot::Sender<bool>,
    },
    ListActiveBans {
        reply: oneshot::Sender<Vec<(PeerId, BanStatus)>>,
    },
    /// Returns a fresh broadcast receiver so callers can stream events
    Subscribe {
        reply: oneshot::Sender<broadcast::Receiver<BanningEvent>>,
    },
}

/// Events emitted by the banning subsystem
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum BanningEvent {
    Banned {
        peer_id: PeerId,
        banned_until: SystemTime,
        offense: OffenseKind,
        context: Option<String>,
    },
    Unbanned {
        peer_id: PeerId,
    },
}
