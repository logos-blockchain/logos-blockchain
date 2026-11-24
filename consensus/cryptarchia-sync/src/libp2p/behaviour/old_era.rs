use std::task::{Context, Poll};

use super::{OngoingRequests, SyncProtocolState, ToSwarmEvent};

/// The chain sync over the protocol of the era before the one in force, until
/// the end of the era's transition period: the requests and responses under
/// way on that protocol, which run to completion. Streams opened over the
/// protocol are refused, and no request is started on it.
pub(super) struct OldEra {
    ongoing_requests: OngoingRequests,
}

impl OldEra {
    /// What is left of the chain sync over a protocol once its era is over.
    /// The handle to the protocol's incoming streams is dropped, which refuses
    /// every stream opened over it from now on.
    pub(super) fn new(sync_protocol_state: SyncProtocolState) -> Self {
        Self {
            ongoing_requests: sync_protocol_state.ongoing_requests,
        }
    }

    pub(super) fn poll(&mut self, cx: &mut Context<'_>) -> Poll<ToSwarmEvent> {
        self.ongoing_requests.poll(cx)
    }
}
