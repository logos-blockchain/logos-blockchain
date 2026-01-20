use std::{collections::HashSet, hash::BuildHasher};

use lb_core::{block::Block, header::HeaderId, mantle::SignedMantleTx};

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
    Fut: Future<Output = Option<Block<SignedMantleTx>>>,
    Visit: FnMut(&Block<SignedMantleTx>) -> Option<R>,
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

        let parent = block.header().parent();
        if parent == genesis {
            break;
        }

        current = Some(parent);
    }

    None
}
