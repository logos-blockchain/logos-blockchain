use std::time::Duration;

use futures::StreamExt as _;
use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};

use crate::tests::node::{
    genesis_block, node_config, run_node, shutdown_node, subscribe_to_sdp_snapshots,
};

/// - The snapshot for epoch 0 and 1 must be from genesis block.
/// - The snapshot for epoch 2+ must be from genesis block if no block was
///   added.
#[test]
fn test_yield_sdp_snapshot_from_genesis() {
    let (settings, _tempdir) = node_config(genesis_block(
        &ZkKey::zero(),
        &Ed25519Key::from_bytes(&[0; _]),
        3,
        1,
    ));

    let epoch_duration = settings.time.slot_config.slot_duration
        * u32::try_from(settings.chain.config.epoch_length()).unwrap();

    let overwatch = run_node(settings);

    let handle = overwatch.handle().clone();
    overwatch.runtime().handle().block_on(async move {
        let mut snapshot_stream = subscribe_to_sdp_snapshots(&handle).await;

        // 1st snapshot must be yielded almost immediately.
        // Snapshot must contain the providers in the genesis block.
        let first = tokio::time::timeout(Duration::from_secs(1), snapshot_stream.next())
            .await
            .expect("1st snapshot must be yielded almost immediately")
            .expect("stream ended unexpectedly");
        assert_eq!(first.epoch, 0);
        assert_eq!(first.providers.len(), 1);

        // 2nd snapshot must be yielded after an epoch has elapsed.
        // Snapshot must contain the providers in the genesis block.
        let margin = Duration::from_millis(100);
        let second = tokio::time::timeout(epoch_duration + margin, snapshot_stream.next())
            .await
            .expect("2nd snapshot not received in time")
            .expect("stream ended unexpectedly");
        assert_eq!(second.epoch, 1);
        assert_eq!(second.providers, first.providers);

        // 3rd snapshot must be yielded after another epoch has elapsed.
        // Snapshot must contain the providers in the genesis block,
        // because no declaration has been added since genesis.
        let third = tokio::time::timeout(epoch_duration + margin, snapshot_stream.next())
            .await
            .expect("3rd snapshot not received in time")
            .expect("stream ended unexpectedly");
        assert_eq!(third.epoch, 2);
        assert_eq!(third.providers, first.providers);
    });

    shutdown_node(overwatch);
}

// TODO: write more tests by adding blocks
