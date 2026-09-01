use libp2p::{TransportError, swarm::DialError};

pub trait DialErrorExt {
    fn is_recoverable(&self) -> bool;
}

impl DialErrorExt for DialError {
    fn is_recoverable(&self) -> bool {
        match self {
            // The host answering at the declared address presented a different
            // identity than the expected one. Nothing we retry changes the key
            // the remote holds — the declaration itself has to change.
            Self::WrongPeerId { .. }
            // The address handed resolves back to this node.
            | Self::LocalPeerId { .. }
            // There is no address to dial for this peer.
            | Self::NoAddresses => false,

            // Per-address transport failures. Permanent only when every address
            // failed because we cannot speak its protocol at all; a timeout,
            // refusal, or unreachable host is worth another attempt.
            Self::Transport(errors) => {
                errors.is_empty()
                    || !errors
                        .iter()
                        .all(|(_, error)| matches!(error, TransportError::MultiaddrNotSupported(_)))
            }

            // Local, transient conditions.
            Self::Denied { .. }
            | Self::Aborted
            | Self::DialPeerConditionFalse(_) => true,
        }
    }
}
