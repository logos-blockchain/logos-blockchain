use super::*;

pub(super) async fn walk_to_target(
    client: &NodeHttpClient,
    start: HeaderId,
    target: HeaderId,
    target_slot: u64,
    cache: &mut HeaderCache,
) -> AncestryPath {
    let mut links = Vec::new();
    let mut current = start;
    let mut seen = HashSet::new();
    for _ in 0..FINALITY_MAX_ANCESTRY_DEPTH {
        if current == target {
            return AncestryPath {
                links,
                target_reached: true,
                complete_below_slot: true,
                error: None,
            };
        }
        if !seen.insert(current) {
            return AncestryPath {
                links,
                target_reached: false,
                complete_below_slot: false,
                error: Some(format!(
                    "ancestry cycle at {}",
                    current.encode_hex::<String>()
                )),
            };
        }
        let link = match fetch_header_link(client, current, cache).await {
            Ok(link) => link,
            Err(error) => {
                return AncestryPath {
                    links,
                    target_reached: false,
                    complete_below_slot: false,
                    error: Some(error),
                };
            }
        };
        links.push(link);
        if link.parent == target {
            return AncestryPath {
                links,
                target_reached: true,
                complete_below_slot: true,
                error: None,
            };
        }
        if link.parent == current {
            return self_parented_ancestry_result(links, current);
        }
        if link.slot < target_slot {
            return AncestryPath {
                links,
                target_reached: false,
                complete_below_slot: true,
                error: None,
            };
        }
        current = link.parent;
    }
    AncestryPath {
        links,
        target_reached: false,
        complete_below_slot: false,
        error: Some(format!(
            "ancestry depth exceeded {FINALITY_MAX_ANCESTRY_DEPTH} blocks"
        )),
    }
}

pub(super) fn self_parented_ancestry_result(
    links: Vec<HeaderLink>,
    block_id: HeaderId,
) -> AncestryPath {
    AncestryPath {
        links,
        target_reached: false,
        complete_below_slot: false,
        error: Some(format!(
            "self-parented header {} cannot prove target ancestry or absence",
            block_id.encode_hex::<String>()
        )),
    }
}

pub(super) fn proven_below_target_slot_path(
    current_slot: u64,
    target_slot: u64,
) -> Option<AncestryPath> {
    (current_slot < target_slot).then_some(AncestryPath {
        links: Vec::new(),
        target_reached: false,
        complete_below_slot: true,
        error: None,
    })
}

pub(super) async fn fetch_header_link(
    client: &NodeHttpClient,
    id: HeaderId,
    cache: &mut HeaderCache,
) -> Result<HeaderLink, String> {
    if let Some(link) = cache.links.get(&id) {
        return Ok(*link);
    }
    let id_text = id.encode_hex::<String>();
    let block = match client.block(&id).await {
        Ok(Some(block)) => block,
        Ok(None) => {
            let error = format!("block API returned no block for {id_text}");
            cache.failures.insert(id_text, error.clone());
            return Err(error);
        }
        Err(error) => {
            let error = format!("block API failed for {id_text}: {error}");
            cache.failures.insert(id_text, error.clone());
            return Err(error);
        }
    };
    if block.header.id != id {
        return Err(format!(
            "block API returned {} while resolving {id_text}",
            block.header.id.encode_hex::<String>()
        ));
    }
    let link = HeaderLink {
        id,
        parent: block.header.parent_block,
        slot: u64::from(block.header.slot),
    };
    cache.links.insert(id, link);
    Ok(link)
}

pub(super) fn ancestry_presence(path: &AncestryPath, target: HeaderId) -> Option<bool> {
    if path.target_reached
        || path
            .links
            .iter()
            .any(|link| link.id == target || link.parent == target)
    {
        Some(true)
    } else if path.complete_below_slot {
        Some(false)
    } else {
        None
    }
}

pub(super) fn ancestry_path_json(path: &AncestryPath) -> Value {
    json!({
        "block_ids": path.links.iter().map(|link| link.id.encode_hex::<String>()).collect::<Vec<_>>(),
        "header_links": path.links.iter().map(|link| json!({
            "block_id": link.id.encode_hex::<String>(),
            "parent_block_id": link.parent.encode_hex::<String>(),
            "slot": link.slot,
        })).collect::<Vec<_>>(),
        "target_reached": path.target_reached,
        "complete_below_target": path.complete_below_slot,
        "error": path.error,
    })
}

pub(super) fn header_cache_failures_json(cache: &HeaderCache) -> Value {
    Value::Object(
        cache
            .failures
            .iter()
            .map(|(id, error)| (id.clone(), json!(error)))
            .collect(),
    )
}

pub(super) async fn block_id_at_height(
    client: &NodeHttpClient,
    tip: HeaderId,
    tip_height: u64,
    target_height: u64,
    cache: &mut HeaderCache,
) -> Result<HeaderId, String> {
    if target_height > tip_height {
        return Err(format!(
            "target height {target_height} exceeds tip height {tip_height}"
        ));
    }
    let mut current = tip;
    let mut height = tip_height;
    while height > target_height {
        let link = fetch_header_link(client, current, cache).await?;
        if link.parent == current {
            return Err(format!("self-parented block at height {height}"));
        }
        current = link.parent;
        height -= 1;
    }
    Ok(current)
}
pub(super) async fn walk_between_finalized_blocks(
    client: &NodeHttpClient,
    start: HeaderId,
    start_slot: u64,
    target: HeaderId,
    target_slot: u64,
    cache: &mut HeaderCache,
) -> AncestryPath {
    if let Some(path) = proven_below_target_slot_path(start_slot, target_slot) {
        return path;
    }
    walk_to_target(client, start, target, target_slot, cache).await
}
