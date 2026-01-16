use libp2p::PeerId;
use logos_blockchain_core::sdp::SessionNumber;

#[derive(Debug, Clone)]
pub enum Opinion {
    Positive {
        peer_id: PeerId,
        session_id: SessionNumber,
    },
    Negative {
        peer_id: PeerId,
        session_id: SessionNumber,
    },
    Blacklist {
        peer_id: PeerId,
        session_id: SessionNumber,
    },
}

#[derive(Debug, Clone)]
pub struct OpinionEvent {
    pub opinions: Vec<Opinion>,
}
