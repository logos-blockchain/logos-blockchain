use std::{
    collections::{HashMap, HashSet},
    fmt::Debug,
    time::Duration,
};

use futures::{
    StreamExt,
    channel::oneshot::{Receiver, Sender},
};
use kzgrs_backend::common::{
    ShareIndex,
    share::{DaLightShare, DaShare, DaSharesCommitments},
};
use nomos_core::{da::BlobId, header::HeaderId, mantle::SignedMantleTx, sdp::SessionNumber};
use nomos_da_messages::common::Share;
use nomos_da_network_core::{
    maintenance::{balancer::ConnectionBalancerCommand, monitor::ConnectionMonitorCommand},
    protocols::{
        dispersal::validator::behaviour::DispersalEvent,
        sampling::{
            self, BehaviourSampleReq, BehaviourSampleRes, SubnetsConfig,
            errors::{HistoricCommitmentsError, HistoricSamplingError, SamplingError},
            opinions::OpinionEvent,
        },
    },
    swarm::{
        DAConnectionMonitorSettings, DAConnectionPolicySettings, DispersalValidationError,
        DispersalValidationResult, DispersalValidatorEvent, ReplicationConfig,
        validator::{CommitmentsArgs, SampleArgs, ValidatorEventsStream},
    },
};
use nomos_libp2p::{Multiaddr, ed25519, secret_key_serde};
use serde::{Deserialize, Serialize};
use tokio::sync::{
    broadcast,
    mpsc::{self, UnboundedSender, error::SendError},
    oneshot,
};
use tracing::error;

pub(crate) const BROADCAST_CHANNEL_SIZE: usize = 128;

pub type BroadcastValidationResultSender = Option<mpsc::Sender<DispersalValidationResult>>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DaNetworkBackendSettings {
    // Identification Secp256k1 private key in Hex format (`0x123...abc`). Default random.
    #[serde(with = "secret_key_serde", default = "ed25519::SecretKey::generate")]
    pub node_key: ed25519::SecretKey,
    pub listening_address: Multiaddr,
    pub policy_settings: DAConnectionPolicySettings,
    pub monitor_settings: DAConnectionMonitorSettings,
    pub balancer_interval: Duration,
    pub redial_cooldown: Duration,
    pub replication_settings: ReplicationConfig,
    pub subnets_settings: SubnetsConfig,
}

/// Sampling events coming from da network
#[derive(Debug, Clone)]
pub enum SamplingEvent {
    /// A success sampling
    SamplingSuccess {
        blob_id: BlobId,
        light_share: Box<DaLightShare>,
    },
    /// Incoming sampling request
    SamplingRequest {
        blob_id: BlobId,
        share_idx: ShareIndex,
        response_sender: mpsc::Sender<Option<DaLightShare>>,
    },
    /// A failed sampling error
    SamplingError { error: SamplingError },
}

impl SamplingEvent {
    #[must_use]
    pub fn blob_id(&self) -> Option<&BlobId> {
        match self {
            Self::SamplingRequest { blob_id, .. } | Self::SamplingSuccess { blob_id, .. } => {
                Some(blob_id)
            }
            Self::SamplingError { error } => error.blob_id(),
        }
    }

    #[must_use]
    pub fn has_blob_id(&self, target: &BlobId) -> bool {
        self.blob_id() == Some(target)
    }
}

/// Commitments events coming from da network.
#[derive(Debug, Clone)]
pub enum CommitmentsEvent {
    /// A success sampling
    CommitmentsSuccess {
        blob_id: BlobId,
        commitments: Box<DaSharesCommitments>,
    },
    /// Incoming sampling request
    CommitmentsRequest {
        blob_id: BlobId,
        response_sender: mpsc::Sender<Option<DaSharesCommitments>>,
    },
    /// A failed sampling error
    CommitmentsError { error: SamplingError },
}

impl CommitmentsEvent {
    #[must_use]
    pub fn blob_id(&self) -> Option<&BlobId> {
        match self {
            Self::CommitmentsRequest { blob_id, .. } | Self::CommitmentsSuccess { blob_id, .. } => {
                Some(blob_id)
            }
            Self::CommitmentsError { error } => error.blob_id(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum HistoricSamplingEvent {
    SamplingSuccess {
        block_id: HeaderId,
        shares: HashMap<BlobId, Vec<DaLightShare>>,
        commitments: HashMap<BlobId, DaSharesCommitments>,
    },
    SamplingError {
        block_id: HeaderId,
        error: HistoricSamplingError,
    },
    CommitmentsSuccess {
        block_id: HeaderId,
        blob_id: BlobId,
        commitments: DaSharesCommitments,
    },
    CommitmentsError {
        block_id: HeaderId,
        error: HistoricCommitmentsError,
    },
}

#[derive(Debug, Clone)]
pub enum VerificationEvent {
    Tx {
        // Number of subnetwork assignations that the node is assigned to at the moment of
        // receiving a dispersal TX. This is added to the TX message received via the network
        // because of dynamic assignations that can change depending
        // on the DA session, and syncinc these moments preciselly between services even if some
        // TX events might reach destination after the assignations has already changed.
        assignations: u16,
        tx: Box<SignedMantleTx>,
        response_sender: BroadcastValidationResultSender,
    },
    Share {
        share: Box<DaShare>,
        response_sender: BroadcastValidationResultSender,
    },
}

impl From<(DaShare, BroadcastValidationResultSender)> for VerificationEvent {
    fn from((share, response_sender): (DaShare, BroadcastValidationResultSender)) -> Self {
        Self::Share {
            share: Box::new(share),
            response_sender,
        }
    }
}

impl From<(u16, Box<SignedMantleTx>, BroadcastValidationResultSender)> for VerificationEvent {
    fn from(
        (assignations, tx, response_sender): (
            u16,
            Box<SignedMantleTx>,
            BroadcastValidationResultSender,
        ),
    ) -> Self {
        Self::Tx {
            assignations,
            tx,
            response_sender,
        }
    }
}

/// Task that handles forwarding of events to the subscriptions channels/stream
pub(crate) async fn handle_validator_events_stream(
    events_streams: ValidatorEventsStream,
    sampling_broadcast_sender: broadcast::Sender<SamplingEvent>,
    commitments_broadcast_sender: broadcast::Sender<CommitmentsEvent>,
    validation_broadcast_sender: broadcast::Sender<VerificationEvent>,
    historic_sample_broadcast_sender: broadcast::Sender<HistoricSamplingEvent>,
    opinion_sender: UnboundedSender<OpinionEvent>,
) {
    let ValidatorEventsStream {
        mut sampling_events_receiver,
        mut validation_events_receiver,
    } = events_streams;
    loop {
        // WARNING: `StreamExt::next` is cancellation safe.
        // If adding more branches check if such methods are within the cancellation
        // safe set: https://docs.rs/tokio/latest/tokio/macro.select.html#cancellation-safety
        tokio::select! {
            Some(sampling_event) = StreamExt::next(&mut sampling_events_receiver) => {
                handle_sampling_event(&sampling_broadcast_sender, &commitments_broadcast_sender, &historic_sample_broadcast_sender, &historic_sample_broadcast_sender, &opinion_sender, sampling_event).await;
            }
            Some(dispersal_event) = StreamExt::next(&mut validation_events_receiver) => {
                handle_dispersal_event(&validation_broadcast_sender, dispersal_event).await;
            }
        }
    }
}

#[expect(
    clippy::cognitive_complexity,
    reason = "complex comunication between libp2p behaviour and service"
)]
async fn handle_dispersal_event(
    validation_broadcast_sender: &broadcast::Sender<VerificationEvent>,
    dispersal_event: DispersalValidatorEvent,
) {
    match (dispersal_event.event, dispersal_event.sender) {
        (DispersalEvent::IncomingShare(share), Some(sender)) => {
            handle_incoming_share_with_response(validation_broadcast_sender, sender, share).await;
        }
        (DispersalEvent::IncomingShare(share), None) => {
            if let Err(error) = validation_broadcast_sender.send((share.data, None).into()) {
                error!(
                    "Error in internal broadcast of validation for blob: {:?}",
                    error.0
                );
            }
        }
        (DispersalEvent::IncomingTx((assignations, tx)), Some(sender)) => {
            handle_incoming_tx_with_response(validation_broadcast_sender, sender, assignations, tx)
                .await;
        }
        (DispersalEvent::IncomingTx((assignations, tx)), None) => {
            if let Err(error) = validation_broadcast_sender.send((assignations, tx, None).into()) {
                error!(
                    "Error in internal broadcast of validation for blob: {:?}",
                    error.0
                );
            }
        }
        (DispersalEvent::DispersalError { error }, _) => {
            error!("Error from dispersal behaviour: {error:?}");
        }
    }
}

#[expect(
    clippy::cognitive_complexity,
    reason = "Channel responses need to be checked"
)]
async fn handle_incoming_share_with_response(
    validation_broadcast_sender: &broadcast::Sender<VerificationEvent>,
    behaviour_sender: Sender<DispersalValidationResult>,
    share: Box<Share>,
) {
    let (service_sender, mut service_receiver) =
        mpsc::channel::<DispersalValidationResult>(BROADCAST_CHANNEL_SIZE);
    if let Err(error) = validation_broadcast_sender.send((share.data, Some(service_sender)).into())
    {
        if let Err(error) = behaviour_sender.send(Err(DispersalValidationError)) {
            error!("Error in internal response sender of share validation error: {error:?}");
        }
        error!(
            "Error in internal broadcast of validation for blob: {:?}",
            error.0
        );
        return;
    }
    let validation_response = service_receiver
        .recv()
        .await
        .unwrap_or(Err(DispersalValidationError));
    if let Err(error) = behaviour_sender.send(validation_response) {
        error!("Error in internal response sender of share validation success: {error:?}");
    }
}

#[expect(
    clippy::cognitive_complexity,
    reason = "Channel responses need to be checked"
)]
async fn handle_incoming_tx_with_response(
    validation_broadcast_sender: &broadcast::Sender<VerificationEvent>,
    behaviour_sender: Sender<DispersalValidationResult>,
    assignations: u16,
    tx: Box<SignedMantleTx>,
) {
    let (service_sender, mut service_receiver) =
        mpsc::channel::<DispersalValidationResult>(BROADCAST_CHANNEL_SIZE);
    if let Err(error) =
        validation_broadcast_sender.send((assignations, tx, Some(service_sender)).into())
    {
        if let Err(error) = behaviour_sender.send(Err(DispersalValidationError)) {
            error!("Error in internal response sender of share validation error: {error:?}",);
        }
        error!(
            "Error in internal broadcast of validation for blob: {:?}",
            error.0
        );
        return;
    }
    let validation_response = service_receiver
        .recv()
        .await
        .unwrap_or(Err(DispersalValidationError));
    if let Err(error) = behaviour_sender.send(validation_response) {
        error!("Error in internal response sender of share validation success: {error:?}");
    }
}

async fn handle_sampling_event(
    sampling_broadcast_sender: &broadcast::Sender<SamplingEvent>,
    commitments_broadcast_sender: &broadcast::Sender<CommitmentsEvent>,
    historic_sample_broadcast_sender: &broadcast::Sender<HistoricSamplingEvent>,
    historic_commitments_broadcast_sender: &broadcast::Sender<HistoricSamplingEvent>,
    opinion_sender: &UnboundedSender<OpinionEvent>,
    sampling_event: sampling::SamplingEvent,
) {
    match sampling_event {
        sampling::SamplingEvent::SamplingSuccess {
            blob_id,
            light_share,
            ..
        } => {
            if let Err(e) = sampling_broadcast_sender.send(SamplingEvent::SamplingSuccess {
                blob_id,
                light_share,
            }) {
                error!("Error in internal broadcast of sampling success: {e:?}");
            }
        }
        sampling::SamplingEvent::CommitmentsSuccess {
            blob_id,
            commitments,
        } => {
            if let Err(e) =
                commitments_broadcast_sender.send(CommitmentsEvent::CommitmentsSuccess {
                    blob_id,
                    commitments,
                })
            {
                error!("Error in internal broadcast of sampling success: {e:?}");
            }
        }
        sampling::SamplingEvent::IncomingSample {
            request_receiver,
            response_sender,
        } => {
            handle_sampling_request(
                sampling_broadcast_sender,
                commitments_broadcast_sender,
                request_receiver,
                response_sender,
            )
            .await;
        }
        sampling::SamplingEvent::SamplingError { error } => {
            handle_sampling_error(
                sampling_broadcast_sender,
                commitments_broadcast_sender,
                error,
            );
        }
        sampling::SamplingEvent::HistoricSamplingSuccess {
            block_id,
            shares,
            commitments,
        } => handle_historic_sample_success(
            block_id,
            shares,
            commitments,
            historic_sample_broadcast_sender,
        ),
        sampling::SamplingEvent::HistoricSamplingError { block_id, error } => {
            handle_historic_sample_error(block_id, error, historic_sample_broadcast_sender);
        }
        sampling::SamplingEvent::Opinion(opinion_event) => {
            handle_opinion_event(opinion_sender, opinion_event);
        }
        sampling::SamplingEvent::HistoricCommitmentsSuccess {
            block_id,
            blob_id,
            commitments,
        } => handle_historic_commitments_success(
            block_id,
            blob_id,
            commitments,
            historic_commitments_broadcast_sender,
        ),
        sampling::SamplingEvent::HistoricCommitmentsError { block_id, error } => {
            handle_historic_commitments_error(
                block_id,
                error,
                historic_commitments_broadcast_sender,
            );
        }
    }
}

fn handle_historic_commitments_success(
    block_id: HeaderId,
    blob_id: BlobId,
    commitments: DaSharesCommitments,
    historic_commitments_broadcast_sender: &broadcast::Sender<HistoricSamplingEvent>,
) {
    if let Err(e) =
        historic_commitments_broadcast_sender.send(HistoricSamplingEvent::CommitmentsSuccess {
            block_id,
            blob_id,
            commitments,
        })
    {
        error!("Error in internal broadcast of historic commitments success: {e:?}");
    }
}

fn handle_historic_commitments_error(
    block_id: HeaderId,
    error: HistoricCommitmentsError,
    historic_commitments_broadcast_sender: &broadcast::Sender<HistoricSamplingEvent>,
) {
    if let Err(e) = historic_commitments_broadcast_sender
        .send(HistoricSamplingEvent::CommitmentsError { block_id, error })
    {
        error!("Error in internal broadcast of historic commitments error: {e:?}");
    }
}

fn handle_opinion_event(
    opinion_sender: &UnboundedSender<OpinionEvent>,
    opinion_event: OpinionEvent,
) {
    if let Err(e) = opinion_sender.send(opinion_event) {
        error!("Error in internal broadcast of opinion event: {e:?}");
    }
}

fn handle_historic_sample_success(
    block_id: HeaderId,
    shares: HashMap<BlobId, Vec<DaLightShare>>,
    commitments: HashMap<BlobId, DaSharesCommitments>,
    historic_sample_broadcast_sender: &broadcast::Sender<HistoricSamplingEvent>,
) {
    if let Err(e) = historic_sample_broadcast_sender.send(HistoricSamplingEvent::SamplingSuccess {
        block_id,
        shares,
        commitments,
    }) {
        error!("Error in internal broadcast of historic sampling success: {e:?}");
    }
}

fn handle_historic_sample_error(
    block_id: HeaderId,
    error: HistoricSamplingError,
    historic_sample_broadcast_sender: &broadcast::Sender<HistoricSamplingEvent>,
) {
    if let Err(e) = historic_sample_broadcast_sender
        .send(HistoricSamplingEvent::SamplingError { block_id, error })
    {
        error!("Error in internal broadcast of historic sampling error: {e:?}");
    }
}

fn handle_sampling_error(
    sampling_broadcast_sender: &broadcast::Sender<SamplingEvent>,
    commitments_broadcast_sender: &broadcast::Sender<CommitmentsEvent>,
    error: SamplingError,
) {
    if let Err(e) = sampling_broadcast_sender.send(SamplingEvent::SamplingError {
        error: error.clone(),
    }) {
        error! {"Error in internal broadcast of sampling error: {e:?}"};
    }
    if let Err(e) = commitments_broadcast_sender.send(CommitmentsEvent::CommitmentsError { error })
    {
        error! {"Error in internal broadcast of commitments error: {e:?}"};
    }
}

async fn handle_sampling_request(
    sampling_broadcast_sender: &broadcast::Sender<SamplingEvent>,
    commitments_broadcast_sender: &broadcast::Sender<CommitmentsEvent>,
    request_receiver: Receiver<BehaviourSampleReq>,
    response_sender: Sender<BehaviourSampleRes>,
) {
    match request_receiver.await {
        Ok(BehaviourSampleReq::Share { blob_id, share_idx }) => {
            handle_incoming_share_request(
                sampling_broadcast_sender,
                response_sender,
                blob_id,
                share_idx,
            )
            .await;
        }
        Ok(BehaviourSampleReq::Commitments { blob_id }) => {
            handle_incoming_commitments_request(
                commitments_broadcast_sender,
                response_sender,
                blob_id,
            )
            .await;
        }
        Err(e) => {
            error!("Request receiver was closed: {e}");
        }
    }
}

#[expect(clippy::cognitive_complexity, reason = "share flow in one place")]
async fn handle_incoming_share_request(
    sampling_broadcast_sender: &broadcast::Sender<SamplingEvent>,
    response_sender: Sender<BehaviourSampleRes>,
    blob_id: BlobId,
    share_idx: u16,
) {
    let (sampling_response_sender, mut sampling_response_receiver) = mpsc::channel(1);

    if let Err(e) = sampling_broadcast_sender.send(SamplingEvent::SamplingRequest {
        blob_id,
        share_idx,
        response_sender: sampling_response_sender,
    }) {
        error!("Error in internal broadcast of sampling request: {e:?}");
        sampling_response_receiver.close();
    }

    if let Some(maybe_share) = sampling_response_receiver.recv().await {
        let result = match maybe_share {
            Some(share) => BehaviourSampleRes::SamplingSuccess {
                blob_id,
                subnetwork_id: share.share_idx,
                share: Box::new(share),
            },
            None => BehaviourSampleRes::SampleNotFound {
                blob_id,
                subnetwork_id: share_idx,
            },
        };

        if response_sender.send(result).is_err() {
            error!("Error sending sampling success response");
        }
    } else if response_sender
        .send(BehaviourSampleRes::SampleNotFound {
            blob_id,
            subnetwork_id: share_idx,
        })
        .is_err()
    {
        error!("Error sending sampling success response");
    }
}

#[expect(clippy::cognitive_complexity, reason = "commitments flow in one place")]
async fn handle_incoming_commitments_request(
    commitments_broadcast_sender: &broadcast::Sender<CommitmentsEvent>,
    response_sender: Sender<BehaviourSampleRes>,
    blob_id: BlobId,
) {
    let (commitments_response_sender, mut commitments_response_receiver) = mpsc::channel(1);

    if let Err(e) = commitments_broadcast_sender.send(CommitmentsEvent::CommitmentsRequest {
        blob_id,
        response_sender: commitments_response_sender,
    }) {
        error!("Error in internal broadcast of commitments request: {e:?}");
        commitments_response_receiver.close();
    }

    if let Some(maybe_share) = commitments_response_receiver.recv().await {
        let result = maybe_share.map_or(
            BehaviourSampleRes::CommitmentsNotFound { blob_id },
            |commitments| BehaviourSampleRes::CommitmentsSuccess {
                blob_id,
                commitments: Box::new(commitments),
            },
        );

        if response_sender.send(result).is_err() {
            error!("Error sending commitments response");
        }
    } else if response_sender
        .send(BehaviourSampleRes::CommitmentsNotFound { blob_id })
        .is_err()
    {
        error!("Error sending commitments response");
    }
}

pub(crate) async fn handle_sample_request(
    sampling_request_channel: &UnboundedSender<(BlobId, SessionNumber)>,
    blob_id: BlobId,
    session: SessionNumber,
) {
    if let Err(SendError(blob_id)) = sampling_request_channel.send((blob_id, session)) {
        error!("Error requesting samples for blob_id: {blob_id:?}");
    }
}

pub(crate) async fn handle_historic_sample_request<Membership>(
    historic_sample_request_channel: &UnboundedSender<SampleArgs<Membership>>,
    block_id: HeaderId,
    blob_ids: HashMap<Membership, HashSet<BlobId>>,
) {
    if let Err(SendError((block_id, _))) =
        historic_sample_request_channel.send((block_id, blob_ids))
    {
        error!("Error requesting historic sample for block_id: {block_id:?}");
    }
}

pub(crate) async fn handle_historic_commitments_request<Membership>(
    historic_commitments_request_channel: &UnboundedSender<CommitmentsArgs<Membership>>,
    block_id: HeaderId,
    blob_id: BlobId,
    session: Membership,
) {
    if let Err(SendError((block_id, _, _))) =
        historic_commitments_request_channel.send((block_id, session, blob_id))
    {
        error!("Error requesting historic commitments for block_id: {block_id:?}");
    }
}

pub(crate) async fn handle_monitor_command<Stats: Debug>(
    monitor_request_channel: &UnboundedSender<ConnectionMonitorCommand<Stats>>,
    command: ConnectionMonitorCommand<Stats>,
) {
    if let Err(SendError(cmd)) = monitor_request_channel.send(command) {
        error!("Channel closed when sending command to monitor: {cmd:?}");
    }
}

pub(crate) async fn handle_balancer_command<Stats: Debug>(
    balancer_request_channel: &UnboundedSender<ConnectionBalancerCommand<Stats>>,
    response_sender: oneshot::Sender<Stats>,
) {
    if let Err(SendError(cmd)) =
        balancer_request_channel.send(ConnectionBalancerCommand::Stats(response_sender))
    {
        error!("Error stats request: {cmd:?}");
    }
}
