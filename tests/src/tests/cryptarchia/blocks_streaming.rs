use std::{
    collections::BTreeMap,
    num::NonZero,
    time::{Duration, Instant},
};

use futures::stream::{self, StreamExt as _};
use lb_common_http_client::ProcessedBlockEvent;
use lb_core::header::HeaderId;
use lb_http_api_common::{DEFAULT_NUMBER_OF_BLOCKS_TO_STREAM, paths::BLOCKS_RANGE_STREAM};
use lb_node::config::RunConfig;
use lb_testing_framework::{
    DeploymentBuilder, LbcEnv, NodeHttpClient, TopologyConfig as TfTopologyConfig,
};
use logos_blockchain_tests::common::manual_cluster::{
    LocalManualClusterHarnessBase, ManualNodeLayout, start_local_manual_cluster_with_layout,
};
use serial_test::serial;
use testing_framework_core::scenario::{DynError, StartedNode};
use tokio::time::timeout;

const NODE_COUNT: usize = 2;
const SECURITY_PARAM: u32 = 7;
const MIN_LIB_HEIGHT: usize = 3;
const MIN_BLOCKS_ABOVE_LIB: usize = 2;
const WAIT_FOR_LIB_AND_TIP_TIMEOUT: Duration = Duration::from_mins(10);

#[derive(Clone)]
struct CanonicalChain {
    ids_by_height: BTreeMap<usize, HeaderId>,
    heights_by_slot: BTreeMap<u64, usize>,
    lib_height: usize,
    lib_slot: u64,
    tip_height: usize,
    tip_slot: u64,
}

impl CanonicalChain {
    fn get_tip_id(&self) -> Option<HeaderId> {
        self.ids_by_height.get(&self.tip_height).copied()
    }

    fn get_lib_id(&self) -> Option<HeaderId> {
        self.ids_by_height.get(&self.lib_height).copied()
    }
}

async fn start_blocks_streaming_cluster(
    test_name: &str,
) -> (LocalManualClusterHarnessBase, Vec<StartedNode<LbcEnv>>) {
    start_local_manual_cluster_with_layout(
        test_name,
        "cryptarchia-blocks-streaming",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(NODE_COUNT)
                .with_test_context(Some(test_name.to_owned())),
        ),
        NODE_COUNT,
        ManualNodeLayout::SelectNodeSeed(0),
        |config| Ok::<_, DynError>(blocks_streaming_config(config)),
    )
    .await
}

const fn blocks_streaming_config(mut config: RunConfig) -> RunConfig {
    config.deployment.time.slot_duration = Duration::from_secs(1);
    config
        .user
        .cryptarchia
        .service
        .bootstrap
        .prolonged_bootstrap_period = Duration::ZERO;
    config.deployment.cryptarchia.security_param = NonZero::new(SECURITY_PARAM).unwrap();

    config
}

fn required_tip_height() -> u64 {
    u64::from(SECURITY_PARAM) + MIN_LIB_HEIGHT as u64 + MIN_BLOCKS_ABOVE_LIB as u64
}

async fn wait_for_lib_and_tip(nodes: &[StartedNode<LbcEnv>]) -> lb_chain_service::CryptarchiaInfo {
    let min_height = required_tip_height();
    let timeout = WAIT_FOR_LIB_AND_TIP_TIMEOUT;
    println!(
        "waiting for canonical chain with height >= {min_height}, lib height >= {MIN_LIB_HEIGHT} and tip at least {MIN_BLOCKS_ABOVE_LIB} above LIB: timeout:{timeout:?}",
    );
    let timeout = tokio::time::sleep(timeout);
    let mut tick: u32 = 0;
    tokio::select! {
        () = timeout => panic!("timed out waiting for 'lib_slot >= 1 && tip_slot > lib_slot'"),

        () = async { loop {
                let Some(infos) = collect_cryptarchia_infos(nodes).await else {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                };

                let all_reached_min_height = infos
                    .iter()
                    .all(|info| info.height >= min_height);

                if all_reached_min_height {
                    let chain = canonical_chain(&nodes[0].client, &infos[0]).await;
                    if chain.lib_height >= MIN_LIB_HEIGHT
                        && chain.tip_height >= chain.lib_height + MIN_BLOCKS_ABOVE_LIB
                    {
                        break;
                    }
                }

                if tick.is_multiple_of(20) {
                    println!(
                        "waiting... {}",
                        infos.iter()
                            .map(|info| format!("{}/{:?}/{:?}", info.height, info.slot, info.lib_slot))
                            .collect::<Vec<_>>()
                            .join(" | ")
                    );
                }
                tick = tick.wrapping_add(1);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

        } => {}
    }
    // Print final stats
    let final_infos = collect_cryptarchia_infos(nodes)
        .await
        .expect("fetching final consensus info should succeed")
        .into_iter()
        .map(|info| format!("{}/{:?}/{:?}", info.height, info.slot, info.lib_slot))
        .collect::<Vec<_>>();
    println!("final: {}", final_infos.join(" | "));

    nodes[0]
        .client
        .consensus_info()
        .await
        .expect("fetching final consensus info should succeed")
        .cryptarchia_info
}

async fn collect_cryptarchia_infos(
    nodes: &[StartedNode<LbcEnv>],
) -> Option<Vec<lb_chain_service::CryptarchiaInfo>> {
    let infos = stream::iter(nodes)
        .then(async |node| node.client.consensus_info().await.ok())
        .collect::<Vec<_>>()
        .await;

    infos.into_iter().collect::<Option<Vec<_>>>().map(|infos| {
        infos
            .into_iter()
            .map(|info| info.cryptarchia_info)
            .collect()
    })
}

async fn canonical_chain(
    node: &NodeHttpClient,
    final_info: &lb_chain_service::CryptarchiaInfo,
) -> CanonicalChain {
    let mut ids_by_height = BTreeMap::new();
    let mut heights_by_slot = BTreeMap::new();
    let mut current_id = final_info.tip;
    let mut current_height = usize::try_from(final_info.height).unwrap();
    let mut lib_height = None;
    let mut previous_slot = None;

    while current_height >= 1 {
        let block = node
            .block(&current_id)
            .await
            .expect("canonical block request should succeed")
            .expect("canonical block should exist");
        ids_by_height.insert(current_height, block.header.id);
        let slot = u64::from(block.header.slot);
        let replaced = heights_by_slot.insert(slot, current_height);
        assert!(
            replaced.is_none(),
            "pre-test invariant failed: duplicate canonical slot {slot} at heights {} and {current_height}",
            replaced.unwrap_or_default()
        );

        if let Some(prev) = previous_slot {
            assert!(
                slot < prev,
                "pre-test invariant failed: canonical slots must be strictly increasing by height; height {current_height} slot {slot} is not < next height slot {prev}",
            );
        }
        previous_slot = Some(slot);

        if current_id == final_info.lib {
            lib_height = Some(current_height);
        }

        if current_height == 1 {
            break;
        }

        current_id = block.header.parent_block;
        current_height -= 1;
    }

    CanonicalChain {
        ids_by_height,
        heights_by_slot,
        lib_height: lib_height.expect("lib must be on the canonical chain"),
        lib_slot: final_info.lib_slot.into_inner(),
        tip_height: usize::try_from(final_info.height).expect("should fit in usize"),
        tip_slot: final_info.slot.into_inner(),
    }
}

async fn setup_nodes_and_chain(
    test_name: &str,
) -> (
    LocalManualClusterHarnessBase,
    Vec<StartedNode<LbcEnv>>,
    CanonicalChain,
) {
    let (base, nodes) = start_blocks_streaming_cluster(test_name).await;
    let final_info = wait_for_lib_and_tip(&nodes).await;
    let chain = canonical_chain(&nodes[0].client, &final_info).await;
    (base, nodes, chain)
}

fn slot_for_height(chain: &CanonicalChain, height: usize) -> u64 {
    *chain
        .heights_by_slot
        .iter()
        .find_map(|(slot, h)| (*h == height).then_some(slot))
        .expect("slot must exist for every canonical height")
}

async fn request_stream_events(
    node: &NodeHttpClient,
    blocks_limit: Option<NonZero<usize>>,
    slot_from: Option<u64>,
    slot_to: Option<u64>,
    descending: Option<bool>,
    chunk_size: Option<NonZero<usize>>,
    immutable_only: Option<bool>,
) -> Vec<ProcessedBlockEvent> {
    let start = Instant::now();
    print!(
        "  request_stream_events: blocks_limit={blocks_limit:?}, slot_from={slot_from:?}, \
        slot_to={slot_to:?}, descending={descending:?}, chunk_size={chunk_size:?}, \
        immutable_only={immutable_only:?}"
    );

    let stream = node
        .blocks_range_stream(
            blocks_limit,
            slot_from,
            slot_to,
            descending,
            chunk_size,
            immutable_only,
        )
        .await
        .expect("blocks stream request should succeed");

    let events = timeout(Duration::from_secs(15), stream.collect::<Vec<_>>())
        .await
        .expect("timed out collecting blocks stream events");
    println!(
        ", collected {} events in {:?}",
        events.len(),
        start.elapsed()
    );

    events
}

fn assert_stream_integrity(_chain: &CanonicalChain, events: &[ProcessedBlockEvent]) {
    let first = events
        .first()
        .expect("stream should contain at least one event");
    for event in events {
        assert_eq!(event.tip, first.tip, "all events must share the same tip");
        assert_eq!(
            event.tip_slot, first.tip_slot,
            "all events must share the same tip slot"
        );
        assert_eq!(event.lib, first.lib, "all events must share the same LIB");
        assert_eq!(
            event.lib_slot, first.lib_slot,
            "all events must share the same LIB slot"
        );
    }
    assert!(
        u64::from(first.lib_slot) <= u64::from(first.tip_slot),
        "LIB slot must not exceed tip slot"
    );
}

async fn refresh_chain(node: &NodeHttpClient, chain: &CanonicalChain) -> CanonicalChain {
    let info = node
        .consensus_info()
        .await
        .expect("fetching consensus info should succeed")
        .cryptarchia_info;
    if let Some(current_tip) = chain.get_tip_id()
        && let Some(current_lib) = chain.get_lib_id()
        && info.tip == current_tip
        && info.lib == current_lib
    {
        return chain.clone();
    }
    canonical_chain(node, &info).await
}

fn blocks_request(
    chain: &CanonicalChain,
    from_height: NonZero<usize>,
    to_height: NonZero<usize>,
) -> (u64, u64) {
    let slot_from = slot_for_height(chain, from_height.get());
    let slot_to = slot_for_height(chain, to_height.get());
    (slot_from, slot_to)
}

fn ids_in_slot_range(
    chain: &CanonicalChain,
    slot_from: u64,
    slot_to: u64,
    descending: bool,
    blocks_limit: Option<NonZero<usize>>,
) -> Vec<HeaderId> {
    let blocks_limit = blocks_limit
        .unwrap_or_else(|| NonZero::new(DEFAULT_NUMBER_OF_BLOCKS_TO_STREAM).unwrap())
        .get();
    let iter: Box<dyn Iterator<Item = (&u64, &usize)>> = if descending {
        Box::new(chain.heights_by_slot.range(slot_from..=slot_to).rev())
    } else {
        Box::new(chain.heights_by_slot.range(slot_from..=slot_to))
    };

    iter.take(blocks_limit)
        .map(|(_, height)| {
            *chain
                .ids_by_height
                .get(height)
                .expect("slot-mapped height must exist in canonical chain")
        })
        .collect()
}

fn assert_event_order_matches_expected(events: &[ProcessedBlockEvent], expected_ids: &[HeaderId]) {
    let actual_ids = events
        .iter()
        .map(|event| event.block.header.id)
        .collect::<Vec<_>>();
    assert_eq!(
        actual_ids, expected_ids,
        "streamed headers must match requested canonical order"
    );
}

#[tokio::test]
#[serial]
async fn test_blocks_streaming() {
    let (_base, nodes, mut chain) =
        setup_nodes_and_chain("blocks_streaming_use_cases_share_one_setup").await;
    let node = &nodes[0].client;

    assert!(
        chain.lib_height > 1,
        "lib height must allow a block below LIB"
    );
    assert!(
        chain.tip_height > chain.lib_height,
        "tip height must allow a block above LIB"
    );
    assert!(chain.lib_height >= 3, "lib height must be at least 3");
    assert!(
        chain.tip_height >= chain.lib_height + 2,
        "tip height must allow streaming three blocks starting from LIB"
    );

    // ============== Happy path =============

    // case: immutable_only=true with no blocks_to should anchor at LIB, various
    // chunk sizes
    println!("case: immutable_only=true without blocks_to anchors at LIB, various chunk sizes");

    let blocks_limit = None;
    for chunk_size in [
        None,
        Some(nz(1)),
        Some(nz(4)),
        Some(nz(chain.lib_height)),
        Some(nz(chain.tip_height + 10)),
    ] {
        for descending in [false, true] {
            chain = refresh_chain(node, &chain).await;
            let events = request_stream_events(
                node,
                blocks_limit,
                None,
                None,
                Some(descending),
                chunk_size,
                Some(true),
            )
            .await;

            let expected_ids =
                ids_in_slot_range(&chain, 0, chain.lib_slot, descending, blocks_limit);
            assert_eq!(
                events.len(),
                expected_ids.len(),
                "immutable_only=true default should return all immutable blocks up to LIB"
            );
            assert_event_order_matches_expected(&events, &expected_ids);
            assert_stream_integrity(&chain, &events);
        }
    }

    // case: single block below LIB
    println!("case: single block below LIB");

    chain = refresh_chain(node, &chain).await;
    let target_height = nz(chain.lib_height - 3);
    let (slot_from, slot_to) = blocks_request(&chain, target_height, target_height);
    let events = request_stream_events(
        node,
        None,
        Some(slot_from),
        Some(slot_to),
        None,
        None,
        Some(false),
    )
    .await;

    assert_eq!(events.len(), 1);
    let expected_id = *chain
        .ids_by_height
        .get(&target_height.get())
        .expect("target height should exist on canonical chain");

    assert_eq!(
        events[0].block.header.id, expected_id,
        "slot range should include requested header"
    );

    assert_stream_integrity(&chain, &events);

    // case: single block at LIB
    println!("case: single block at LIB");

    chain = refresh_chain(node, &chain).await;
    let target_height = nz(chain.lib_height);
    let (slot_from, slot_to) = (chain.lib_slot, chain.lib_slot);
    let events = request_stream_events(
        node,
        None,
        Some(slot_from),
        Some(slot_to),
        None,
        None,
        Some(false),
    )
    .await;

    let expected_id = *chain
        .ids_by_height
        .get(&target_height.get())
        .expect("target height should exist on canonical chain");

    assert_eq!(
        events[0].block.header.id, expected_id,
        "slot range should include requested header"
    );

    assert_stream_integrity(&chain, &events);

    // case: single block above LIB
    println!("case: single block above LIB");

    chain = refresh_chain(node, &chain).await;
    let target_height = nz(chain.lib_height + 1);
    let (slot_from, slot_to) = blocks_request(&chain, target_height, target_height);
    let events = request_stream_events(
        node,
        None,
        Some(slot_from),
        Some(slot_to),
        None,
        None,
        Some(false),
    )
    .await;

    let expected_id = *chain
        .ids_by_height
        .get(&target_height.get())
        .expect("target height should exist on canonical chain");

    assert_eq!(
        events[0].block.header.id, expected_id,
        "slot range should include requested header"
    );
    assert_stream_integrity(&chain, &events);

    // case: three blocks up to LIB, various chunk sizes
    println!("case: three blocks up to LIB, various chunk sizes");

    let blocks_limit = None;
    for chunk_size in [
        None,
        Some(nz(1)),
        Some(nz(4)),
        Some(nz(chain.lib_height)),
        Some(nz(chain.tip_height + 10)),
    ] {
        for descending in [false, true] {
            chain = refresh_chain(node, &chain).await;
            let blocks_from = nz(chain.lib_height - 2);
            let blocks_to = nz(chain.lib_height);
            let (slot_from, slot_to) = blocks_request(&chain, blocks_from, blocks_to);
            let events = request_stream_events(
                node,
                blocks_limit,
                Some(slot_from),
                Some(slot_to),
                Some(descending),
                chunk_size,
                Some(false),
            )
            .await;

            let expected_ids =
                ids_in_slot_range(&chain, slot_from, slot_to, descending, blocks_limit);
            assert_event_order_matches_expected(&events, &expected_ids);
            assert_stream_integrity(&chain, &events);
        }
    }

    // case: three blocks from LIB and up, various chunk sizes
    println!("case: three blocks from LIB and up, various chunk sizes");

    let blocks_limit = None;
    for chunk_size in [
        None,
        Some(nz(1)),
        Some(nz(4)),
        Some(nz(chain.lib_height)),
        Some(nz(chain.tip_height + 10)),
    ] {
        for descending in [false, true] {
            chain = refresh_chain(node, &chain).await;
            let blocks_from = nz(chain.lib_height);
            let blocks_to = nz(chain.lib_height + 2);
            let (slot_from, slot_to) = blocks_request(&chain, blocks_from, blocks_to);
            let events = request_stream_events(
                node,
                blocks_limit,
                Some(slot_from),
                Some(slot_to),
                Some(descending),
                chunk_size,
                Some(false),
            )
            .await;

            let expected_ids =
                ids_in_slot_range(&chain, slot_from, slot_to, descending, blocks_limit);
            assert_event_order_matches_expected(&events, &expected_ids);
            assert_stream_integrity(&chain, &events);
        }
    }

    // case: all blocks, various chunk sizes
    println!("case: all blocks, various chunk sizes");

    let blocks_limit = None;
    for chunk_size in [
        None,
        Some(nz(1)),
        Some(nz(4)),
        Some(nz(chain.lib_height)),
        Some(nz(chain.tip_height + 10)),
    ] {
        for descending in [false, true] {
            chain = refresh_chain(node, &chain).await;
            let (slot_from, slot_to) = (0, chain.tip_slot);
            let events = request_stream_events(
                node,
                blocks_limit,
                Some(slot_from),
                Some(slot_to),
                Some(descending),
                chunk_size,
                Some(false),
            )
            .await;

            let expected_ids =
                ids_in_slot_range(&chain, slot_from, slot_to, descending, blocks_limit);
            assert_event_order_matches_expected(&events, &expected_ids);
            assert_stream_integrity(&chain, &events);
        }
    }

    // case: limited blocks, various chunk sizes
    println!("case: limited blocks, various chunk sizes");

    let blocks_limit = Some(nz(3));
    for chunk_size in [
        None,
        Some(nz(1)),
        Some(nz(4)),
        Some(nz(chain.lib_height)),
        Some(nz(chain.tip_height + 10)),
    ] {
        for descending in [false, true] {
            chain = refresh_chain(node, &chain).await;
            let (slot_from, slot_to) = (0, chain.tip_slot);
            let events = request_stream_events(
                node,
                blocks_limit,
                Some(slot_from),
                Some(slot_to),
                Some(descending),
                chunk_size,
                Some(false),
            )
            .await;

            let expected_ids =
                ids_in_slot_range(&chain, slot_from, slot_to, descending, blocks_limit);
            assert_event_order_matches_expected(&events, &expected_ids);
            assert_stream_integrity(&chain, &events);
        }
    }

    // case: ascending above LIB without slot_from is best-effort bounded
    println!("case: ascending above LIB without slot_from is best-effort bounded");

    chain = refresh_chain(node, &chain).await;
    let tip_slot = chain.tip_slot;
    let blocks_limit = 7;
    let events = request_stream_events(
        node,
        Some(nz(blocks_limit)),
        None,
        Some(tip_slot),
        Some(false),
        None,
        Some(false),
    )
    .await;

    assert!(
        !events.is_empty(),
        "best-effort ascending request should still return some blocks"
    );
    assert!(
        events.len() <= blocks_limit,
        "best-effort ascending request must still respect blocks_limit"
    );
    assert!(
        events.windows(2).all(|pair| {
            u64::from(pair[0].block.header.slot) <= u64::from(pair[1].block.header.slot)
        }),
        "best-effort ascending request should preserve ascending order"
    );

    // ============== Failure modes =============

    // case: single block above LIB (immutable only, should fail)
    println!("case: single block above LIB (immutable only, should fail)");

    chain = refresh_chain(node, &chain).await;
    let target_height = nz(chain.lib_height + 3);
    let (slot_from, slot_to) = blocks_request(&chain, target_height, target_height);
    let Err(err) = node
        .blocks_range_stream(None, Some(slot_from), Some(slot_to), None, None, Some(true))
        .await
    else {
        panic!("immutable-only request above LIB should fail");
    };
    assert!(
        matches!(err, lb_common_http_client::Error::Server(ref message) if message.contains("lib_slot")),
        "immutable-only request above LIB should mention lib_slot, got: {err}"
    );

    // case: three blocks from LIB and up (immutable only, should fail)
    println!("case: three blocks from LIB and up (immutable only, should fail)");

    chain = refresh_chain(node, &chain).await;
    let blocks_from = nz(chain.lib_height);
    let blocks_to = nz(chain.lib_height + 2);
    let (slot_from, slot_to) = blocks_request(&chain, blocks_from, blocks_to);
    let Err(err) = node
        .blocks_range_stream(None, Some(slot_from), Some(slot_to), None, None, Some(true))
        .await
    else {
        panic!("immutable-only request above LIB should fail");
    };
    assert!(
        matches!(err, lb_common_http_client::Error::Server(ref message) if message.contains("lib_slot")),
        "immutable-only request above LIB should mention lib_slot, got: {err}"
    );

    // case: all blocks, small chunked (immutable only, should fail above LIB)
    println!("case: all blocks, small chunked (immutable only, should fail above LIB)");

    chain = refresh_chain(node, &chain).await;
    let blocks_from = nz(1);
    let blocks_to = nz(chain.tip_height);
    let (slot_from, slot_to) = blocks_request(&chain, blocks_from, blocks_to);
    let Err(err) = node
        .blocks_range_stream(
            None,
            Some(slot_from),
            Some(slot_to),
            None,
            Some(nz(4)),
            Some(true),
        )
        .await
    else {
        panic!("immutable-only request above LIB should fail");
    };
    assert!(
        matches!(err, lb_common_http_client::Error::Server(ref message) if message.contains("lib_slot")),
        "immutable-only request above LIB should mention lib_slot, got: {err}"
    );

    // case: blocks_limit=0 should fail (400) via raw HTTP query
    println!("case: blocks_limit=0 should fail (400) via raw HTTP query");

    let tip_slot = u64::from(
        node.consensus_info()
            .await
            .expect("fetching consensus info should succeed")
            .cryptarchia_info
            .slot,
    );
    let client = reqwest::Client::new();

    let mut url = node.base_url().clone();
    url.set_path(BLOCKS_RANGE_STREAM);
    url.set_query(Some(&format!("blocks_limit=0&slot_to={tip_slot}")));

    let resp = client
        .get(url)
        .send()
        .await
        .expect("raw blocks/stream request should complete");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "blocks_limit=0 must return 400"
    );

    let body = resp
        .text()
        .await
        .expect("error response body should be readable");
    assert!(
        body.contains("blocks_limit"),
        "400 body should mention blocks_limit, got: {body}"
    );

    // case: slot_to above tip should fail (400)
    println!("case: slot_to above tip should fail (400) via raw HTTP query");

    let mut url = node.base_url().clone();
    url.set_path(BLOCKS_RANGE_STREAM);
    url.set_query(Some(&format!("slot_to={}", tip_slot + 1)));

    let resp = client
        .get(url)
        .send()
        .await
        .expect("raw blocks/stream request should complete");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "slot_to above tip must return 400"
    );

    let body = resp
        .text()
        .await
        .expect("error response body should be readable");
    assert!(
        body.contains("tip_slot"),
        "400 body should mention tip_slot, got: {body}"
    );

    // case: immutable_only=true with slot_to above LIB should fail (400)
    println!(
        "case: immutable_only=true with slot_to above LIB should fail (400) via raw HTTP query"
    );

    let lib_slot = u64::from(
        node.consensus_info()
            .await
            .expect("fetching consensus info should succeed")
            .cryptarchia_info
            .lib_slot,
    );
    let mut url = node.base_url().clone();
    url.set_path(BLOCKS_RANGE_STREAM);
    url.set_query(Some(&format!(
        "slot_to={}&immutable_only=true",
        lib_slot + 1
    )));

    let resp = client
        .get(url)
        .send()
        .await
        .expect("raw blocks/stream request should complete");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "immutable_only=true with slot_to above LIB must return 400"
    );

    let body = resp
        .text()
        .await
        .expect("error response body should be readable");
    assert!(
        body.contains("lib_slot"),
        "400 body should mention lib_slot, got: {body}"
    );
}

const fn nz(value: usize) -> NonZero<usize> {
    NonZero::new(value).unwrap()
}
