mod connections;
pub mod errors;
mod historic;
pub mod opinions;
mod requests;
mod responses;
mod streams;

use std::collections::HashMap;

use errors::SamplingError;
use futures::{
    channel::oneshot::{Receiver, Sender},
    future::BoxFuture,
};
use kzgrs_backend::common::{
    ShareIndex,
    share::{DaLightShare, DaSharesCommitments},
};
use libp2p::{PeerId, swarm::NetworkBehaviour};
use nomos_core::{da::BlobId, header::HeaderId, sdp::SessionNumber};
use nomos_da_messages::{common, sampling, sampling::SampleResponse};
use serde::{Deserialize, Serialize};
use streams::SampleStream;
use subnetworks_assignations::MembershipHandler;
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    SubnetworkId,
    addressbook::AddressBookHandler,
    protocols::sampling::{
        errors::{HistoricCommitmentsError, HistoricSamplingError},
        historic::request_behaviour::HistoricRequestSamplingBehaviour,
        opinions::OpinionEvent,
        requests::request_behaviour::RequestSamplingBehaviour,
        responses::response_behaviour::ResponseSamplingBehaviour,
    },
    swarm::validator::{CommitmentsArgs, SampleArgs},
};

type SampleResponseFutureSuccess = (SessionNumber, PeerId, SampleResponse, SampleStream);
type SampleRequestFutureSuccess = (PeerId, SampleStream);

type SampleResponseFutureError = (SamplingError, Option<SampleStream>, SessionNumber);
type SampleRequestFutureError = (SamplingError, Option<SampleStream>);

type SamplingResponseStreamFuture =
    BoxFuture<'static, Result<SampleResponseFutureSuccess, SampleResponseFutureError>>;
type SamplingRequestStreamFuture =
    BoxFuture<'static, Result<SampleRequestFutureSuccess, SampleRequestFutureError>>;

/// Auxiliary struct that binds where to send a request and the pair channel to
/// listen for a response
struct ResponseChannel {
    request_sender: Sender<BehaviourSampleReq>,
    response_receiver: Receiver<BehaviourSampleRes>,
}

#[derive(Debug, Clone)]
pub enum BehaviourSampleReq {
    Share {
        blob_id: BlobId,
        share_idx: ShareIndex,
    },
    Commitments {
        blob_id: BlobId,
    },
}

impl TryFrom<sampling::SampleRequest> for BehaviourSampleReq {
    type Error = Vec<u8>;

    fn try_from(req: sampling::SampleRequest) -> Result<Self, Self::Error> {
        match req {
            sampling::SampleRequest::Share(sampling::SampleShare { blob_id, share_idx }) => {
                Ok(Self::Share { blob_id, share_idx })
            }
            sampling::SampleRequest::Commitments(sampling::SampleCommitments { blob_id }) => {
                Ok(Self::Commitments { blob_id })
            }
        }
    }
}

#[derive(Debug)]
pub enum BehaviourSampleRes {
    SamplingSuccess {
        blob_id: BlobId,
        subnetwork_id: SubnetworkId,
        share: Box<DaLightShare>,
    },
    CommitmentsSuccess {
        blob_id: BlobId,
        commitments: Box<DaSharesCommitments>,
    },
    SampleNotFound {
        blob_id: BlobId,
        subnetwork_id: SubnetworkId,
    },
    CommitmentsNotFound {
        blob_id: BlobId,
    },
}

impl From<BehaviourSampleRes> for SampleResponse {
    fn from(res: BehaviourSampleRes) -> Self {
        match res {
            BehaviourSampleRes::SamplingSuccess { share, blob_id, .. } => {
                Self::Share(common::LightShare::new(blob_id, *share))
            }
            BehaviourSampleRes::CommitmentsSuccess { commitments, .. } => {
                Self::Commitments(*commitments)
            }
            BehaviourSampleRes::SampleNotFound {
                blob_id,
                subnetwork_id,
            } => Self::Error(sampling::SampleError::new_share(
                blob_id,
                subnetwork_id,
                sampling::SampleErrorType::NotFound,
                "Sample not found",
            )),
            BehaviourSampleRes::CommitmentsNotFound { blob_id } => {
                Self::Error(sampling::SampleError::new_commitments(
                    blob_id,
                    sampling::SampleErrorType::NotFound,
                    "Commitments not found",
                ))
            }
        }
    }
}

#[derive(Debug)]
pub enum SamplingEvent {
    /// A blob successfully arrived its destination
    SamplingSuccess {
        blob_id: BlobId,
        subnetwork_id: SubnetworkId,
        light_share: Box<DaLightShare>,
    },
    HistoricSamplingSuccess {
        block_id: HeaderId,
        shares: HashMap<BlobId, Vec<DaLightShare>>,
        commitments: HashMap<BlobId, DaSharesCommitments>,
    },
    HistoricCommitmentsSuccess {
        block_id: HeaderId,
        blob_id: BlobId,
        commitments: DaSharesCommitments,
    },
    CommitmentsSuccess {
        blob_id: BlobId,
        commitments: Box<DaSharesCommitments>,
    },
    IncomingSample {
        request_receiver: Receiver<BehaviourSampleReq>,
        response_sender: Sender<BehaviourSampleRes>,
    },
    SamplingError {
        error: SamplingError,
    },
    HistoricSamplingError {
        block_id: HeaderId,
        error: HistoricSamplingError,
    },
    HistoricCommitmentsError {
        block_id: HeaderId,
        error: HistoricCommitmentsError,
    },
    Opinion(OpinionEvent),
}

impl From<requests::SamplingEvent> for SamplingEvent {
    fn from(value: requests::SamplingEvent) -> Self {
        match value {
            requests::SamplingEvent::SamplingSuccess {
                blob_id,
                subnetwork_id,
                light_share,
            } => Self::SamplingSuccess {
                blob_id,
                subnetwork_id,
                light_share,
            },
            requests::SamplingEvent::CommitmentsSuccess {
                blob_id,
                commitments,
            } => Self::CommitmentsSuccess {
                blob_id,
                commitments,
            },
            requests::SamplingEvent::SamplingError { error } => Self::SamplingError { error },
            requests::SamplingEvent::Opinion(opinion_event) => Self::Opinion(opinion_event),
        }
    }
}

impl From<historic::HistoricSamplingEvent> for SamplingEvent {
    fn from(value: historic::HistoricSamplingEvent) -> Self {
        match value {
            historic::HistoricSamplingEvent::SamplingSuccess {
                block_id,
                commitments,
                shares,
            } => Self::HistoricSamplingSuccess {
                block_id,
                commitments,
                shares,
            },
            historic::HistoricSamplingEvent::CommitmentsSuccess {
                block_id,
                blob_id,
                commitments,
            } => Self::HistoricCommitmentsSuccess {
                block_id,
                blob_id,
                commitments,
            },
            historic::HistoricSamplingEvent::SamplingError { block_id, error } => {
                Self::HistoricSamplingError { block_id, error }
            }
            historic::HistoricSamplingEvent::CommitmentsError { block_id, error } => {
                Self::HistoricCommitmentsError { block_id, error }
            }
            historic::HistoricSamplingEvent::Opinion(opinion_event) => Self::Opinion(opinion_event),
        }
    }
}

impl From<responses::SamplingEvent> for SamplingEvent {
    fn from(value: responses::SamplingEvent) -> Self {
        match value {
            responses::SamplingEvent::IncomingSample {
                request_receiver,
                response_sender,
            } => Self::IncomingSample {
                request_receiver,
                response_sender,
            },
            responses::SamplingEvent::SamplingError { error } => Self::SamplingError { error },
        }
    }
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub struct SubnetsConfig {
    /// Number of unique subnets that samples should be taken from when sampling
    /// a blob.
    pub num_of_subnets: usize,
    /// Numer of connection attemps to the peers in a subnetwork, if previous
    /// connection attempt failed.
    pub shares_retry_limit: usize,
    /// Number of attemts for retrieving commitments from a random sampling
    /// peers that are selected for connections.
    pub commitments_retry_limit: usize,
}

#[derive(NetworkBehaviour)]
#[behaviour(out_event = "SamplingEvent")]
pub struct SamplingBehaviour<Membership, HistoricMembership, Addressbook>
where
    Membership: MembershipHandler,
    HistoricMembership: MembershipHandler,
    Addressbook: AddressBookHandler,
{
    requests: RequestSamplingBehaviour<Membership, Addressbook>,
    historical_requests: HistoricRequestSamplingBehaviour<HistoricMembership, Addressbook>,
    responses: ResponseSamplingBehaviour,
}

impl<Membership, HistoricMembership, Addressbook>
    SamplingBehaviour<Membership, HistoricMembership, Addressbook>
where
    Membership: MembershipHandler + Clone + Send + Sync + 'static,
    Membership::NetworkId: Send,
    HistoricMembership: MembershipHandler + Clone + Send + Sync + 'static,
    HistoricMembership::NetworkId: Send,
    Addressbook: AddressBookHandler + Clone + 'static,
{
    pub fn new(
        local_peer_id: PeerId,
        membership: Membership,
        addressbook: Addressbook,
        subnets_config: SubnetsConfig,
        refresh_signal: impl futures::Stream<Item = ()> + Send + 'static,
    ) -> Self {
        Self {
            requests: RequestSamplingBehaviour::new(
                local_peer_id,
                membership,
                addressbook.clone(),
                subnets_config,
                refresh_signal,
            ),
            historical_requests: HistoricRequestSamplingBehaviour::new(
                local_peer_id,
                addressbook,
                subnets_config,
            ),
            responses: ResponseSamplingBehaviour::new(subnets_config),
        }
    }

    pub fn shares_request_channel(&self) -> UnboundedSender<(BlobId, SessionNumber)> {
        self.requests.shares_request_channel()
    }

    pub fn commitments_request_channel(&self) -> UnboundedSender<(BlobId, SessionNumber)> {
        self.requests.commitments_request_channel()
    }

    pub fn historical_request_channel(&self) -> UnboundedSender<SampleArgs<HistoricMembership>> {
        self.historical_requests.historic_request_channel()
    }

    pub fn historical_commitments_request_channel(
        &self,
    ) -> UnboundedSender<CommitmentsArgs<HistoricMembership>> {
        self.historical_requests
            .historic_commitments_request_channel()
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use futures::StreamExt as _;
    use kzgrs::Proof;
    use kzgrs_backend::common::{Column, share::DaLightShare};
    use libp2p::{Multiaddr, PeerId, Swarm, identity::Keypair, swarm::SwarmEvent};
    use log::debug;
    use rand::Rng as _;
    use subnetworks_assignations::MembershipHandler;
    use tokio::time;
    use tokio_stream::wrappers::IntervalStream;
    use tracing_subscriber::{EnvFilter, fmt::TestWriter};

    use super::{SamplingBehaviour, SamplingEvent};
    use crate::{
        SubnetworkId,
        addressbook::AddressBookHandler,
        protocols::sampling::{BehaviourSampleRes, SubnetsConfig},
        test_utils::{AllNeighbours, new_swarm_in_memory},
    };

    async fn test_sampling_swarm(
        mut swarm: Swarm<
            SamplingBehaviour<
                impl MembershipHandler<Id = PeerId, NetworkId = SubnetworkId>
                + Clone
                + Send
                + Sync
                + 'static,
                impl MembershipHandler<Id = PeerId, NetworkId = SubnetworkId>
                + Clone
                + Send
                + Sync
                + 'static,
                impl AddressBookHandler<Id = PeerId> + Send + Sync + 'static,
            >,
        >,
        msg_count: usize,
    ) -> Vec<[u8; 32]> {
        let mut res = vec![];
        loop {
            match swarm.next().await {
                None => {}
                Some(SwarmEvent::Behaviour(SamplingEvent::IncomingSample {
                    request_receiver,
                    response_sender,
                })) => {
                    debug!("Received request");
                    // spawn here because otherwise we block polling
                    tokio::spawn(request_receiver);
                    response_sender
                        .send(BehaviourSampleRes::SamplingSuccess {
                            blob_id: Default::default(),
                            subnetwork_id: Default::default(),
                            share: Box::new(DaLightShare {
                                column: Column(vec![]),
                                share_idx: 0,
                                combined_column_proof: Proof::default(),
                            }),
                        })
                        .unwrap();
                }
                Some(SwarmEvent::Behaviour(SamplingEvent::SamplingSuccess { blob_id, .. })) => {
                    debug!("Received response");
                    res.push(blob_id);
                }
                Some(SwarmEvent::Behaviour(SamplingEvent::SamplingError { error })) => {
                    debug!("Error during sampling: {error}");
                }
                Some(event) => {
                    debug!("{event:?}");
                }
            }
            if res.len() == msg_count {
                break res;
            }
        }
    }

    #[tokio::test]
    async fn test_sampling_two_peers() {
        const MSG_COUNT: usize = 10;

        drop(
            tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::from_default_env())
                .compact()
                .with_writer(TestWriter::default())
                .try_init(),
        );
        let k1 = Keypair::generate_ed25519();
        let k2 = Keypair::generate_ed25519();

        // Generate a random peer ids not to conflict with other tests
        let p1_id = rand::rng().random::<u64>();
        let p1_address: Multiaddr = format!("/memory/{p1_id}").parse().unwrap();

        let p2_id = rand::rng().random::<u64>();
        let p2_address: Multiaddr = format!("/memory/{p2_id}").parse().unwrap();

        let neighbours_p1 = AllNeighbours::default();
        neighbours_p1.add_neighbour(PeerId::from_public_key(&k1.public()));
        neighbours_p1.add_neighbour(PeerId::from_public_key(&k2.public()));
        let p1_addresses = vec![(PeerId::from_public_key(&k2.public()), p2_address.clone())];
        neighbours_p1.update_addresses(p1_addresses);

        let neighbours_p2 = AllNeighbours::default();
        neighbours_p2.add_neighbour(PeerId::from_public_key(&k1.public()));
        neighbours_p2.add_neighbour(PeerId::from_public_key(&k2.public()));
        let p2_addresses = vec![(PeerId::from_public_key(&k1.public()), p1_address.clone())];
        neighbours_p2.update_addresses(p2_addresses);

        let p1_behavior: SamplingBehaviour<_, AllNeighbours, _> = SamplingBehaviour::new(
            PeerId::from_public_key(&k1.public()),
            neighbours_p1.clone(),
            neighbours_p1.clone(),
            SubnetsConfig {
                num_of_subnets: 1,
                shares_retry_limit: 1,
                commitments_retry_limit: 1,
            },
            Box::pin(IntervalStream::new(time::interval(Duration::from_secs(1))).map(|_| ())),
        );

        let mut p1 = new_swarm_in_memory(&k1, p1_behavior);

        let p2_behavior: SamplingBehaviour<_, AllNeighbours, _> = SamplingBehaviour::new(
            PeerId::from_public_key(&k2.public()),
            neighbours_p2.clone(),
            neighbours_p2.clone(),
            SubnetsConfig {
                num_of_subnets: 1,
                shares_retry_limit: 1,
                commitments_retry_limit: 1,
            },
            Box::pin(IntervalStream::new(time::interval(Duration::from_secs(1))).map(|_| ())),
        );
        let mut p2 = new_swarm_in_memory(&k2, p2_behavior);

        let request_sender_1 = p1.behaviour().shares_request_channel();
        let request_sender_2 = p2.behaviour().shares_request_channel();
        let _p1_address = p1_address.clone();
        let _p2_address = p2_address.clone();

        let t1 = tokio::spawn(async move {
            p1.listen_on(p1_address).unwrap();
            time::sleep(Duration::from_secs(1)).await;
            test_sampling_swarm(p1, MSG_COUNT).await
        });
        let t2 = tokio::spawn(async move {
            p2.listen_on(p2_address).unwrap();
            time::sleep(Duration::from_secs(1)).await;
            test_sampling_swarm(p2, MSG_COUNT).await
        });
        time::sleep(Duration::from_secs(2)).await;
        for i in 0..MSG_COUNT {
            request_sender_1.send(([i as u8; 32], 0)).unwrap();
            request_sender_2.send(([i as u8; 32], 0)).unwrap();
        }

        let res1 = t1.await.unwrap();
        let res2 = t2.await.unwrap();
        assert_eq!(res1, res2);
    }
}
