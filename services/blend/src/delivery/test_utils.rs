use core::{num::NonZeroU64, time::Duration};

use futures::{Stream, StreamExt as _};
use tokio::time::Instant;

use crate::message::BlendPayload;

pub const ROUND: Duration = Duration::from_secs(1);
pub const DEADLINE: NonZeroU64 = NonZeroU64::new(6).unwrap();

#[must_use]
pub fn proposal() -> BlendPayload {
    BlendPayload::BlockProposal(b"proposal".to_vec())
}

#[must_use]
pub fn transaction() -> BlendPayload {
    BlendPayload::Transaction(b"transaction".to_vec())
}

/// Turns the clock until `round`, collecting whatever expired on the way.
/// Rounds are counted from `start` rather than from now, so a test that
/// calls this several times does not accumulate an offset.
pub async fn until<Detection>(
    detection: &mut Detection,
    start: Instant,
    round: u64,
) -> Vec<BlendPayload>
where
    Detection: Stream<Item = Vec<BlendPayload>> + Unpin,
{
    let stop = start + ROUND * u32::try_from(round).expect("The tests turn few rounds.");
    let mut expired = Vec::new();
    let _timed_out = tokio::time::timeout_at(stop, async {
        while let Some(batch) = detection.next().await {
            expired.extend(batch);
        }
    })
    .await;
    expired
}
