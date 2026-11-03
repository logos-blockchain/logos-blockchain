use lb_core::{mantle::Value, sdp::Locator};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Serialize, Deserialize, ToSchema)]
pub struct JoinBlendRequestBody {
    pub locator: Locator,
    /// When omitted, the minimum stake.
    #[serde(default)]
    pub amount: Option<Value>,
}
