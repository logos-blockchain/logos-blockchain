use std::{collections::HashMap, pin::Pin, str::FromStr as _, sync::Arc, time::Duration};

use futures::Stream;
use lb_chain_broadcast_service::{ActiveProviders, BlockBroadcastMsg, BlockBroadcastService};
use lb_core::{
    block::genesis::{GenesisBlock, GenesisBlockBuilder},
    mantle::{
        CryptarchiaParameter, GenesisTx as _, MantleTx, Note, Op, OpProof, SignedMantleTx,
        Transaction as _,
        genesis_tx::GenesisTx,
        ops::{
            channel::{ChannelId, Ed25519PublicKey, MsgId, inscribe::InscriptionOp},
            sdp::SDPDeclareOp,
        },
    },
    sdp::{Locator, MinStake, ServiceParameters, ServiceType},
};
use lb_cryptarchia_engine::{EpochConfig, time::SlotConfig};
use lb_groth16::{CompressedGroth16Proof, Field as _, Fr};
use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519Signature, ZkKey, ZkSignature};
use lb_ledger::mantle::{
    self,
    sdp::{ServiceRewardsParameters, rewards::blend},
};
use lb_storage_service::{
    StorageService,
    backends::rocksdb::{RocksBackend, RocksBackendSettings},
};
use lb_time_service::{TimeService, TimeServiceSettings, backends::SystemTimeBackend};
use lb_tracing_service::{
    ConsoleLayerSettings, FilterLayerSettings, LoggerLayerSettings, MetricsLayerSettings, Tracing,
    TracingLayerSettings, TracingSettings,
};
use lb_utils::math::NonNegativeRatio;
use overwatch::{
    derive_services,
    overwatch::{Overwatch, OverwatchHandle, OverwatchRunner},
};
use tempfile::TempDir;
use time::OffsetDateTime;
use tokio::sync::oneshot;
use tracing::Level;

use crate::{
    BootstrapConfig, CryptarchiaConsensus, CryptarchiaSettings, OfflineGracePeriodConfig,
    StartingState,
};

#[derive_services]
pub struct Node {
    chain: CryptarchiaConsensus<SignedMantleTx, RocksBackend, SystemTimeBackend, RuntimeServiceId>,
    block_broadcast: BlockBroadcastService<RuntimeServiceId>,
    storage: StorageService<RocksBackend, RuntimeServiceId>,
    time: TimeService<SystemTimeBackend, RuntimeServiceId>,
    tracing: Tracing<RuntimeServiceId>,
}

pub fn run_node(settings: NodeServiceSettings) -> Overwatch<RuntimeServiceId> {
    let overwatch = OverwatchRunner::<Node>::run(settings, None).unwrap();
    overwatch
        .runtime()
        .handle()
        .block_on(overwatch.handle().start_all_services())
        .unwrap();
    overwatch
}

pub fn shutdown_node(overwatch: Overwatch<RuntimeServiceId>) {
    overwatch
        .runtime()
        .handle()
        .block_on(overwatch.handle().shutdown())
        .unwrap();
    overwatch.blocking_wait_finished();
}

pub fn node_config(genesis_block: GenesisBlock) -> (NodeServiceSettings, TempDir) {
    let tempdir = TempDir::new().unwrap();

    let epoch_config = EpochConfig {
        epoch_stake_distribution_stabilization: 1.try_into().unwrap(),
        epoch_period_nonce_buffer: 1.try_into().unwrap(),
        epoch_period_nonce_stabilization: 1.try_into().unwrap(),
    };
    let consensus_config = lb_cryptarchia_engine::Config::new(
        2.try_into().unwrap(),
        NonNegativeRatio::new(1, 2.try_into().unwrap()),
        0.5.try_into().unwrap(),
    );
    let base_period_length = consensus_config.base_period_length();
    let epoch_length = epoch_config.epoch_length(base_period_length);
    let genesis_time = genesis_block
        .genesis_tx()
        .cryptarchia_parameter()
        .genesis_time;

    (
        NodeServiceSettings {
            chain: CryptarchiaSettings {
                config: lb_ledger::Config {
                    epoch_config,
                    consensus_config,
                    sdp_config: mantle::sdp::Config {
                        service_params: Arc::new(HashMap::from_iter([(
                            ServiceType::BlendNetwork,
                            ServiceParameters {
                                lock_period: 10.into(),
                                inactivity_period: 10.into(),
                                retention_period: 10.into(),
                                epoch: 0.into(),
                            },
                        )])),
                        service_rewards_params: ServiceRewardsParameters {
                            blend: blend::RewardsParameters {
                                rounds_per_session: epoch_length.try_into().unwrap(),
                                message_frequency_per_round: 1.0.try_into().unwrap(),
                                num_blend_layers: 1.try_into().unwrap(),
                                data_replication_factor: 0,
                                minimum_network_size: 1.try_into().unwrap(),
                                activity_threshold_sensitivity: 1,
                            },
                        },
                        min_stake: MinStake {
                            threshold: 1,
                            timestamp: 0,
                        },
                    },
                    faucet_pk: None,
                },
                starting_state: StartingState::Genesis {
                    genesis_block: Box::new(genesis_block),
                },
                recovery_file: tempdir.path().join("chain-service.json"),
                bootstrap: BootstrapConfig {
                    prolonged_bootstrap_period: Duration::from_secs(1),
                    force_bootstrap: false,
                    offline_grace_period: OfflineGracePeriodConfig {
                        grace_period: Duration::from_mins(1),
                        state_recording_interval: Duration::from_secs(30),
                    },
                },
            },
            block_broadcast: (),
            storage: RocksBackendSettings {
                db_path: tempdir.path().join("db"),
                read_only: false,
                column_family: None,
            },
            time: TimeServiceSettings {
                slot_config: SlotConfig {
                    slot_duration: Duration::from_secs(1),
                    genesis_time,
                },
                epoch_config,
                base_period_length,
                backend: (),
            },
            tracing: TracingSettings {
                logger: LoggerLayerSettings {
                    file: None,
                    stdout: true,
                    stderr: false,
                    loki: None,
                    gelf: None,
                    otlp: None,
                },
                tracing: TracingLayerSettings::None,
                filter: FilterLayerSettings::None,
                metrics: MetricsLayerSettings::None,
                console: ConsoleLayerSettings::None,
                level: Level::DEBUG,
            },
        },
        tempdir,
    )
}

pub fn genesis_block(
    zk_key: &ZkKey,
    ed_key: &Ed25519Key,
    note_count: usize,
    declaration_count: usize,
) -> GenesisBlock {
    let genesis_block = GenesisBlockBuilder::new()
        .add_notes((1..=note_count).map(|i| Note::new(i as u64 * 10, zk_key.to_public_key())))
        .set_inscription(InscriptionOp {
            channel_id: ChannelId::from([0; 32]),
            inscription: CryptarchiaParameter {
                chain_id: "test-chain".into(),
                genesis_time: OffsetDateTime::now_utc(),
                epoch_nonce: Fr::ZERO,
            }
            .encode(),
            parent: MsgId::root(),
            signer: Ed25519PublicKey::from_bytes(&[0; 32]).unwrap(),
        })
        .build()
        .unwrap();

    let transfer_op = genesis_block.genesis_tx().genesis_transfer().clone();
    let mut ops = vec![
        Op::Transfer(transfer_op.clone()),
        Op::ChannelInscribe(genesis_block.genesis_tx().genesis_inscription().clone()),
    ];
    let locked_notes = (0..declaration_count)
        .map(|i| {
            let locked_note = transfer_op.outputs.utxo_by_index(i, &transfer_op).unwrap();
            ops.push(Op::SDPDeclare(SDPDeclareOp {
                service_type: ServiceType::BlendNetwork,
                locators: vec![
                    Locator::from_str(format!("/ip4/198.51.100.{i}/tcp/4242").as_str()).unwrap(),
                ],
                provider_id: ed_key.public_key().into(),
                zk_id: zk_key.to_public_key(),
                locked_note_id: locked_note.id(),
            }));
            locked_note
        })
        .collect::<Vec<_>>();
    let mantle_tx = MantleTx(ops);
    let mantle_tx_hash = mantle_tx.hash();

    let mut ops_proofs = vec![
        OpProof::ZkSig(ZkSignature::new(CompressedGroth16Proof::from_bytes(
            &[0; _],
        ))),
        OpProof::Ed25519Sig(Ed25519Signature::zero()),
    ];
    for _ in &locked_notes {
        ops_proofs.push(OpProof::ZkAndEd25519Sigs {
            zk_sig: ZkKey::multi_sign(&[zk_key.clone(), zk_key.clone()], &mantle_tx_hash.to_fr())
                .unwrap(),
            ed25519_sig: ed_key.sign_payload(mantle_tx_hash.as_signing_bytes().as_ref()),
        });
    }

    GenesisBlockBuilder::new()
        .with_genesis_tx(
            GenesisTx::from_tx(SignedMantleTx {
                mantle_tx,
                ops_proofs,
            })
            .unwrap(),
        )
        .build()
}

pub async fn subscribe_to_sdp_snapshots(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Pin<Box<dyn Stream<Item = ActiveProviders>>> {
    let relay = handle.relay::<BlockBroadcastService<_>>().await.unwrap();
    let (tx, rx) = oneshot::channel();
    relay
        .send(BlockBroadcastMsg::SubscribeBlendProviders { result_sender: tx })
        .await
        .unwrap();
    Box::pin(rx.await.unwrap())
}
