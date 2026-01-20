use std::fmt::{Debug, Display};

use axum::{extract::State, response::Response};
use lb_api_service::http::mantle;
use overwatch::{overwatch::OverwatchHandle, services::AsServiceId};

use super::backend::TestHttpCryptarchiaService;
use crate::{
    generic_services::{SdpService, TxMempoolService},
    make_request_and_return_response,
};

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
