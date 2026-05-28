use std::{
    collections::BTreeMap,
    num::NonZero,
    path::PathBuf,
    time::{Duration, Instant},
};

use futures::stream::{self, StreamExt as _};
use lb_common_http_client::ProcessedBlockEvent;
use lb_core::header::HeaderId;
use lb_http_api_common::{
    DEFAULT_NUMBER_OF_BLOCKS_TO_STREAM, MAX_BLOCKS_STREAM_BLOCKS, MAX_BLOCKS_STREAM_CHUNK_SIZE,
    paths::BLOCKS_RANGE_STREAM,
    queries::{BlockFilter, BlockSortOrder, BlocksStreamQuery},
};
use lb_node::config::RunConfig;
use lb_testing_framework::{
    DeploymentBuilder, LbcEnv, NodeHttpClient, TopologyConfig as TfTopologyConfig,
};
use logos_blockchain_tests::{
    common::manual_cluster::{
        LocalManualClusterHarnessBase, ManualNodeLayout, start_local_manual_cluster_with_layout,
    },
    cucumber::defaults::E2E_ARTIFACTS_DIR,
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
        Some(PathBuf::from(E2E_ARTIFACTS_DIR)),
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
    params: BlocksStreamQuery,
) -> Vec<ProcessedBlockEvent> {
    let start = Instant::now();
    print!(
        "  request_stream_events: blocks_limit={:?}, slot_from={:?}, \
        slot_to={:?}, descending={:?}, chunk_size={:?}, \
        immutable_only={:?}",
        params.blocks_limit,
        params.slot_from,
        params.slot_to,
        params.order,
        params.server_batch_size,
        params.block_filter
    );

    let stream = node
        .blocks_range_stream(params)
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

    let max_height = chain
        .ids_by_height
        .keys()
        .collect::<Vec<_>>()
        .iter()
        .max()
        .map(|&&v| nz(v))
        .expect("slot-mapped height must exist in canonical chain")
        .get();
    let blocks_limit = blocks_limit.min(max_height);
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
                BlocksStreamQuery {
                    slot_from: None,
                    slot_to: None,
                    order: if descending {
                        // None should default to `descending = true`
                        None
                    } else {
                        Some(BlockSortOrder::Ascending)
                    },
                    blocks_limit,
                    server_batch_size: chunk_size,
                    block_filter: Some(BlockFilter::ImmutableOnly),
                },
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

    // single block below LIB, at LIB and above LIB
    println!("case: single block below LIB, at LIB and above LIB");

    for target_height in [
        nz(chain.lib_height - 3),
        nz(chain.lib_height),
        nz(chain.lib_height + 1),
    ] {
        chain = refresh_chain(node, &chain).await;
        let (slot_from, slot_to) = blocks_request(&chain, target_height, target_height);
        let events = request_stream_events(
            node,
            BlocksStreamQuery {
                slot_from: Some(slot_from),
                slot_to: Some(slot_to),
                order: None,
                blocks_limit: None,
                server_batch_size: None,
                block_filter: None,
            },
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
    }

    // case: three blocks up to LIB, three blocks from LIB, all blocks, limited
    // blocks, various chunk sizes
    println!(
        "case: three blocks up to LIB, three blocks from LIB, all blocks, limited blocks, various \
        chunk sizes"
    );

    for (blocks_limit, blocks_from, blocks_to) in [
        (None, nz(chain.lib_height - 2), nz(chain.lib_height)),
        (None, nz(chain.lib_height), nz(chain.lib_height + 2)),
        (None, nz(1), nz(chain.tip_height)),
        (Some(nz(3)), nz(1), nz(chain.tip_height)),
        (
            Some(nz(MAX_BLOCKS_STREAM_BLOCKS)),
            nz(1),
            nz(chain.tip_height),
        ),
    ] {
        for chunk_size in [
            None,
            Some(nz(1)),
            Some(nz(4)),
            Some(nz(chain.lib_height)),
            Some(nz(chain.tip_height + 10)),
            Some(nz(MAX_BLOCKS_STREAM_CHUNK_SIZE)),
        ] {
            for descending in [false, true] {
                chain = refresh_chain(node, &chain).await;
                let (slot_from, slot_to) = blocks_request(&chain, blocks_from, blocks_to);
                let events = request_stream_events(
                    node,
                    BlocksStreamQuery {
                        slot_from: Some(slot_from),
                        slot_to: Some(slot_to),
                        order: Some(if descending {
                            BlockSortOrder::Descending
                        } else {
                            BlockSortOrder::Ascending
                        }),
                        blocks_limit,
                        server_batch_size: chunk_size,
                        block_filter: Some(BlockFilter::MutableAndImmutable),
                    },
                )
                .await;

                let expected_ids =
                    ids_in_slot_range(&chain, slot_from, slot_to, descending, blocks_limit);
                assert_event_order_matches_expected(&events, &expected_ids);
                assert_stream_integrity(&chain, &events);
            }
        }
    }

    // case: ascending above LIB without slot_from is best-effort bounded
    println!("case: ascending above LIB without slot_from is best-effort bounded");

    chain = refresh_chain(node, &chain).await;
    let tip_slot = chain.tip_slot;
    let blocks_limit = 7;
    let events = request_stream_events(
        node,
        BlocksStreamQuery {
            slot_from: None,
            slot_to: Some(tip_slot),
            order: Some(BlockSortOrder::Ascending),
            blocks_limit: Some(nz(blocks_limit)),
            server_batch_size: None,
            block_filter: Some(BlockFilter::MutableAndImmutable),
        },
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

    // ============== Clamping (soft failures) =============

    // case: three blocks from LIB and up (immutable only, slot_to clamps to LIB)
    println!("case: three blocks from LIB and up (immutable only, slot_to clamps to LIB)");

    chain = refresh_chain(node, &chain).await;
    let blocks_from = nz(chain.lib_height);
    let blocks_to = nz(chain.lib_height + 2);
    let (slot_from, slot_to) = blocks_request(&chain, blocks_from, blocks_to);
    let events = request_stream_events(
        node,
        BlocksStreamQuery {
            slot_from: Some(slot_from),
            slot_to: Some(slot_to),
            order: None,
            blocks_limit: None,
            server_batch_size: None,
            block_filter: Some(BlockFilter::ImmutableOnly),
        },
    )
    .await;
    let expected_ids = ids_in_slot_range(&chain, slot_from, chain.lib_slot, true, None);
    assert_event_order_matches_expected(&events, &expected_ids);
    assert_stream_integrity(&chain, &events);

    // case: all blocks, small chunked (immutable only, slot_to clamps to LIB)
    println!("case: all blocks, small chunked (immutable only, slot_to clamps to LIB)");

    chain = refresh_chain(node, &chain).await;
    let blocks_from = nz(1);
    let blocks_to = nz(chain.tip_height);
    let (slot_from, slot_to) = blocks_request(&chain, blocks_from, blocks_to);
    let events = request_stream_events(
        node,
        BlocksStreamQuery {
            slot_from: Some(slot_from),
            slot_to: Some(slot_to),
            order: None,
            blocks_limit: None,
            server_batch_size: Some(nz(4)),
            block_filter: Some(BlockFilter::ImmutableOnly),
        },
    )
    .await;
    let expected_ids = ids_in_slot_range(&chain, slot_from, chain.lib_slot, true, None);
    assert_event_order_matches_expected(&events, &expected_ids);
    assert_stream_integrity(&chain, &events);

    // case: slot_to above tip should clamp to tip and succeed
    println!("case: slot_to above tip should clamp to tip and succeed via raw HTTP query");

    let client = reqwest::Client::new();
    chain = refresh_chain(node, &chain).await;
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
        reqwest::StatusCode::OK,
        "slot_to above tip must clamp and return 200"
    );

    let body = resp
        .text()
        .await
        .expect("stream response body should be readable");
    let events = body
        .lines()
        .map(|line| {
            serde_json::from_str::<ProcessedBlockEvent>(line)
                .expect("ndjson line should decode into ProcessedBlockEvent")
        })
        .collect::<Vec<_>>();
    let expected_ids = ids_in_slot_range(&chain, 0, chain.tip_slot, true, None);
    assert_event_order_matches_expected(&events, &expected_ids);
    assert_stream_integrity(&chain, &events);

    // case: immutable_only=true with slot_to above LIB should clamp to LIB and
    // succeed
    println!(
        "case: immutable_only=true with slot_to above LIB should clamp to LIB and succeed via raw HTTP query"
    );

    chain = refresh_chain(node, &chain).await;
    let (_, lib_slot) = current_tip_lib_slot(node).await;
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
        reqwest::StatusCode::OK,
        "immutable_only=true with slot_to above LIB must clamp and return 200"
    );

    let body = resp
        .text()
        .await
        .expect("stream response body should be readable");
    let events = body
        .lines()
        .map(|line| {
            serde_json::from_str::<ProcessedBlockEvent>(line)
                .expect("ndjson line should decode into ProcessedBlockEvent")
        })
        .collect::<Vec<_>>();
    let expected_ids = ids_in_slot_range(&chain, 0, chain.lib_slot, true, None);
    assert_event_order_matches_expected(&events, &expected_ids);
    assert_stream_integrity(&chain, &events);

    // ============== Failure modes =============

    // case: single block above LIB (immutable only, explicit slot_from stays above
    // clamped LIB)
    println!(
        "case: single block above LIB (immutable only, explicit slot_from stays above clamped LIB)"
    );

    chain = refresh_chain(node, &chain).await;
    let target_height = nz(chain.lib_height + 3);
    let (slot_from, slot_to) = blocks_request(&chain, target_height, target_height);
    let Err(err) = node
        .blocks_range_stream(BlocksStreamQuery {
            slot_from: Some(slot_from),
            slot_to: Some(slot_to),
            order: None,
            blocks_limit: None,
            server_batch_size: None,
            block_filter: Some(BlockFilter::ImmutableOnly),
        })
        .await
    else {
        panic!("explicit slot_from above clamped LIB should fail");
    };
    assert!(
        matches!(err, lb_common_http_client::Error::Server(ref message) if message.contains("slot_from")),
        "invalid clamped range should mention 'slot_from', got: {err}"
    );

    // case: blocks_limit=0 and server_batch_size=0 should fail (400) via raw HTTP
    // query
    println!("case: blocks_limit=0 and server_batch_size=0 should fail (400) via raw HTTP query");

    let (tip_slot, _) = current_tip_lib_slot(node).await;
    for query_part in ["blocks_limit=0", "server_batch_size=0"] {
        let mut url = node.base_url().clone();
        url.set_path(BLOCKS_RANGE_STREAM);
        url.set_query(Some(&format!("{query_part}&slot_to={tip_slot}")));

        let resp = client
            .get(url)
            .send()
            .await
            .expect("raw blocks/stream request should complete");

        assert_eq!(
            resp.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "'{query_part}' must return 400"
        );

        let body = resp
            .text()
            .await
            .expect("error response body should be readable");
        assert!(
            body.contains("expected a nonzero usize"),
            "400 body should mention 'expected a nonzero usize', got: {body}"
        );
    }

    // case: slot_from > slot_to should fail client-side validation
    println!("case: slot_from > slot_to should fail client-side validation");

    chain = refresh_chain(node, &chain).await;
    let Err(err) = node
        .blocks_range_stream(BlocksStreamQuery {
            slot_from: Some(chain.lib_slot),
            slot_to: Some(chain.lib_slot / 2),
            order: None,
            blocks_limit: None,
            server_batch_size: None,
            block_filter: Some(BlockFilter::MutableAndImmutable),
        })
        .await
    else {
        panic!("slot_from > slot_to should fail client-side validation");
    };

    assert!(
        matches!(err, lb_common_http_client::Error::Client(ref message)
            if message.contains("slot_from") && message.contains("slot_to")),
        "expected client-side slot range validation error, got: {err}"
    );
}

const fn nz(value: usize) -> NonZero<usize> {
    NonZero::new(value).unwrap()
}

async fn current_tip_lib_slot(node: &NodeHttpClient) -> (u64, u64) {
    let cryptarchia_info = node
        .consensus_info()
        .await
        .expect("fetching consensus info should succeed")
        .cryptarchia_info;
    (
        u64::from(cryptarchia_info.slot),
        u64::from(cryptarchia_info.lib_slot),
    )
}
