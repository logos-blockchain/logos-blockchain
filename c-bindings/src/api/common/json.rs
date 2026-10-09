use std::ffi::{CString, c_char};

use serde::Serialize;

use crate::{OperationStatus, OperationStatusCode, StatusResult};

pub fn into_json_c_string(value: &impl Serialize) -> StatusResult<*mut c_char> {
    let json = serde_json::to_string(value).map_err(|error| {
        OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to serialize to JSON: {error}"),
        )
    })?;
    CString::new(json).map(CString::into_raw).map_err(|error| {
        OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to create CString: {error}"),
        )
    })
}
