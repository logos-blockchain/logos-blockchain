pub use lb_c_common::FfiResult;

use crate::OperationStatus;

pub type StatusResult<Value> = Result<Value, OperationStatus>;
pub type FfiStatusResult<Value> = FfiResult<Value, OperationStatus>;
