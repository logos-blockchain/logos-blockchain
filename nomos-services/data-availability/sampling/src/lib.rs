pub mod backend;
pub mod mempool;
pub mod network;
pub mod storage;
pub mod verifier;

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fmt::{Debug, Display},
    marker::PhantomData,
    time::Duration,
};

use backend::{DaSamplingServiceBackend, SamplingState};
use either::Either;
use futures::{FutureExt as _, Stream, future::BoxFuture, stream::FuturesUnordered};
use kzgrs_backend::common::{
    ShareIndex,
    share::{DaLightShare, DaShare, DaSharesCommitments},
};
use network::NetworkAdapter;
use nomos_core::{da::BlobId, header::HeaderId, mantle::SignedMantleTx, sdp::SessionNumber};
use nomos_da_network_core::protocols::sampling::errors::SamplingError;
use nomos_da_network_service::{
    NetworkService,
    backends::libp2p::common::{CommitmentsEvent, HistoricSamplingEvent, SamplingEvent},
};
use nomos_storage::StorageService;
use nomos_tracing::{error_with_id, info_with_id};
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};
use serde::{Deserialize, Serialize};
use services_utils::wait_until_services_are_ready;
use storage::DaStorageAdapter;
use subnetworks_assignations::MembershipHandler;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::StreamExt as _;
use tracing::{error, instrument};
use verifier::{VerifierBackend, kzgrs::KzgrsDaVerifier};

use crate::mempool::{Blob, DaMempoolAdapter};

const HISTORICAL_SAMPLING_TIMEOUT: Duration = Duration::from_secs(30);

type HistoricFallbackResult = (BlobId, Option<(Vec<DaLightShare>, DaSharesCommitments)>);
type LongTask = BoxFuture<'static, ()>;
type HistoricSamplingFallbackTask = BoxFuture<'static, HistoricFallbackResult>;
type SamplingContinuationTask = BoxFuture<'static, (Blob, Option<DaSharesCommitments>)>;
type HistoricCommitmentsFallbackResult = (BlobId, Option<DaSharesCommitments>);
type HistoricCommitmentsFallbackTask = BoxFuture<'static, HistoricCommitmentsFallbackResult>;

struct PendingTasks<'a> {
    long_tasks: &'a mut FuturesUnordered<BoxFuture<'static, ()>>,
    sampling_continuations: &'a mut FuturesUnordered<SamplingContinuationTask>,
    delayed_sdp_sampling_triggers: &'a mut FuturesUnordered<BoxFuture<'static, Blob>>,
    historic_fallback_continuations: &'a mut FuturesUnordered<HistoricSamplingFallbackTask>,
    historic_commitments_continuations: &'a mut FuturesUnordered<HistoricCommitmentsFallbackTask>,
}

pub type DaSamplingService<
    SamplingBackend,
    SamplingNetwork,
    SamplingStorage,
    MempoolAdapter,
    RuntimeServiceId,
> = GenericDaSamplingService<
    SamplingBackend,
    SamplingNetwork,
    SamplingStorage,
    KzgrsDaVerifier,
    MempoolAdapter,
    RuntimeServiceId,
>;

#[derive(Debug)]
pub enum DaSamplingServiceMsg<BlobId> {
    TriggerSampling {
        blob_id: BlobId,
        session: SessionNumber,
    },
    GetCommitments {
        blob_id: BlobId,
        session: SessionNumber,
        response_sender: oneshot::Sender<Option<DaSharesCommitments>>,
    },
    GetValidatedBlobs {
        reply_channel: oneshot::Sender<BTreeSet<BlobId>>,
    },
    MarkInBlock {
        blobs_id: Vec<BlobId>,
    },
    RequestHistoricSampling {
        block_id: HeaderId,
        blob_ids: HashMap<BlobId, SessionNumber>,
        reply_channel: oneshot::Sender<bool>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaSamplingServiceSettings<BackendSettings, ShareVerifierSettings> {
    pub sampling_settings: BackendSettings,
    pub share_verifier_settings: ShareVerifierSettings,
    pub commitments_wait_duration: Duration,
    pub sdp_blob_trigger_sampling_delay: Duration,
}

pub struct GenericDaSamplingService<
    SamplingBackend,
    SamplingNetwork,
    SamplingStorage,
    ShareVerifier,
    MempoolAdapter,
    RuntimeServiceId,
> where
    SamplingBackend: DaSamplingServiceBackend,
    SamplingNetwork: NetworkAdapter<RuntimeServiceId>,
    SamplingStorage: DaStorageAdapter<RuntimeServiceId>,
    MempoolAdapter: DaMempoolAdapter,
    ShareVerifier: VerifierBackend,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    _phantom: PhantomData<(
        SamplingBackend,
        SamplingNetwork,
        SamplingStorage,
        MempoolAdapter,
        ShareVerifier,
    )>,
}

impl<
    SamplingBackend,
    SamplingNetwork,
    SamplingStorage,
    ShareVerifier,
    MempoolAdapter,
    RuntimeServiceId,
>
    GenericDaSamplingService<
        SamplingBackend,
        SamplingNetwork,
        SamplingStorage,
        ShareVerifier,
        MempoolAdapter,
        RuntimeServiceId,
    >
where
    SamplingBackend: DaSamplingServiceBackend,
    SamplingNetwork: NetworkAdapter<RuntimeServiceId>,
    SamplingStorage: DaStorageAdapter<RuntimeServiceId>,
    MempoolAdapter: DaMempoolAdapter,
    ShareVerifier: VerifierBackend,
{
    #[must_use]
    pub const fn new(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    ) -> Self {
        Self {
            service_resources_handle,
            _phantom: PhantomData,
        }
    }
}

impl<
    SamplingBackend,
    SamplingNetwork,
    SamplingStorage,
    ShareVerifier,
    MempoolAdapter,
    RuntimeServiceId,
>
    GenericDaSamplingService<
        SamplingBackend,
        SamplingNetwork,
        SamplingStorage,
        ShareVerifier,
        MempoolAdapter,
        RuntimeServiceId,
    >
where
    SamplingBackend: DaSamplingServiceBackend<
            BlobId = BlobId,
            Share = DaShare,
            SharesCommitments = DaSharesCommitments,
        > + Send,
    SamplingBackend::Settings: Clone,
    SamplingNetwork: NetworkAdapter<RuntimeServiceId> + Send + Sync + 'static,
    SamplingStorage: DaStorageAdapter<RuntimeServiceId, Share = DaShare> + Send + Sync,
    MempoolAdapter: DaMempoolAdapter<Tx = SignedMantleTx> + Send + Sync + 'static,
    ShareVerifier: VerifierBackend<DaShare = DaShare> + Send + Sync + Clone + 'static,
{
    #[instrument(skip_all)]
    async fn handle_service_message(
        msg: <Self as ServiceData>::Message,
        network_adapter: &mut SamplingNetwork,
        storage_adapter: &SamplingStorage,
        sampler: &mut SamplingBackend,
        commitments_wait_duration: Duration,
        share_verifier: &ShareVerifier,
        tasks: &mut PendingTasks<'_>,
    ) {
        let (long_tasks, sampling_continuations) =
            (&mut tasks.long_tasks, &mut tasks.sampling_continuations);

        match msg {
            DaSamplingServiceMsg::TriggerSampling { blob_id, session } => {
                if matches!(sampler.init_sampling(blob_id).await, SamplingState::Init) {
                    info_with_id!(blob_id, "InitSampling");

                    if let Ok(Some(commitments)) = storage_adapter.get_commitments(blob_id).await {
                        // Handle inline, no need to wait for commitments over network
                        info_with_id!(blob_id, "Got commitments from storage");
                        sampler.add_commitments(&blob_id, commitments);

                        if let Err(e) = network_adapter.start_sampling(blob_id, session).await {
                            sampler.handle_sampling_error(blob_id).await;
                            error_with_id!(blob_id, "Error starting sampling: {e}");
                        }
                    } else {
                        // Need network fetch - use async path
                        let (tx, rx) = oneshot::channel();

                        if let Some(future) = Self::request_commitments_from_network(
                            network_adapter,
                            commitments_wait_duration,
                            blob_id,
                            session,
                            tx,
                        )
                        .await
                        {
                            long_tasks.push(future);

                            let continuation = async move {
                                let commitments = rx.await.unwrap_or(None);
                                (Blob { blob_id, session }, commitments)
                            }
                            .boxed();

                            sampling_continuations.push(continuation);
                        } else {
                            sampler.handle_sampling_error(blob_id).await;
                        }
                    }
                }
            }
            DaSamplingServiceMsg::GetCommitments {
                blob_id,
                session,
                response_sender,
            } => {
                if let Some(future) = Self::request_commitments(
                    storage_adapter,
                    network_adapter,
                    commitments_wait_duration,
                    blob_id,
                    session,
                    response_sender,
                )
                .await
                {
                    long_tasks.push(future);
                }
            }
            DaSamplingServiceMsg::GetValidatedBlobs { reply_channel } => {
                let validated_blobs = sampler.get_validated_blobs().await;
                if let Err(_e) = reply_channel.send(validated_blobs) {
                    error!("Error repliying validated blobs request");
                }
            }
            DaSamplingServiceMsg::MarkInBlock { blobs_id } => {
                sampler.mark_completed(&blobs_id).await;
            }
            DaSamplingServiceMsg::RequestHistoricSampling {
                block_id,
                blob_ids,
                reply_channel,
            } => {
                if let Some(future) = Self::request_and_wait_historic_sampling(
                    network_adapter,
                    share_verifier,
                    block_id,
                    blob_ids,
                    reply_channel,
                    HISTORICAL_SAMPLING_TIMEOUT,
                )
                .await
                {
                    long_tasks.push(future);
                }
            }
        }
    }

    #[instrument(skip_all)]
    async fn handle_sampling_message(
        event: SamplingEvent,
        sampler: &mut SamplingBackend,
        storage_adapter: &SamplingStorage,
        verifier: &ShareVerifier,
        network_adapter: &SamplingNetwork,
    ) -> Option<(
        LongTask,
        Either<HistoricSamplingFallbackTask, HistoricCommitmentsFallbackTask>,
    )> {
        match event {
            SamplingEvent::SamplingSuccess {
                blob_id,
                light_share,
            } => {
                Self::handle_success(sampler, verifier, blob_id, light_share).await;
                None
            }
            SamplingEvent::SamplingError { error } => {
                Self::handle_error(error, sampler, network_adapter, verifier).await
            }
            SamplingEvent::SamplingRequest {
                blob_id,
                share_idx,
                response_sender,
            } => {
                Self::handle_request(storage_adapter, blob_id, share_idx, response_sender).await;
                None
            }
        }
    }

    async fn handle_success(
        sampler: &mut SamplingBackend,
        verifier: &ShareVerifier,
        blob_id: BlobId,
        light_share: Box<DaLightShare>,
    ) {
        info_with_id!(blob_id, "SamplingSuccess");

        let Some(commitments) = sampler.get_commitments(&blob_id) else {
            error_with_id!(blob_id, "Error getting commitments for blob");
            sampler.handle_sampling_error(blob_id).await;
            return;
        };

        if verifier.verify(&commitments, &light_share).is_err() {
            error_with_id!(blob_id, "SamplingError");
            sampler.handle_sampling_error(blob_id).await;
            return;
        }

        sampler
            .handle_sampling_success(blob_id, light_share.share_idx)
            .await;
    }

    async fn handle_error(
        error: SamplingError,
        sampler: &mut SamplingBackend,
        network_adapter: &SamplingNetwork,
        verifier: &ShareVerifier,
    ) -> Option<(
        LongTask,
        Either<HistoricSamplingFallbackTask, HistoricCommitmentsFallbackTask>,
    )> {
        let Some(blob_id) = error.blob_id() else {
            error!("Error while sampling: {error}");
            return None;
        };

        error_with_id!(blob_id, "SamplingError");

        match error {
            SamplingError::BlobNotFound { .. } => {
                sampler.handle_sampling_error(*blob_id).await;
            }
            SamplingError::MismatchSession { blob_id, session } => {
                return Self::handle_sampling_session_mismatch(
                    blob_id,
                    session,
                    sampler,
                    network_adapter,
                    verifier,
                )
                .await
                .map(|(task, continuation)| (task, Either::Left(continuation)));
            }
            SamplingError::CommitmentsMismatchSession { session, blob_id } => {
                return Self::handle_commitments_session_mismatch(
                    blob_id,
                    session,
                    network_adapter,
                )
                .await
                .map(|(task, continuation)| (task, Either::Right(continuation)));
            }
            _ => {
                error!("Error while sampling: {error}");
            }
        }

        None
    }

    async fn handle_commitments_session_mismatch(
        blob_id: BlobId,
        session: SessionNumber,
        network_adapter: &SamplingNetwork,
    ) -> Option<(LongTask, HistoricCommitmentsFallbackTask)> {
        tracing::warn!("Commitments session mismatch for {blob_id:?}, falling back to historic");

        let (tx, rx) = oneshot::channel();

        let future = (Self::request_historic_commitments_fallback(
            network_adapter,
            HeaderId::from(blob_id),
            blob_id,
            session,
            tx,
            HISTORICAL_SAMPLING_TIMEOUT,
        )
        .await)?;

        let continuation = async move {
            let result = rx.await.unwrap_or(None);
            (blob_id, result)
        }
        .boxed();

        Some((future, continuation))
    }

    async fn handle_sampling_session_mismatch(
        blob_id: BlobId,
        session: SessionNumber,
        sampler: &mut SamplingBackend,
        network_adapter: &SamplingNetwork,
        verifier: &ShareVerifier,
    ) -> Option<(LongTask, HistoricSamplingFallbackTask)> {
        tracing::warn!(
            "Session mismatch for {blob_id:?} and session {session:?}, falling back to historic sampling"
        );

        let (tx, rx) = oneshot::channel();

        let Some(future) = Self::request_historic_sampling_fallback(
            network_adapter,
            verifier,
            HeaderId::from(blob_id),
            blob_id,
            session,
            tx,
            HISTORICAL_SAMPLING_TIMEOUT,
        )
        .await
        else {
            sampler.handle_sampling_error(blob_id).await;
            return None;
        };

        let continuation = async move {
            let result = rx.await.unwrap_or(None);
            (blob_id, result)
        }
        .boxed();

        Some((future, continuation))
    }

    async fn handle_request(
        storage_adapter: &SamplingStorage,
        blob_id: BlobId,
        share_idx: ShareIndex,
        response_sender: mpsc::Sender<Option<DaLightShare>>,
    ) {
        info_with_id!(blob_id, "SamplingRequest");
        let maybe_share = storage_adapter
            .get_light_share(blob_id, share_idx.to_le_bytes())
            .await
            .map_err(|error| {
                error!("Failed to get share from storage adapter: {error}");
            })
            .ok()
            .flatten();

        if response_sender.send(maybe_share).await.is_err() {
            error!("Error sending sampling response");
        }
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "nested error check when writing into response sender"
    )]
    async fn handle_commitments_message(
        storage_adapter: &SamplingStorage,
        commitments_message: CommitmentsEvent,
    ) {
        match commitments_message {
            CommitmentsEvent::CommitmentsSuccess { .. } => {
                // Handled on demand with `wait_commitments`, this stream
                // handler ignores such messages.
            }
            CommitmentsEvent::CommitmentsRequest {
                blob_id,
                response_sender,
            } => {
                if let Ok(commitments) = storage_adapter.get_commitments(blob_id).await
                    && let Err(err) = response_sender.send(commitments).await
                {
                    tracing::error!("Couldn't send commitments response: {err:?}");
                }
            }
            CommitmentsEvent::CommitmentsError { error } => match error.blob_id() {
                Some(blob_id) => {
                    error_with_id!(blob_id, "Commitments response error: {error}");
                }
                None => {
                    tracing::error!("Commitments response error: {error}");
                }
            },
        }
    }

    fn handle_incoming_blob(
        blob: Blob,
        sdp_blob_trigger_sampling_delay: Duration,
        tasks: &PendingTasks<'_>,
    ) {
        // Trigger sampling after delay
        let delayed_future = async move {
            tokio::time::sleep(sdp_blob_trigger_sampling_delay).await;
            blob
        }
        .boxed();

        tasks.delayed_sdp_sampling_triggers.push(delayed_future);
    }

    async fn request_commitments_from_network(
        network_adapter: &SamplingNetwork,
        wait_duration: Duration,
        blob_id: BlobId,
        session: SessionNumber,
        result_sender: oneshot::Sender<Option<DaSharesCommitments>>,
    ) -> Option<LongTask> {
        let Ok(commitments_stream) = network_adapter.listen_to_commitments_messages().await else {
            tracing::error!("Error subscribing to commitments stream");
            drop(result_sender.send(None));
            return None;
        };

        let Ok(historic_commitments_stream) =
            network_adapter.listen_to_historic_sampling_messages().await
        else {
            tracing::error!("Error subscribing to commitments stream");
            drop(result_sender.send(None));
            return None;
        };

        if network_adapter
            .request_commitments(blob_id, session)
            .await
            .is_err()
        {
            drop(result_sender.send(None));
            return None;
        }

        let future = async move {
            let result = tokio::time::timeout(wait_duration, async move {
                let mut commits_stream = commitments_stream;
                let mut historic_stream = historic_commitments_stream;
                loop {
                    tokio::select! {
                        Some(message) = commits_stream.next() => {
                            if let CommitmentsEvent::CommitmentsSuccess {
                                blob_id: received_blob_id,
                                commitments,
                            } = message
                                && received_blob_id == blob_id
                            {
                                return Some(*commitments);
                            }
                        }
                        Some(event) = historic_stream.next() => {
                            if let HistoricSamplingEvent::CommitmentsSuccess {
                                blob_id: received_blob_id,
                                commitments,
                                ..
                            } = event
                                && received_blob_id == blob_id
                            {
                                return Some(commitments);
                            }
                        }
                        else => break,
                    }
                }
                None // streams closed without finding match
            })
            .await;

            // result is Result<Option<Commitments>, Elapsed>
            // Ok(Some(...)) = found, Ok(None) = streams closed, Err(_) = timeout
            let commitments = result.ok().flatten();
            drop(result_sender.send(commitments));
        }
        .boxed();

        Some(future)
    }

    async fn request_commitments(
        storage_adapter: &SamplingStorage,
        network_adapter: &SamplingNetwork,
        wait_duration: Duration,
        blob_id: BlobId,
        session: SessionNumber,
        result_sender: oneshot::Sender<Option<DaSharesCommitments>>,
    ) -> Option<LongTask> {
        if let Ok(Some(commitments)) = storage_adapter.get_commitments(blob_id).await {
            drop(result_sender.send(Some(commitments)));
            return None;
        }

        Self::request_commitments_from_network(
            network_adapter,
            wait_duration,
            blob_id,
            session,
            result_sender,
        )
        .await
    }

    async fn wait_and_verify_historic_response(
        mut stream: impl Stream<Item = HistoricSamplingEvent> + Send + Unpin,
        timeout: Duration,
        target_block_id: HeaderId,
        expected_blob_ids: HashSet<BlobId>,
        verifier: ShareVerifier,
    ) -> Option<(
        HashMap<BlobId, Vec<DaLightShare>>,
        HashMap<BlobId, DaSharesCommitments>,
    )> {
        tokio::time::timeout(timeout, async move {
            while let Some(event) = stream.next().await {
                match event {
                    HistoricSamplingEvent::SamplingSuccess {
                        block_id,
                        shares,
                        commitments,
                    } if block_id == target_block_id => {
                        if Self::verify_historic_sampling(
                            &expected_blob_ids,
                            &shares,
                            &commitments,
                            &verifier,
                        ) {
                            return Some((shares, commitments));
                        }
                        return None;
                    }
                    HistoricSamplingEvent::SamplingError { block_id, .. }
                        if block_id == target_block_id =>
                    {
                        return None;
                    }
                    _ => (),
                }
            }
            None
        })
        .await
        .unwrap_or(None)
    }

    async fn request_historic_sampling_fallback(
        network_adapter: &SamplingNetwork,
        verifier: &ShareVerifier,
        block_id: HeaderId,
        blob_id: BlobId,
        session: SessionNumber,
        result_sender: oneshot::Sender<Option<(Vec<DaLightShare>, DaSharesCommitments)>>,
        timeout: Duration,
    ) -> Option<LongTask> {
        let mut blob_ids = HashMap::new();
        blob_ids.insert(blob_id, session);
        let blobs: HashSet<BlobId> = [blob_id].into();

        let Ok(historic_stream) = network_adapter.listen_to_historic_sampling_messages().await
        else {
            drop(result_sender.send(None));
            return None;
        };

        if let Err(error) = network_adapter
            .request_historic_sampling(block_id, blob_ids)
            .await
        {
            drop(result_sender.send(None));
            error_with_id!(
                blob_id,
                "Request historic sampling fallback failed: {error}"
            );
            return None;
        }

        let verifier = verifier.clone();
        Some(
            async move {
                let result = Self::wait_and_verify_historic_response(
                    historic_stream,
                    timeout,
                    block_id,
                    blobs,
                    verifier,
                )
                .await
                .and_then(|(shares, commitments)| {
                    // Extract single blob data
                    let blob_shares = shares.get(&blob_id)?.clone();
                    let blob_commitments = commitments.get(&blob_id)?.clone();
                    Some((blob_shares, blob_commitments))
                });

                drop(result_sender.send(result));
            }
            .boxed(),
        )
    }

    async fn request_historic_commitments_fallback(
        network_adapter: &SamplingNetwork,
        block_id: HeaderId,
        blob_id: BlobId,
        session: SessionNumber,
        result_sender: oneshot::Sender<Option<DaSharesCommitments>>,
        timeout: Duration,
    ) -> Option<LongTask> {
        // Get the historic commitments stream
        let Ok(historic_commitments_stream) =
            network_adapter.listen_to_historic_sampling_messages().await
        else {
            drop(result_sender.send(None));
            return None;
        };

        if let Err(error) = network_adapter
            .request_historic_commitments(block_id, blob_id, session)
            .await
        {
            drop(result_sender.send(None));
            error_with_id!(
                blob_id,
                "Request historic commitments fallback failed: {error}"
            );
            return None;
        }

        Some(
            async move {
                let result = tokio::time::timeout(timeout, async move {
                    let mut stream = historic_commitments_stream;
                    while let Some(event) = stream.next().await {
                        match event {
                            HistoricSamplingEvent::CommitmentsSuccess {
                                block_id: received_block_id,
                                blob_id: received_blob_id,
                                commitments,
                            } if received_block_id == block_id => {
                                if received_blob_id == blob_id {
                                    return Some(commitments);
                                }
                                error_with_id!(
                                    blob_id,
                                    "Historic Commitments: received wrong blob_id: {received_block_id}"
                                );
                                return None;
                            }
                            _ => {}
                        }
                    }
                    None
                })
                .await
                .unwrap_or(None);

                drop(result_sender.send(result));
            }
            .boxed(),
        )
    }

    async fn request_and_wait_historic_sampling(
        network_adapter: &SamplingNetwork,
        verifier: &ShareVerifier,
        block_id: HeaderId,
        blob_ids: HashMap<BlobId, SessionNumber>,
        reply_channel: oneshot::Sender<bool>,
        timeout: Duration,
    ) -> Option<LongTask> {
        let blobs: HashSet<BlobId> = blob_ids.keys().copied().collect();
        let Ok(historic_stream) = network_adapter.listen_to_historic_sampling_messages().await
        else {
            if let Err(e) = reply_channel.send(false) {
                tracing::error!("Failed to send historic sampling response: {}", e);
            }
            return None;
        };

        if network_adapter
            .request_historic_sampling(block_id, blob_ids)
            .await
            .is_err()
        {
            if let Err(e) = reply_channel.send(false) {
                tracing::error!("Failed to send historic sampling response: {}", e);
            }
            return None;
        }

        let verifier = verifier.clone();
        Some(
            async move {
                let result = Self::wait_and_verify_historic_response(
                    historic_stream,
                    timeout,
                    block_id,
                    blobs,
                    verifier,
                )
                .await
                .is_some();

                if let Err(e) = reply_channel.send(result) {
                    tracing::error!("Failed to send historic sampling result: {}", e);
                }
            }
            .boxed(),
        )
    }

    #[inline]
    fn verify_historic_sampling(
        expected_blob_ids: &HashSet<BlobId>,
        shares: &HashMap<BlobId, Vec<DaLightShare>>,
        commitments: &HashMap<BlobId, DaSharesCommitments>,
        verifier: &ShareVerifier,
    ) -> bool {
        // Check counts match
        if shares.len() != expected_blob_ids.len() || commitments.len() != expected_blob_ids.len() {
            return false;
        }

        // Check all expected blobs are present
        if !expected_blob_ids
            .iter()
            .all(|b| shares.contains_key(b) && commitments.contains_key(b))
        {
            return false;
        }

        // Verify all shares
        // TODO: maybe spawn blocking so it yields while it verifies on a separate
        // thread
        for (blob_id, blob_shares) in shares {
            let Some(blob_commitments) = commitments.get(blob_id) else {
                return false;
            };

            for share in blob_shares {
                if verifier.verify(blob_commitments, share).is_err() {
                    return false;
                }
            }
        }

        true
    }
}

impl<
    SamplingBackend,
    SamplingNetwork,
    SamplingStorage,
    ShareVerifier,
    MempoolAdapter,
    RuntimeServiceId,
> ServiceData
    for GenericDaSamplingService<
        SamplingBackend,
        SamplingNetwork,
        SamplingStorage,
        ShareVerifier,
        MempoolAdapter,
        RuntimeServiceId,
    >
where
    SamplingBackend: DaSamplingServiceBackend,
    SamplingNetwork: NetworkAdapter<RuntimeServiceId>,
    SamplingStorage: DaStorageAdapter<RuntimeServiceId>,
    MempoolAdapter: DaMempoolAdapter,
    ShareVerifier: VerifierBackend,
{
    type Settings = DaSamplingServiceSettings<SamplingBackend::Settings, ShareVerifier::Settings>;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = DaSamplingServiceMsg<SamplingBackend::BlobId>;
}

#[async_trait::async_trait]
impl<
    SamplingBackend,
    SamplingNetwork,
    SamplingStorage,
    ShareVerifier,
    MempoolAdapter,
    RuntimeServiceId,
> ServiceCore<RuntimeServiceId>
    for GenericDaSamplingService<
        SamplingBackend,
        SamplingNetwork,
        SamplingStorage,
        ShareVerifier,
        MempoolAdapter,
        RuntimeServiceId,
    >
where
    SamplingBackend: DaSamplingServiceBackend<
            BlobId = BlobId,
            Share = DaShare,
            SharesCommitments = DaSharesCommitments,
        > + Send,
    SamplingBackend::Settings: Clone + Send + Sync,
    SamplingNetwork: NetworkAdapter<RuntimeServiceId> + Send + Sync + 'static,
    SamplingNetwork::Settings: Send + Sync,
    SamplingNetwork::Membership: MembershipHandler + Clone + 'static,
    SamplingStorage: DaStorageAdapter<RuntimeServiceId, Share = DaShare> + Send + Sync,
    MempoolAdapter: DaMempoolAdapter<Tx = SignedMantleTx> + Send + Sync + 'static,
    ShareVerifier: VerifierBackend<DaShare = DaShare> + Send + Sync + Clone + 'static,
    ShareVerifier::Settings: Clone + Send + Sync,
    RuntimeServiceId: AsServiceId<Self>
        + AsServiceId<
            NetworkService<
                SamplingNetwork::Backend,
                SamplingNetwork::Membership,
                SamplingNetwork::MembershipAdapter,
                SamplingNetwork::Storage,
                SamplingNetwork::ApiAdapter,
                SamplingNetwork::SdpAdapter,
                RuntimeServiceId,
            >,
        > + AsServiceId<StorageService<SamplingStorage::Backend, RuntimeServiceId>>
        + AsServiceId<MempoolAdapter::MempoolService>
        + Debug
        + Display
        + Sync
        + Send
        + 'static,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, DynError> {
        Ok(Self::new(service_resources_handle))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "this function has a lot of cases to handle"
    )]
    async fn run(mut self) -> Result<(), DynError> {
        let Self {
            mut service_resources_handle,
            ..
        } = self;
        let DaSamplingServiceSettings {
            sampling_settings,
            share_verifier_settings,
            commitments_wait_duration,
            sdp_blob_trigger_sampling_delay,
        } = service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();

        let network_relay = service_resources_handle
            .overwatch_handle
            .relay::<NetworkService<_, _, _, _, _, _, _>>()
            .await?;
        let mut network_adapter = SamplingNetwork::new(network_relay).await;
        let mut sampling_message_stream = network_adapter.listen_to_sampling_messages().await?;
        let mut commitments_message_stream =
            network_adapter.listen_to_commitments_messages().await?;

        let storage_relay = service_resources_handle
            .overwatch_handle
            .relay::<StorageService<_, _>>()
            .await?;
        let storage_adapter = SamplingStorage::new(storage_relay).await;

        let mempool_relay = service_resources_handle
            .overwatch_handle
            .relay::<MempoolAdapter::MempoolService>()
            .await?;
        let mempool_adapter = MempoolAdapter::new(mempool_relay);

        let mut sampler = SamplingBackend::new(sampling_settings);
        let share_verifier = ShareVerifier::new(share_verifier_settings);
        let mut next_prune_tick = sampler.prune_interval();

        service_resources_handle.status_updater.notify_ready();
        tracing::info!(
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        wait_until_services_are_ready!(
            &service_resources_handle.overwatch_handle,
            Some(Duration::from_secs(60)),
            NetworkService<_, _, _, _,_, _, _>,
            StorageService<_, _>,
            MempoolAdapter::MempoolService
        )
        .await?;

        let mut blob_stream = mempool_adapter.subscribe().await?;

        let mut long_tasks: FuturesUnordered<BoxFuture<'static, ()>> = FuturesUnordered::new();
        let mut delayed_sdp_sampling_triggers: FuturesUnordered<BoxFuture<'static, Blob>> =
            FuturesUnordered::new();
        let mut sampling_continuations: FuturesUnordered<SamplingContinuationTask> =
            FuturesUnordered::new();
        let mut historic_fallback_continuations: FuturesUnordered<HistoricSamplingFallbackTask> =
            FuturesUnordered::new();
        let mut historic_commitments_fallback_continuations: FuturesUnordered<
            HistoricCommitmentsFallbackTask,
        > = FuturesUnordered::new();

        let pending_tasks = &mut PendingTasks {
            long_tasks: &mut long_tasks,
            sampling_continuations: &mut sampling_continuations,
            delayed_sdp_sampling_triggers: &mut delayed_sdp_sampling_triggers,
            historic_fallback_continuations: &mut historic_fallback_continuations,
            historic_commitments_continuations: &mut historic_commitments_fallback_continuations,
        };

        loop {
            tokio::select! {
                        Some(service_message) = service_resources_handle.inbound_relay.recv() => {
                            Self::handle_service_message(
                                service_message,
                                &mut network_adapter,
                                &storage_adapter,
                                &mut sampler,
                                commitments_wait_duration,
                                &share_verifier,
                                pending_tasks,
                            ).await;
                        }
                        Some(sampling_message) = sampling_message_stream.next() => {
                                if let Some((future, continuation)) = Self::handle_sampling_message(
                                    sampling_message,
                                    &mut sampler,
                                    &storage_adapter,
                                    &share_verifier,
                                    &network_adapter,
                                ).await {
                                    pending_tasks.long_tasks.push(future);
                                    continuation.either(
                                            |sampling| pending_tasks.historic_fallback_continuations.push(sampling),
                                            |commitments| pending_tasks.historic_commitments_continuations.push(commitments),
            );
                                }
                            }
                        Some(commitments_message) = commitments_message_stream.next() => {
                            Self::handle_commitments_message(
                                &storage_adapter,
                                commitments_message
                            ).await;
                        }
                        // Handle completed sampling continuations
                        Some((blob, commitments)) = pending_tasks.sampling_continuations.next() => {
                            if let Some(commitments) = commitments {
                                info_with_id!(blob.blob_id, "Got commitments for triggered sampling");
                                sampler.add_commitments(&blob.blob_id, commitments);

                                if let Err(e) = network_adapter.start_sampling(blob.blob_id, blob.session).await {
                                    sampler.handle_sampling_error(blob.blob_id).await;
                                    error_with_id!(blob.blob_id, "Error starting sampling: {e}");
                                }
                            } else {
                                error_with_id!(blob.blob_id, "Failed to get commitments for triggered sampling");
                                sampler.handle_sampling_error(blob.blob_id).await;
                            }
                        }

                        Some(blob) = blob_stream.next() => {
                            Self::handle_incoming_blob(
                                blob,
                                sdp_blob_trigger_sampling_delay,
                                pending_tasks,
                            );
                        }
                        Some(blob) = pending_tasks.delayed_sdp_sampling_triggers.next() => {
                            Self::handle_service_message(
                                DaSamplingServiceMsg::TriggerSampling { blob_id: blob.blob_id, session: blob.session },
                                &mut network_adapter,
                                &storage_adapter,
                                &mut sampler,
                                commitments_wait_duration,
                                &share_verifier,
                                pending_tasks,
                            ).await;
                        }
                        // Process completed long tasks (they just run to completion)
                        Some(()) = pending_tasks.long_tasks.next() => {}

                        Some((blob_id, maybe_result)) = pending_tasks.historic_fallback_continuations.next() => {
                            if let Some((shares, commitments)) = maybe_result {
                                info_with_id!(blob_id, "Historic sampling fallback succeeded");
                                sampler.add_commitments(&blob_id, commitments);

                                for share in shares {
                                    sampler.handle_sampling_success(blob_id, share.share_idx).await;
                                }
                            } else {
                                error_with_id!(blob_id, "Historic sampling fallback failed");
                                sampler.handle_sampling_error(blob_id).await;
                            }
                        }

                        Some((blob_id, maybe_commitments)) = pending_tasks.historic_commitments_continuations.next() => {
                            if let Some(commitments) = maybe_commitments {
                                info_with_id!(blob_id, "Historic commitments fallback succeeded");
                                sampler.add_commitments(&blob_id, commitments);
                            }
                        }

                        // cleanup not on time samples
                        _ = next_prune_tick.tick() => {
                            sampler.prune();
                        }
                }
        }
    }
}
