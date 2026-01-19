use std::{
    fmt::{Debug, Display},
    hash::Hash,
};

use axum::{Json, extract::State, response::Response};
use lb_api_service::http::{da, mantle};
use lb_core::{header::HeaderId, sdp::SessionNumber};
use lb_da_network_service::{
    NetworkService, api::ApiAdapter as ApiAdapterTrait, backends::NetworkBackend,
    sdp::SdpAdapter as SdpAdapterTrait,
};
use lb_da_sampling_service::{
    DaSamplingService, backend::DaSamplingServiceBackend, mempool::DaMempoolAdapter,
};
use lb_subnetworks_assignations::MembershipHandler;
use overwatch::{overwatch::OverwatchHandle, services::AsServiceId};
use serde::{Deserialize, Serialize};

use super::backend::TestHttpCryptarchiaService;
use crate::{
    generic_services::{SdpService, TxMempoolService},
    make_request_and_return_response,
};

pub async fn da_get_membership<
    Backend,
    Membership,
    MembershipAdapter,
    MembershipStorage,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(session_id): Json<SessionNumber>,
) -> Response
where
    Backend: NetworkBackend<RuntimeServiceId> + Send + 'static,
    Membership: MembershipHandler + Clone + Send + Sync + 'static,
    Membership::Id: Send + Sync + 'static,
    Membership::NetworkId: Send + Sync + 'static,
    ApiAdapter: ApiAdapterTrait + Send + Sync + 'static,
    SdpAdapter: SdpAdapterTrait<RuntimeServiceId> + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + 'static
        + AsServiceId<
            NetworkService<
                Backend,
                Membership,
                MembershipAdapter,
                MembershipStorage,
                ApiAdapter,
                SdpAdapter,
                RuntimeServiceId,
            >,
        >,
{
    make_request_and_return_response!(da::da_get_membership::<
        Backend,
        Membership,
        MembershipAdapter,
        MembershipStorage,
        ApiAdapter,
        SdpAdapter,
        RuntimeServiceId,
    >(handle, session_id))
}

#[derive(Serialize, Deserialize)]
pub struct HistoricSamplingRequest<BlobId>
where
    BlobId: Eq + Hash,
{
    pub block_id: HeaderId,
    pub blob_ids: Vec<(BlobId, SessionNumber)>,
}

pub async fn da_historic_sampling<
    SamplingBackend,
    SamplingNetwork,
    SamplingStorage,
    SamplingMempoolAdapter,
    RuntimeServiceId,
>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(request): Json<HistoricSamplingRequest<SamplingBackend::BlobId>>,
) -> Response
where
    SamplingBackend: DaSamplingServiceBackend,
    <SamplingBackend as DaSamplingServiceBackend>::BlobId: Send + Eq + Hash + 'static,
    SamplingNetwork: lb_da_sampling_service::network::NetworkAdapter<RuntimeServiceId>,
    SamplingStorage: lb_da_sampling_service::storage::DaStorageAdapter<RuntimeServiceId>,
    SamplingMempoolAdapter: DaMempoolAdapter,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + AsServiceId<
            DaSamplingService<
                SamplingBackend,
                SamplingNetwork,
                SamplingStorage,
                SamplingMempoolAdapter,
                RuntimeServiceId,
            >,
        >,
{
    make_request_and_return_response!(da::da_historic_sampling::<
        SamplingBackend,
        SamplingNetwork,
        SamplingStorage,
        SamplingMempoolAdapter,
        RuntimeServiceId,
    >(
        handle,
        request.block_id,
        request.blob_ids.into_iter().collect()
    ))
}

pub async fn get_sdp_declarations<RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<TestHttpCryptarchiaService<RuntimeServiceId>>
        + AsServiceId<SdpService<RuntimeServiceId>>
        + AsServiceId<TxMempoolService<RuntimeServiceId>>,
{
    make_request_and_return_response!(mantle::get_sdp_declarations::<RuntimeServiceId>(&handle))
}
