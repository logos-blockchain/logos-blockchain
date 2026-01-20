use lb_core::sdp::SessionNumber;
use libp2p::PeerId;

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
