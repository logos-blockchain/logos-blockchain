use std::{collections::HashMap, pin::Pin, str::FromStr as _, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::{Stream, stream};
use lb_chain_broadcast_service::BlockBroadcastMsg;
use lb_core::{
    mantle::ledger::NoteId,
    sdp::{
        Declaration, DeclarationMessage, Declarations, Locator, MinStake, ProviderId, ProviderInfo,
        ServiceParameters, ServiceType,
    },
};
use lb_cryptarchia_engine::{Epoch, EpochConfig, Slot};
use lb_cryptarchia_sync::HeaderId;
use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};
use lb_ledger::mantle::sdp::{ServiceRewardsParameters, rewards};
use lb_utils::math::NonNegativeRatio;
use overwatch::{DynError, services::relay::OutboundRelay};
use tokio::{sync::mpsc, time::timeout};

use crate::{
    relays::BroadcastRelay,
    sdp::{storage::Storage, take_and_broadcast_sdp_snapshot},
};

/// Minimal in-memory implementation of [`Storage`] for tests.
#[derive(Default)]
struct InMemoryStorage {
    // header_id → (slot, parent_id)
    chain: HashMap<HeaderId, (Slot, HeaderId)>,
    // header_id → declarations stored alongside that block
    declarations: HashMap<HeaderId, Declarations>,
}

impl InMemoryStorage {
    fn insert(&mut self, id: HeaderId, slot: Slot, parent: HeaderId, decls: Declarations) {
        self.chain.insert(id, (slot, parent));
        self.declarations.insert(id, decls);
    }
}

#[async_trait]
impl Storage for InMemoryStorage {
    async fn block_ids(
        &self,
        from_descendant: HeaderId,
    ) -> Pin<Box<dyn Stream<Item = (HeaderId, Slot)> + Send>> {
        let chain = self.chain.clone();
        Box::pin(stream::unfold(from_descendant, move |id| {
            let chain = chain.clone();
            async move {
                let (slot, parent) = *chain.get(&id)?;
                Some(((id, slot), parent))
            }
        }))
    }

    async fn sdp_declarations_at(&self, block: HeaderId) -> Result<Option<Declarations>, DynError> {
        Ok(self.declarations.get(&block).cloned())
    }
}

/// The snapshot for epoch 0~1 should be taken at the genesis block.
#[tokio::test]
async fn epoch_0_1_genesis_snapshot() {
    let config = config();
    let epoch_length = config.epoch_length();

    let genesis_id = id(0);
    let genesis_decls = decls(0, 0.into());
    let (relay, mut broadcast_receiver) = broadcast_relay();

    // Add a block to advance LIB.
    let mut storage = InMemoryStorage::default();
    let lib = id(1);
    storage.insert(
        lib,
        1.into(), // still epoch 0
        genesis_id,
        decls(1, 0.into()), // with different declarations
    );

    // Take a snapshot for epoch 0.
    // It should be taken at the genesis block.
    let snapshot =
        take_and_broadcast_sdp_snapshot(0.into(), lib, &genesis_decls, &config, &storage, &relay)
            .await
            .unwrap();
    assert_eq!(snapshot, genesis_decls);
    assert_broadcast(&mut broadcast_receiver, 0.into(), &genesis_decls).await;

    // Take a snapshot for epoch 1.
    // It should be taken at the genesis block.
    let snapshot =
        take_and_broadcast_sdp_snapshot(1.into(), lib, &genesis_decls, &config, &storage, &relay)
            .await
            .unwrap();
    assert_eq!(snapshot, genesis_decls);
    assert_broadcast(&mut broadcast_receiver, 1.into(), &genesis_decls).await;

    // Add a block to advance LIB.
    let lib = id(2);
    storage.insert(
        lib,
        Slot::from(epoch_length), // epoch 1
        genesis_id,
        decls(2, 1.into()), // with different declarations
    );

    // Take a snapshot for epoch 1 again.
    // It should be still taken at the genesis block.
    let snapshot =
        take_and_broadcast_sdp_snapshot(1.into(), lib, &genesis_decls, &config, &storage, &relay)
            .await
            .unwrap();
    assert_eq!(snapshot, genesis_decls);
    assert_broadcast(&mut broadcast_receiver, 1.into(), &genesis_decls).await;
}

/// For epoch 2+, if LIB is newer than the last block of `current_epoch-2`,
/// the snapshot should be taken at the last block of `current_epoch-2`.
#[tokio::test]
async fn snapshot_at_last_block_of_epoch_minus_2() {
    let config = config();
    let epoch_len = config.epoch_length();
    let (relay, mut rx) = broadcast_relay();

    let genesis_id = id(0);
    let genesis_decls = Declarations::default();

    let mut storage = InMemoryStorage::default();
    storage.insert(id(1), 1.into(), genesis_id, decls(0, 0.into())); // epoch 0
    storage.insert(id(2), epoch_len.into(), id(1), decls(1, 1.into())); // epoch 1
    storage.insert(id(3), (2 * epoch_len).into(), id(2), decls(2, 2.into())); // epoch 2
    storage.insert(id(4), (2 * epoch_len + 1).into(), id(3), decls(3, 2.into())); // epoch 2 <- last
    storage.insert(id(5), (3 * epoch_len).into(), id(4), decls(4, 3.into())); // epoch 3
    storage.insert(id(6), (4 * epoch_len).into(), id(5), decls(5, 4.into())); // epoch 4
    let lib = id(6); // LIB is in epoch 4.

    // Take a snapshot for epoch 4.
    let snapshot =
        take_and_broadcast_sdp_snapshot(4.into(), lib, &genesis_decls, &config, &storage, &relay)
            .await
            .unwrap();
    // snapshot should be taken at id(4)
    let expected = storage.sdp_declarations_at(id(4)).await.unwrap().unwrap();
    assert_eq!(snapshot, expected);
    assert_broadcast(&mut rx, 4.into(), &expected).await;
}

/// For epoch 2+, if LIB is older than the last block of `current_epoch-2`,
/// the snapshot should be taken at the last block of an older epoch
/// which is not newer than LIB.
#[tokio::test]
async fn snapshot_at_older_epoch_if_lib_is_old() {
    let config = config();
    let epoch_len = config.epoch_length();
    let (relay, mut rx) = broadcast_relay();

    let genesis_id = id(0);
    let genesis_decls = Declarations::default();

    let mut storage = InMemoryStorage::default();
    storage.insert(id(1), 1.into(), genesis_id, decls(0, 0.into())); // epoch 0
    storage.insert(id(2), epoch_len.into(), id(1), decls(1, 1.into())); // epoch 1
    storage.insert(id(3), (2 * epoch_len).into(), id(2), decls(2, 2.into())); // epoch 2 <- LIB
    storage.insert(id(4), (2 * epoch_len + 1).into(), id(3), decls(3, 2.into())); // epoch 2 <- last
    storage.insert(id(5), (3 * epoch_len).into(), id(4), decls(4, 3.into())); // epoch 3
    storage.insert(id(6), (4 * epoch_len).into(), id(5), decls(5, 4.into())); // epoch 4
    let lib = id(3); // LIB is in epoch 2 but older than the last block of epoch 2 (= cur_epoch-2)

    // Take a snapshot for epoch 4.
    let snapshot =
        take_and_broadcast_sdp_snapshot(4.into(), lib, &genesis_decls, &config, &storage, &relay)
            .await
            .unwrap();
    // snapshot should be taken at id(2) which is the last block of epoch 1
    let expected = storage.sdp_declarations_at(id(2)).await.unwrap().unwrap();
    assert_eq!(snapshot, expected);
    assert_broadcast(&mut rx, 4.into(), &expected).await;
}

// For epoch 2+, if there is no block in `current_epoch - 2`,
// the snapshot should be taken at the last block of the most recent older epoch.
#[tokio::test]
async fn snapshot_at_older_block_if_no_blocks_in_epoch_minus_2() {
    let config = config();
    let epoch_len = config.epoch_length();
    let (relay, mut rx) = broadcast_relay();

    let genesis_id = id(0);
    let genesis_decls = decls(0, 0.into());

    let mut storage = InMemoryStorage::default();
    storage.insert(id(1), 1.into(), genesis_id, decls(0, 0.into())); // epoch 0
    storage.insert(id(2), epoch_len.into(), id(1), decls(1, 1.into())); // epoch 1 <- LIB
    let lib = id(2); // LIB is at epoch 1.

    // Take a snapshot for epoch 4.
    let snapshot =
        take_and_broadcast_sdp_snapshot(4.into(), lib, &genesis_decls, &config, &storage, &relay)
            .await
            .unwrap();
    // snapshot should be taken at id(1) which is the last block of epoch 0
    // because there's no block in epoch 2 and there's no last block finalized in epoch 1.
    assert_eq!(snapshot, genesis_decls);
    assert_broadcast(&mut rx, 4.into(), &genesis_decls).await;
}

// For epoch 2+, if LIB is the last block of `current_epoch - 2`,
// the snapshot should be taken at LIB.
#[tokio::test]
async fn snapshot_at_lib() {
    let config = config();
    let epoch_len = config.epoch_length();
    let (relay, mut rx) = broadcast_relay();

    let genesis_id = id(0);
    let genesis_decls = decls(0, 0.into());

    let mut storage = InMemoryStorage::default();
    storage.insert(id(1), 1.into(), genesis_id, decls(0, 0.into())); // epoch 0
    storage.insert(id(2), epoch_len.into(), id(1), decls(1, 1.into())); // epoch 1
    storage.insert(id(3), config.last_slot(2.into()), id(2), decls(2, 2.into())); // epoch 2 <- LIB and last
    let lib = id(3); // LIB is the last block of epoch 2 because it's on the last slot of the epoch

    // Take a snapshot for epoch 4.
    let snapshot =
        take_and_broadcast_sdp_snapshot(4.into(), lib, &genesis_decls, &config, &storage, &relay)
            .await
            .unwrap();
    // snapshot should be taken at LIB id(3) which is the last block of epoch 2.
    let expected = storage.sdp_declarations_at(id(3)).await.unwrap().unwrap();
    assert_eq!(snapshot, expected);
    assert_broadcast(&mut rx, 4.into(), &expected).await;
}

fn id(byte: u8) -> HeaderId {
    [byte; 32].into()
}

fn decls(marker: u8, epoch: Epoch) -> Declarations {
    let decl = DeclarationMessage {
        service_type: ServiceType::BlendNetwork,
        locators: Locator::from_str("/ip4/1.1.1.1/udp/7777").unwrap().into(),
        provider_id: ProviderId(Ed25519Key::from_bytes(&[marker; 32]).public_key()),
        zk_id: ZkKey::from(lb_groth16::Fr::from(u64::from(marker))).to_public_key(),
        locked_note_id: NoteId(lb_groth16::Fr::from(u64::from(marker))),
    };
    Declarations::from_iter([(
        ServiceType::BlendNetwork,
        HashMap::from_iter([(decl.id(), Declaration::new(epoch, &decl))]),
    )])
}

fn broadcast_relay() -> (BroadcastRelay, mpsc::Receiver<BlockBroadcastMsg>) {
    let (tx, rx) = mpsc::channel::<BlockBroadcastMsg>(8);
    (OutboundRelay::new(tx), rx)
}

async fn assert_broadcast(
    rx: &mut mpsc::Receiver<BlockBroadcastMsg>,
    expected_epoch: Epoch,
    expected_decls: &Declarations,
) {
    let msg = timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("broadcast should arrive")
        .expect("broadcast channel closed");
    let BlockBroadcastMsg::BroadcastBlendProviders(active) = msg else {
        panic!("expected BroadcastBlendProviders, got {msg:?}");
    };
    assert_eq!(active.epoch, expected_epoch);
    assert_eq!(active.providers, to_providers(expected_decls));
}

fn to_providers(decls: &Declarations) -> HashMap<ProviderId, ProviderInfo> {
    decls
        .iter()
        .find(|(svc, _)| **svc == ServiceType::BlendNetwork)
        .map(|(_, m)| {
            m.values()
                .map(|d| {
                    (
                        d.provider_id,
                        ProviderInfo {
                            locators: d.locators.clone(),
                            zk_id: d.zk_id,
                        },
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn config() -> lb_ledger::Config {
    let mut service_params = HashMap::new();
    service_params.insert(
        ServiceType::BlendNetwork,
        ServiceParameters {
            lock_period: 10.into(),
            inactivity_period: 1.into(),
            retention_period: 1.into(),
            epoch: 0.into(),
        },
    );
    let epoch_config = EpochConfig {
        epoch_stake_distribution_stabilization: 1.try_into().unwrap(),
        epoch_period_nonce_buffer: 1.try_into().unwrap(),
        epoch_period_nonce_stabilization: 1.try_into().unwrap(),
    };
    let consensus_config = lb_cryptarchia_engine::Config::new(
        1.try_into().unwrap(),
        NonNegativeRatio::new(1, 2.try_into().unwrap()),
        1.0.try_into().unwrap(),
    );
    let epoch_length = epoch_config.epoch_length(consensus_config.base_period_length());
    lb_ledger::Config {
        epoch_config,
        consensus_config,
        sdp_config: lb_ledger::mantle::sdp::Config {
            service_params: Arc::new(service_params),
            service_rewards_params: ServiceRewardsParameters {
                blend: rewards::blend::RewardsParameters {
                    rounds_per_session: epoch_length.try_into().unwrap(),
                    message_frequency_per_round: 1.0.try_into().unwrap(),
                    num_blend_layers: 3.try_into().unwrap(),
                    minimum_network_size: 1.try_into().unwrap(),
                    data_replication_factor: 0,
                    activity_threshold_sensitivity: 1,
                },
            },
            min_stake: MinStake {
                threshold: 1,
                timestamp: 0,
            },
        },
        faucet_pk: None,
    }
}
