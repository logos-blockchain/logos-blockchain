use lb_core::{mantle::NoteId, sdp::Locator};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct JoinBlendRequestBody {
    pub locator: Locator,
    pub locked_note_id: NoteId,
}
