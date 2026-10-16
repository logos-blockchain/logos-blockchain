use lb_core::{mantle::NoteId, sdp::Locator};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Serialize, Deserialize, ToSchema)]
pub struct JoinBlendRequestBody {
    pub locator: Locator,
    /// When omitted, the node picks a note held by its blend ZK key.
    #[serde(default)]
    pub service_note_id: Option<NoteId>,
}
