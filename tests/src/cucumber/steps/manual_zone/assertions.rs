use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use futures::StreamExt as _;
use lb_common_http_client::Slot;
use lb_core::mantle::ops::channel::MsgId;
use lb_zone_sdk::{
    ZoneMessage, adapter::NodeHttpClient as ZoneNodeHttpClient, indexer::ZoneIndexer,
};

use super::support::{DiscardedPayloads, ZoneTestError};
use crate::cucumber::error::{StepError, StepResult};

pub(super) async fn wait_for_indexer_unordered(
    indexer: &ZoneIndexer<ZoneNodeHttpClient>,
    expected: &HashSet<Vec<u8>>,
    timeout_duration: Duration,
) -> Result<HashSet<Vec<u8>>, ZoneTestError> {
    let mut seen = HashSet::new();
    let mut cursor = None;
    let start = Instant::now();

    loop {
        if start.elapsed() > timeout_duration {
            return Err(ZoneTestError::IndexerTimeout);
        }

        read_next_indexed_blocks(indexer, &mut cursor, |payload| {
            if expected.contains(&payload) {
                seen.insert(payload);
            }
        })
        .await?;

        if seen == *expected {
            return Ok(seen);
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub(super) async fn scan_indexer_for_payloads(
    indexer: &ZoneIndexer<ZoneNodeHttpClient>,
    expected: &HashSet<Vec<u8>>,
) -> Result<Vec<Vec<u8>>, ZoneTestError> {
    let mut payloads = Vec::new();
    let mut cursor = None;

    loop {
        let saw_message = read_next_indexed_blocks(indexer, &mut cursor, |payload| {
            if expected.contains(&payload) {
                payloads.push(payload);
            }
        })
        .await?;

        if !saw_message {
            return Ok(payloads);
        }
    }
}

pub(super) async fn wait_until_sorted_conflict_settles(
    indexer: &ZoneIndexer<ZoneNodeHttpClient>,
    expected: &HashSet<Vec<u8>>,
    discarded: &DiscardedPayloads,
    total: usize,
    timeout_duration: Duration,
) -> Result<Vec<Vec<u8>>, ZoneTestError> {
    let mut on_chain = Vec::new();
    let mut cursor = None;
    let start = Instant::now();

    loop {
        if start.elapsed() > timeout_duration {
            return Err(ZoneTestError::IndexerTimeout);
        }

        let expected_count = total.saturating_sub(discarded.lock().await.len());
        if expected_count > 0 && on_chain.len() >= expected_count {
            return Ok(on_chain);
        }

        read_next_indexed_blocks(indexer, &mut cursor, |payload| {
            if expected.contains(&payload) && !on_chain.contains(&payload) {
                on_chain.push(payload);
            }
        })
        .await?;

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn read_next_indexed_blocks(
    indexer: &ZoneIndexer<ZoneNodeHttpClient>,
    cursor: &mut Option<(MsgId, Slot)>,
    mut visit: impl FnMut(Vec<u8>),
) -> Result<bool, ZoneTestError> {
    let stream = indexer
        .next_messages(*cursor)
        .await
        .map_err(|error| ZoneTestError::Indexer {
            message: error.to_string(),
        })?;
    futures::pin_mut!(stream);

    let mut saw_message = false;

    while let Some((message, slot)) = stream.next().await {
        let ZoneMessage::Block(block) = message else {
            continue;
        };

        saw_message = true;
        *cursor = Some((block.id, slot));
        visit(block.data);
    }

    Ok(saw_message)
}

pub(super) fn assert_sorted_outcome(
    on_chain: &[Vec<u8>],
    discarded: &HashSet<Vec<u8>>,
    total: usize,
) -> StepResult {
    let issues = sorted_outcome_issues(on_chain, discarded, total);
    if issues.is_empty() {
        return Ok(());
    }

    Err(StepError::LogicalError {
        message: issues.join("; "),
    })
}

fn sorted_outcome_issues(
    on_chain: &[Vec<u8>],
    discarded: &HashSet<Vec<u8>>,
    total: usize,
) -> Vec<String> {
    let mut issues = Vec::new();
    let unique: HashSet<&Vec<u8>> = on_chain.iter().collect();
    if unique.len() != on_chain.len() {
        issues.push("Duplicate inscriptions detected on chain".to_owned());
    }

    if !on_chain.windows(2).all(|window| window[0] <= window[1]) {
        issues.push(format!(
            "On-chain payloads are not sorted: {:?}",
            render_payloads(on_chain)
        ));
    }

    let on_chain_set: HashSet<Vec<u8>> = on_chain.iter().cloned().collect();
    let overlap: Vec<Vec<u8>> = on_chain_set.intersection(discarded).cloned().collect();
    if !overlap.is_empty() {
        issues.push(format!(
            "Payloads appeared both on-chain and discarded: {:?}",
            render_payloads(&overlap)
        ));
    }

    if on_chain.len() + discarded.len() != total {
        issues.push(format!(
            "sorted conflict accounting mismatch: on_chain={} discarded={} total={total}",
            on_chain.len(),
            discarded.len()
        ));
    }

    issues
}

fn render_payloads(payloads: &[Vec<u8>]) -> Vec<String> {
    payloads
        .iter()
        .map(|payload| String::from_utf8_lossy(payload).to_string())
        .collect()
}
