use core::{num::NonZeroUsize, time::Duration};
use std::collections::HashSet;

use chain_leader::LeaderConfig;
use chain_network::{IbdConfig, OrphanConfig, SyncConfig};
use chain_service::{OfflineGracePeriodConfig, StartingState};
use groth16::CompressedGroth16Proof;
use key_management_system_service::keys::{Ed25519Key, ZkKey, ZkPublicKey, ZkSignature};
use nomos_core::{
    mantle::{
        MantleTx, Note, OpProof, Utxo,
        genesis_tx::GenesisTx,
        ledger::Tx as LedgerTx,
        ops::{
            Op,
            channel::{ChannelId, Ed25519PublicKey, MsgId, inscribe::InscriptionOp},
        },
    },
    sdp::{DeclarationMessage, Locator, ProviderId, ServiceType},
};
use nomos_node::{
    SignedMantleTx, Transaction as _,
    config::cryptarchia::serde::{Config, NetworkConfig, ServiceConfig},
};
use num_bigint::BigUint;

pub const SHORT_PROLONGED_BOOTSTRAP_PERIOD: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct ProviderInfo {
    pub service_type: ServiceType,
    pub provider_sk: Ed25519Key,
    pub zk_sk: ZkKey,
    pub locator: Locator,
    pub note: ServiceNote,
}

impl ProviderInfo {
    #[must_use]
    pub fn provider_id(&self) -> ProviderId {
        ProviderId(self.provider_sk.public_key())
    }

    #[must_use]
    pub fn zk_id(&self) -> ZkPublicKey {
        self.zk_sk.to_public_key()
    }
}

/// General consensus configuration for a chosen participant, that later could
/// be converted into a specific service or services configuration.
#[derive(Clone)]
pub struct GeneralConsensusConfig {
    user_config: Config,
    genesis_tx: GenesisTx,
    pub utxos: Vec<Utxo>,
    pub blend_notes: Vec<ServiceNote>,
    pub da_notes: Vec<ServiceNote>,
}

impl GeneralConsensusConfig {
    pub fn override_genesis_tx(&mut self, genesis_tx: GenesisTx) {
        self.user_config.service.starting_state = StartingState::Genesis {
            genesis_tx: genesis_tx.clone(),
        };
        self.genesis_tx = genesis_tx;
    }

    #[must_use]
    pub const fn user_config(&self) -> &Config {
        &self.user_config
    }

    #[must_use]
    pub const fn genesis_tx(&self) -> &GenesisTx {
        &self.genesis_tx
    }
}

#[derive(Clone)]
pub struct ServiceNote {
    pub pk: ZkPublicKey,
    pub sk: ZkKey,
    pub note: Note,
    pub output_index: usize,
}

fn create_genesis_tx(utxos: &[Utxo]) -> GenesisTx {
    // Create a genesis inscription op (similar to config.yaml)
    let inscription = InscriptionOp {
        channel_id: ChannelId::from([0; 32]),
        inscription: vec![103, 101, 110, 101, 115, 105, 115], // "genesis" in bytes
        parent: MsgId::root(),
        signer: Ed25519PublicKey::from_bytes(&[0; 32]).unwrap(),
    };

    // Create ledger transaction with the utxos as outputs
    let outputs: Vec<Note> = utxos.iter().map(|u| u.note).collect();
    let ledger_tx = LedgerTx::new(vec![], outputs);

    // Create the mantle transaction
    let mantle_tx = MantleTx {
        ops: vec![Op::ChannelInscribe(inscription)],
        ledger_tx,
        execution_gas_price: 0,
        storage_gas_price: 0,
    };
    let signed_mantle_tx = SignedMantleTx {
        mantle_tx,
        ops_proofs: vec![OpProof::NoProof],
        ledger_tx_proof: ZkSignature::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
    };

    // Wrap in GenesisTx
    GenesisTx::from_tx(signed_mantle_tx).expect("Invalid genesis transaction")
}

#[must_use]
pub fn create_consensus_configs(
    ids: &[[u8; 32]],
    prolonged_bootstrap_period: Duration,
) -> Vec<GeneralConsensusConfig> {
    let mut leader_keys = Vec::new();
    let mut blend_notes = Vec::new();
    let mut da_notes = Vec::new();

    let utxos = create_utxos_for_leader_and_services(
        ids,
        &mut leader_keys,
        &mut blend_notes,
        &mut da_notes,
    );
    let genesis_tx = create_genesis_tx(&utxos);

    leader_keys
        .into_iter()
        .map(|(pk, sk)| GeneralConsensusConfig {
            blend_notes: blend_notes.clone(),
            da_notes: da_notes.clone(),
            genesis_tx: genesis_tx.clone(),
            utxos: utxos.clone(),
            user_config: Config {
                leader: LeaderConfig {
                    pk,
                    sk: sk.into_unsecured(),
                },
                network: NetworkConfig {
                    bootstrap: chain_network::BootstrapConfig {
                        ibd: IbdConfig {
                            delay_before_new_download: Duration::from_secs(10),
                            peers: HashSet::new(),
                        },
                    },
                    sync: SyncConfig {
                        orphan: OrphanConfig {
                            max_orphan_cache_size: NonZeroUsize::new(5)
                                .expect("Max orphan cache size must be non-zero"),
                        },
                    },
                },
                service: ServiceConfig {
                    bootstrap: chain_service::BootstrapConfig {
                        force_bootstrap: false,
                        offline_grace_period: OfflineGracePeriodConfig {
                            grace_period: Duration::from_secs(20 * 60),
                            state_recording_interval: Duration::from_secs(60),
                        },
                        prolonged_bootstrap_period,
                    },
                    recovery_file: "./recovery/cryptarchia.json".into(),
                    starting_state: StartingState::Genesis {
                        genesis_tx: genesis_tx.clone(),
                    },
                },
            },
        })
        .collect()
}

fn create_utxos_for_leader_and_services(
    ids: &[[u8; 32]],
    leader_keys: &mut Vec<(ZkPublicKey, ZkKey)>,
    blend_notes: &mut Vec<ServiceNote>,
    da_notes: &mut Vec<ServiceNote>,
) -> Vec<Utxo> {
    let derive_key_material = |prefix: &[u8], id_bytes: &[u8]| -> [u8; 16] {
        let mut sk_data = [0; 16];
        let prefix_len = prefix.len();

        sk_data[..prefix_len].copy_from_slice(prefix);
        let remaining_len = 16 - prefix_len;
        sk_data[prefix_len..].copy_from_slice(&id_bytes[..remaining_len]);

        sk_data
    };

    let mut utxos = Vec::new();

    // Assume output index which will be set by the ledger tx.
    let mut output_index = 0;

    // Create notes for leader, Blend and DA declarations.
    for &id in ids {
        let sk_leader_data = derive_key_material(b"ld", &id);
        let sk_leader = ZkKey::from(BigUint::from_bytes_le(&sk_leader_data));
        let pk_leader = sk_leader.to_public_key();
        leader_keys.push((pk_leader, sk_leader));
        utxos.push(Utxo {
            note: Note::new(1_000, pk_leader),
            tx_hash: BigUint::from(0u8).into(),
            output_index: 0,
        });
        output_index += 1;

        let sk_da_data = derive_key_material(b"da", &id);
        let sk_da = ZkKey::from(BigUint::from_bytes_le(&sk_da_data));
        let pk_da = sk_da.to_public_key();
        let note_da = Note::new(1, pk_da);
        da_notes.push(ServiceNote {
            pk: pk_da,
            sk: sk_da,
            note: note_da,
            output_index,
        });
        utxos.push(Utxo {
            note: note_da,
            tx_hash: BigUint::from(0u8).into(),
            output_index: 0,
        });
        output_index += 1;

        let sk_blend_data = derive_key_material(b"bn", &id);
        let sk_blend = ZkKey::from(BigUint::from_bytes_le(&sk_blend_data));
        let pk_blend = sk_blend.to_public_key();
        let note_blend = Note::new(1, pk_blend);
        blend_notes.push(ServiceNote {
            pk: pk_blend,
            sk: sk_blend,
            note: note_blend,
            output_index,
        });
        utxos.push(Utxo {
            note: note_blend,
            tx_hash: BigUint::from(0u8).into(),
            output_index: 0,
        });
        output_index += 1;
    }

    utxos
}

#[must_use]
pub fn create_genesis_tx_with_declarations(
    ledger_tx: LedgerTx,
    providers: Vec<ProviderInfo>,
) -> GenesisTx {
    let inscription = InscriptionOp {
        channel_id: ChannelId::from([0; 32]),
        inscription: vec![103, 101, 110, 101, 115, 105, 115], // "genesis" in bytes
        parent: MsgId::root(),
        signer: Ed25519PublicKey::from_bytes(&[0; 32]).unwrap(),
    };

    let ledger_tx_hash = ledger_tx.hash();

    let mut ops = vec![Op::ChannelInscribe(inscription)];

    for provider in &providers {
        let utxo = Utxo {
            tx_hash: ledger_tx_hash,
            output_index: provider.note.output_index,
            note: provider.note.note,
        };
        let declaration = DeclarationMessage {
            service_type: provider.service_type,
            locators: vec![provider.locator.clone()],
            provider_id: provider.provider_id(),
            zk_id: provider.zk_id(),
            locked_note_id: utxo.id(),
        };
        ops.push(Op::SDPDeclare(declaration));
    }

    let mantle_tx = MantleTx {
        ops,
        ledger_tx,
        execution_gas_price: 0,
        storage_gas_price: 0,
    };

    let mantle_tx_hash = mantle_tx.hash();
    let mut ops_proofs = vec![OpProof::NoProof];

    for provider in providers {
        let zk_sig =
            ZkKey::multi_sign(&[provider.note.sk, provider.zk_sk], mantle_tx_hash.as_ref())
                .unwrap();
        let ed25519_sig = provider
            .provider_sk
            .sign_payload(mantle_tx_hash.as_signing_bytes().as_ref());

        ops_proofs.push(OpProof::ZkAndEd25519Sigs {
            zk_sig,
            ed25519_sig,
        });
    }

    let signed_mantle_tx = SignedMantleTx {
        mantle_tx,
        ops_proofs,
        ledger_tx_proof: ZkSignature::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
    };

    GenesisTx::from_tx(signed_mantle_tx).expect("Invalid genesis transaction")
}
