use std::task::{Context, Poll};

use futures::{
    AsyncWriteExt as _, FutureExt as _, StreamExt as _, future::BoxFuture, stream::FuturesUnordered,
};
use libp2p::{
    Multiaddr, PeerId, Stream,
    core::{Endpoint, transport::PortUse},
    swarm::{
        ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
        THandlerOutEvent, ToSwarm,
    },
};
use libp2p_stream::IncomingStreams;
use log::debug;
use nomos_core::mantle::{Op, SignedMantleTx, ops::channel::blob::BlobOp};
use nomos_da_messages::{
    common::Share,
    dispersal,
    packing::{pack_to_writer, unpack_from_reader},
};
use subnetworks_assignations::MembershipHandler;
use thiserror::Error;

use crate::{SubnetworkId, protocol::DISPERSAL_PROTOCOL};

#[derive(Debug, Error)]
pub enum DispersalError {
    #[error("Stream disconnected: {error}")]
    Io {
        peer_id: PeerId,
        error: std::io::Error,
    },
}

impl DispersalError {
    #[must_use]
    pub const fn peer_id(&self) -> Option<&PeerId> {
        match self {
            Self::Io { peer_id, .. } => Some(peer_id),
        }
    }
}

impl Clone for DispersalError {
    fn clone(&self) -> Self {
        match self {
            Self::Io { peer_id, error } => Self::Io {
                peer_id: *peer_id,
                error: std::io::Error::new(error.kind(), error.to_string()),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub enum DispersalEvent {
    /// Received a network message.
    IncomingShare(Box<Share>),
    /// Number of currently assigned subnetworks to the node and TX for a blob.
    IncomingTx((u16, Box<SignedMantleTx>)),
    /// Something went wrong receiving the blob
    DispersalError { error: DispersalError },
}

impl DispersalEvent {
    #[must_use]
    pub fn share_size(&self) -> Option<usize> {
        match self {
            Self::IncomingShare(share) => Some(share.data.column_len()),
            Self::IncomingTx { .. } | Self::DispersalError { .. } => None,
        }
    }
}

impl From<Share> for DispersalEvent {
    fn from(share: Share) -> Self {
        Self::IncomingShare(Box::new(share))
    }
}

impl From<(u16, SignedMantleTx)> for DispersalEvent {
    fn from(tx: (u16, SignedMantleTx)) -> Self {
        Self::IncomingTx((tx.0, Box::new(tx.1)))
    }
}

type DispersalTask =
    BoxFuture<'static, Result<(PeerId, dispersal::DispersalRequest, Stream), DispersalError>>;

pub struct DispersalValidatorBehaviour<Membership> {
    local_peer_id: PeerId,
    stream_behaviour: libp2p_stream::Behaviour,
    incoming_streams: IncomingStreams,
    tasks: FuturesUnordered<DispersalTask>,
    membership: Membership,
}

impl<Membership: MembershipHandler> DispersalValidatorBehaviour<Membership> {
    pub fn new(local_peer_id: PeerId, membership: Membership) -> Self {
        let stream_behaviour = libp2p_stream::Behaviour::new();
        let mut stream_control = stream_behaviour.new_control();
        let incoming_streams = stream_control
            .accept(DISPERSAL_PROTOCOL)
            .expect("Just a single accept to protocol is valid");
        let tasks = FuturesUnordered::new();
        Self {
            local_peer_id,
            stream_behaviour,
            incoming_streams,
            tasks,
            membership,
        }
    }

    fn process_dispersal_request(
        request: &dispersal::DispersalRequest,
    ) -> Option<dispersal::DispersalResponse> {
        match request {
            dispersal::DispersalRequest::Share(share_request) => {
                let blob_id = share_request.share.blob_id;
                Some(dispersal::DispersalResponse::BlobId(blob_id))
            }
            dispersal::DispersalRequest::Tx(signed_mantle_tx) => {
                if let Some(Op::ChannelBlob(BlobOp { blob, .. })) =
                    signed_mantle_tx.mantle_tx.ops.first()
                {
                    Some(dispersal::DispersalResponse::Tx(*blob))
                } else {
                    // Validator does not acknowledge malformed Tx at network level, but validation
                    // happens at the service level.
                    // If transaction is invalid in term of ledger, responsible services might
                    // terminate the connection to this peer.
                    None
                }
            }
        }
    }

    /// Stream handling messages task.
    /// This task handles a single message receive. Then it writes up the
    /// acknowledgment into the same stream as response and finish.
    async fn handle_new_stream(
        peer_id: PeerId,
        mut stream: Stream,
    ) -> Result<(PeerId, dispersal::DispersalRequest, Stream), DispersalError> {
        let request: dispersal::DispersalRequest = unpack_from_reader(&mut stream)
            .await
            .map_err(|error| DispersalError::Io { peer_id, error })?;

        if let Some(response) = Self::process_dispersal_request(&request) {
            pack_to_writer(&response, &mut stream)
                .await
                .map_err(|error| DispersalError::Io { peer_id, error })?;

            stream
                .flush()
                .await
                .map_err(|error| DispersalError::Io { peer_id, error })?;
        }
        Ok((peer_id, request, stream))
    }
}

impl<M: MembershipHandler<Id = PeerId, NetworkId = SubnetworkId> + 'static> NetworkBehaviour
    for DispersalValidatorBehaviour<M>
{
    type ConnectionHandler = <libp2p_stream::Behaviour as NetworkBehaviour>::ConnectionHandler;
    type ToSwarm = DispersalEvent;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.stream_behaviour.handle_established_inbound_connection(
            connection_id,
            peer,
            local_addr,
            remote_addr,
        )
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role_override: Endpoint,
        port_use: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        // No membership filtering - validators accept dispersal from any peer,
        // including non-members. This allows non-member executors to disperse
        // data to the network.
        self.stream_behaviour
            .handle_established_outbound_connection(
                connection_id,
                peer,
                addr,
                role_override,
                port_use,
            )
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        self.stream_behaviour.on_swarm_event(event);
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        self.stream_behaviour
            .on_connection_handler_event(peer_id, connection_id, event);
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        let Self {
            local_peer_id,
            incoming_streams,
            tasks,
            ..
        } = self;
        match tasks.poll_next_unpin(cx) {
            Poll::Ready(Some(Ok((peer_id, message, stream)))) => {
                tasks.push(Self::handle_new_stream(peer_id, stream).boxed());
                cx.waker().wake_by_ref();
                return match message {
                    dispersal::DispersalRequest::Share(share_request) => {
                        Poll::Ready(ToSwarm::GenerateEvent(DispersalEvent::IncomingShare(
                            Box::new(share_request.share),
                        )))
                    }
                    dispersal::DispersalRequest::Tx(signed_mantle_tx) => {
                        let session = if let Some(Op::ChannelBlob(BlobOp { session, .. })) =
                            signed_mantle_tx.mantle_tx.ops.first()
                        {
                            *session
                        } else {
                            tracing::debug!("Rejecting TX without BlobOp");
                            return Poll::Pending;
                        };

                        let current_session = self.membership.session_id();
                        if session != current_session {
                            tracing::debug!(
                                "Rejecting TX from session {session}, current session is {current_session}",
                            );
                            return Poll::Pending;
                        }

                        let assignations = self.membership.membership(local_peer_id).len();
                        Poll::Ready(ToSwarm::GenerateEvent(DispersalEvent::IncomingTx((
                            assignations as u16,
                            Box::new(signed_mantle_tx),
                        ))))
                    }
                };
            }
            Poll::Ready(Some(Err(error))) => {
                debug!("Error on dispersal stream {error:?}");
                cx.waker().wake_by_ref();
            }
            _ => {}
        }
        if let Poll::Ready(Some((peer_id, stream))) = incoming_streams.poll_next_unpin(cx) {
            tasks.push(Self::handle_new_stream(peer_id, stream).boxed());
            cx.waker().wake_by_ref();
        }

        Poll::Pending
    }
}
