use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use lb_core::mantle::ops::channel::inscribe::Inscription;

use super::support::{DiscardedPayloads, ZoneTestError, indexed_inscription_payloads};
use crate::cucumber::{
    error::{StepError, StepResult},
    world::ZoneReaderConfig,
};

pub(super) async fn wait_for_indexer_unordered(
    reader: &ZoneReaderConfig,
    expected: &HashSet<Inscription>,
    timeout_duration: Duration,
) -> Result<HashSet<Inscription>, ZoneTestError> {
    let start = Instant::now();

    loop {
        if start.elapsed() > timeout_duration {
            return Err(ZoneTestError::IndexerTimeout);
        }

        let seen: HashSet<Inscription> =
            indexed_inscription_payloads(&reader.finalized_history().await)
                .into_iter()
                .filter(|payload| expected.contains(payload))
                .collect();

        if seen == *expected {
            return Ok(seen);
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub(super) async fn scan_indexer_for_payloads(
    reader: &ZoneReaderConfig,
    expected: &HashSet<Inscription>,
) -> Result<Vec<Inscription>, ZoneTestError> {
    Ok(
        indexed_inscription_payloads(&reader.finalized_history().await)
            .into_iter()
            .filter(|payload| expected.contains(payload))
            .collect(),
    )
}

pub(super) async fn wait_until_sorted_conflict_settles(
    reader: &ZoneReaderConfig,
    expected: &HashSet<Inscription>,
    discarded: &DiscardedPayloads,
    total: usize,
    timeout_duration: Duration,
) -> Result<Vec<Inscription>, ZoneTestError> {
    let start = Instant::now();

    loop {
        if start.elapsed() > timeout_duration {
            return Err(ZoneTestError::IndexerTimeout);
        }

        let mut on_chain: Vec<Inscription> = Vec::new();
        for payload in indexed_inscription_payloads(&reader.finalized_history().await) {
            if expected.contains(&payload) && !on_chain.contains(&payload) {
                on_chain.push(payload);
            }
        }

        let discarded_snapshot = discarded.lock().await.clone();
        let expected_count = total.saturating_sub(discarded_snapshot.len());
        let has_discarded_on_chain = on_chain
            .iter()
            .any(|payload| discarded_snapshot.contains(payload));
        if expected_count > 0 && on_chain.len() >= expected_count && !has_discarded_on_chain {
            return Ok(on_chain);
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
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
