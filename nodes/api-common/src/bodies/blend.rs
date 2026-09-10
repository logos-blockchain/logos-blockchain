use lb_core::{mantle::NoteId, sdp::Locator};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Serialize, Deserialize, ToSchema)]
pub struct JoinBlendRequestBody {
    pub locator: Locator,
    pub service_note_id: NoteId,
}
