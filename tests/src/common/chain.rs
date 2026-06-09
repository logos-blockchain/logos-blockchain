use std::{collections::HashSet, hash::BuildHasher, time::Duration};

use lb_common_http_client::ApiBlock;
use lb_core::{
    header::HeaderId,
    mantle::{Transaction as _, TxHash},
};
use lb_testing_framework::NodeHttpClient;
use tokio::time::{sleep, timeout};

/// Walk the chain backwards from `start`, visiting each block exactly once
/// until either `visit_block` breaks or we reach genesis.
pub async fn scan_chain_until<F, Fut, Visit, R, S>(
    start: HeaderId,
    scanned_blocks: &mut HashSet<HeaderId, S>,
    mut get_block: F,
    mut visit_block: Visit,
) -> Option<R>
where
    S: BuildHasher,
    F: FnMut(HeaderId) -> Fut,
    Fut: Future<Output = Option<ApiBlock>>,
    Visit: FnMut(&ApiBlock) -> Option<R>,
{
    let mut current = Some(start);
    let genesis = HeaderId::from([0; 32]);

    while let Some(header_id) = current {
        if !scanned_blocks.insert(header_id) {
            break;
        }

        let Some(block) = get_block(header_id).await else {
            break;
        };

        if let Some(result) = visit_block(&block) {
            return Some(result);
        }

        let parent = block.header.parent_block;
        if parent == genesis {
            break;
        }

        current = Some(parent);
    }

    None
}

pub async fn wait_for_transactions_inclusion(
    client: &NodeHttpClient,
    tx_hashes: &[TxHash],
    duration: Duration,
) -> bool {
    let expected: HashSet<_> = tx_hashes.iter().copied().collect();

    timeout(duration, async {
        loop {
            let consensus = client
                .consensus_info()
                .await
                .expect("fetching consensus info should succeed");
            if consensus.cryptarchia_info.height == 0 {
                sleep(Duration::from_millis(500)).await;

                continue;
            }

            let mut scanned_blocks = HashSet::new();
            let mut found = HashSet::new();

            let found = scan_chain_until(
                consensus.cryptarchia_info.tip,
                &mut scanned_blocks,
                async |header_id| {
                    client
                        .block(&header_id)
                        .await
                        .expect("fetching storage block should succeed")
                },
                |block: &ApiBlock| {
                    for tx in &block.transactions {
                        let hash = tx.hash();
                        if expected.contains(&hash) {
                            found.insert(hash);
                        }
                    }

                    (found == expected).then_some(())
                },
            )
            .await
            .is_some();

            if found {
                break;
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .is_ok()
}
