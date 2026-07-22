use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use futures::StreamExt as _;
use lb_common_http_client::Slot;
use lb_core::mantle::ops::channel::inscribe::Inscription;
use lb_zone_sdk::{
    ZoneMessage, adapter::NodeHttpClient as ZoneNodeHttpClient, indexer::ZoneIndexer,
};

use super::support::{DiscardedPayloads, ZoneTestError};
use crate::cucumber::error::{StepError, StepResult};

pub(super) async fn wait_for_indexer_unordered(
    indexer: &ZoneIndexer<ZoneNodeHttpClient>,
    expected: &HashSet<Inscription>,
    timeout_duration: Duration,
) -> Result<HashSet<Inscription>, ZoneTestError> {
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
    expected: &HashSet<Inscription>,
) -> Result<Vec<Inscription>, ZoneTestError> {
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
    expected: &HashSet<Inscription>,
    discarded: &DiscardedPayloads,
    total: usize,
    timeout_duration: Duration,
) -> Result<Vec<Inscription>, ZoneTestError> {
    let mut on_chain = Vec::new();
    let mut cursor = None;
    let start = Instant::now();

    loop {
        if start.elapsed() > timeout_duration {
            return Err(ZoneTestError::IndexerTimeout);
        }

        let discarded_snapshot = discarded.lock().await.clone();
        let expected_count = total.saturating_sub(discarded_snapshot.len());
        let has_discarded_on_chain = on_chain
            .iter()
            .any(|payload| discarded_snapshot.contains(payload));
        if expected_count > 0 && on_chain.len() >= expected_count && !has_discarded_on_chain {
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
    cursor: &mut Option<Slot>,
    mut visit: impl FnMut(Inscription),
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
        saw_message = true;
        *cursor = Some(slot);
        if let ZoneMessage::Block(block) = message {
            visit(block.data);
        }
    }

    Ok(saw_message)
}

pub(super) fn assert_sorted_outcome(
    on_chain: &[Inscription],
    discarded: &HashSet<Inscription>,
    total: usize,
    expected_by_sequencer: &HashMap<String, Vec<Inscription>>,
) -> StepResult {
    let issues = sorted_outcome_issues(on_chain, discarded, total, expected_by_sequencer);
    if issues.is_empty() {
        return Ok(());
    }

    Err(StepError::LogicalError {
        message: issues.join("; "),
    })
}

fn sorted_outcome_issues(
    on_chain: &[Inscription],
    discarded: &HashSet<Inscription>,
    total: usize,
    expected_by_sequencer: &HashMap<String, Vec<Inscription>>,
) -> Vec<String> {
    let mut issues = Vec::new();
    let unique: HashSet<&Inscription> = on_chain.iter().collect();
    if unique.len() != on_chain.len() {
        issues.push("Duplicate inscriptions detected on chain".to_owned());
    }

    let on_chain_set: HashSet<Inscription> = on_chain.iter().cloned().collect();
    let overlap: Vec<Inscription> = on_chain_set.intersection(discarded).cloned().collect();
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

    for (sequencer_alias, expected_payloads) in expected_by_sequencer {
        let surviving = on_chain
            .iter()
            .filter(|payload| expected_payloads.contains(*payload))
            .cloned()
            .collect::<Vec<_>>();

        let mut last_index = None;
        for payload in &surviving {
            let Some(index) = expected_payloads
                .iter()
                .position(|expected| expected == payload)
            else {
                continue;
            };

            if let Some(previous_index) = last_index
                && index <= previous_index
            {
                issues.push(format!(
                    "Per-sequencer order was not preserved for {sequencer_alias}: {:?}",
                    render_payloads(&surviving)
                ));
                break;
            }

            last_index = Some(index);
        }
    }

    issues
}

fn render_payloads(payloads: &[Inscription]) -> Vec<String> {
    payloads
        .iter()
        .map(|payload| String::from_utf8_lossy(payload).to_string())
        .collect()
}
