use std::ffi::{CStr, CString, c_char};

use lb_c_macros::panic_to_error;
use lb_http_api_common::bodies::mantle::{
    LimitsResponseBody, MantleTxFeeRequestBody, MantleTxFeeResponseBody,
};
use lb_node::RuntimeServiceId;

use crate::{
    LogosBlockchainNode,
    errors::{OperationStatus, OperationStatusCode},
    result::{FfiStatusResult, StatusResult},
    return_error_if_null_pointer, unwrap_or_return_error,
};

/// What a transaction costs once funded.
///
/// This is a synchronous wrapper that mirrors the node's `POST /mantle/tx-fee`
/// HTTP handler: it prices the request's transaction builder against the
/// ledger state at the request's tip, or the current one, under the gas
/// profile of its era.
pub(crate) fn mantle_tx_fee_sync(
    node: &LogosBlockchainNode,
    request: &MantleTxFeeRequestBody,
) -> StatusResult<MantleTxFeeResponseBody> {
    let runtime_handle = node.get_runtime_handle()?;
    runtime_handle.block_on(async {
        let priced = lb_api_service::http::mantle::tx_fee::<RuntimeServiceId>(
            node.get_overwatch_handle(),
            request.tip,
            &request.tx_builder,
        )
        .await
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::DynError,
                format!("Failed to price the transaction: {error}"),
            )
        })?;
        Ok(MantleTxFeeResponseBody {
            tip: priced.tip,
            tx_fee: priced.fee,
        })
    })
}

pub type FfiMantleTxFeeResult = FfiStatusResult<*mut c_char>;

/// What a transaction costs once funded.
///
/// The request and response are JSON strings with the exact same schemas as
/// the node's `POST /mantle/tx-fee` HTTP request and response bodies. The
/// response carries the mandatory fee, before any priority reserve, of the
/// builder as it stands, or with one funding input and a change note when it
/// has no input.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
/// - `request_json`: A non-null, NUL-terminated JSON encoding of the request.
///
/// # Returns
///
/// A [`FfiMantleTxFeeResult`] containing the JSON-encoded response on
/// success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all pointers are valid.
///
/// # Memory Management
///
/// This function allocates the returned C string. The caller must free it
/// using the [`free_cstring`](crate::api::free_cstring) function.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mantle_tx_fee(
    node: *const LogosBlockchainNode,
    request_json: *const c_char,
) -> FfiMantleTxFeeResult {
    return_error_if_null_pointer!(node);
    return_error_if_null_pointer!(request_json);
    let node = unsafe { &*node };

    let request_json = match unsafe { CStr::from_ptr(request_json) }.to_str() {
        Ok(request_json) => request_json,
        Err(error) => {
            return FfiMantleTxFeeResult::err(OperationStatus::error(
                OperationStatusCode::ValidationError,
                format!("Request is not valid UTF-8: {error}"),
            ));
        }
    };
    let request: MantleTxFeeRequestBody = match serde_json::from_str(request_json) {
        Ok(request) => request,
        Err(error) => {
            return FfiMantleTxFeeResult::err(OperationStatus::error(
                OperationStatusCode::ValidationError,
                format!("Failed to parse tx fee request: {error}"),
            ));
        }
    };

    let response = unwrap_or_return_error!(mantle_tx_fee_sync(node, &request));

    let response_json = match serde_json::to_string(&response) {
        Ok(response_json) => response_json,
        Err(error) => {
            return FfiMantleTxFeeResult::err(OperationStatus::error(
                OperationStatusCode::RuntimeError,
                format!("Failed to serialize tx fee response: {error}"),
            ));
        }
    };
    match CString::new(response_json) {
        Ok(response_json) => FfiMantleTxFeeResult::ok(response_json.into_raw()),
        Err(error) => FfiMantleTxFeeResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to create response CString: {error}"),
        )),
    }
}

/// What a transaction must fit in the era of the current tip, and what its
/// Operations cost there.
///
/// This is a synchronous wrapper that mirrors the node's `GET /mantle/limits`
/// HTTP handler.
pub(crate) fn mantle_limits_sync(node: &LogosBlockchainNode) -> StatusResult<LimitsResponseBody> {
    let runtime_handle = node.get_runtime_handle()?;
    runtime_handle.block_on(async {
        let config = lb_api_service::http::mantle::ledger_config_at_tip::<RuntimeServiceId>(
            node.get_overwatch_handle(),
        )
        .await
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::DynError,
                format!("Failed to read the era's ledger config: {error}"),
            )
        })?;
        Ok(LimitsResponseBody {
            max_op_data_size: config.max_op_data_size,
            max_transactions_size: config.max_transactions_size,
            max_gas_per_block: config.execution_market.max_gas_per_block.into(),
            gas_profile: config.gas_profile,
        })
    })
}

pub type FfiMantleLimitsResult = FfiStatusResult<*mut c_char>;

/// What a transaction must fit in the era of the current tip, and what its
/// Operations cost there.
///
/// The response is a JSON string with the exact same schema as the node's
/// `GET /mantle/limits` HTTP response body: the most bytes of data an
/// Operation may carry, the most bytes a block's transactions may take, the
/// most execution gas a block may consume, and the era's gas profile.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A [`FfiMantleLimitsResult`] containing the JSON-encoded response on
/// success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that `node` is non-null and points to a valid
/// [`LogosBlockchainNode`].
///
/// # Memory Management
///
/// This function allocates the returned C string. The caller must free it
/// using the [`free_cstring`](crate::api::free_cstring) function.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mantle_limits(node: *const LogosBlockchainNode) -> FfiMantleLimitsResult {
    return_error_if_null_pointer!(node);
    let node = unsafe { &*node };

    let response = unwrap_or_return_error!(mantle_limits_sync(node));

    let response_json = match serde_json::to_string(&response) {
        Ok(response_json) => response_json,
        Err(error) => {
            return FfiMantleLimitsResult::err(OperationStatus::error(
                OperationStatusCode::RuntimeError,
                format!("Failed to serialize limits response: {error}"),
            ));
        }
    };
    match CString::new(response_json) {
        Ok(response_json) => FfiMantleLimitsResult::ok(response_json.into_raw()),
        Err(error) => FfiMantleLimitsResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to create response CString: {error}"),
        )),
    }
}
