pub mod locked_notes;
pub mod rewards;

use std::collections::HashMap;

use key_management_system_keys::keys::{Ed25519Signature, ZkPublicKey, ZkSignature};
use locked_notes::LockedNotes;
use nomos_blend_message::crypto::proofs::RealProofsVerifier;
use nomos_core::{
    block::BlockNumber,
    mantle::{
        Note, NoteId, OpProof, TxHash, Utxo,
        ops::sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
    },
    sdp::{
        Declaration, DeclarationId, MinStake, Nonce, ProviderId, ProviderInfo, ServiceParameters,
        ServiceType, SessionNumber,
    },
};
use rewards::{Error as RewardsError, Rewards};

use crate::{
    EpochState, UtxoTree,
    mantle::sdp::rewards::{blend, da},
};

type Declarations = rpds::RedBlackTreeMapSync<DeclarationId, Declaration>;

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq)]
enum Service {
    DataAvailability(ServiceState<da::Rewards>),
    BlendNetwork(ServiceState<blend::Rewards<RealProofsVerifier>>),
}

impl Service {
    fn try_apply_header(
        self,
        block_number: u64,
        epoch_state: &EpochState,
        config: &ServiceParameters,
    ) -> (Self, Vec<Utxo>) {
        match self {
            Self::DataAvailability(state) => {
                let (new_state, utxos) = state.try_apply_header(block_number, epoch_state, config);
                (Self::DataAvailability(new_state), utxos)
            }
            Self::BlendNetwork(state) => {
                let (new_state, utxos) = state.try_apply_header(block_number, epoch_state, config);
                (Self::BlendNetwork(new_state), utxos)
            }
        }
    }

    fn declare(&mut self, id: DeclarationId, declaration: Declaration) -> Result<(), Error> {
        match self {
            Self::DataAvailability(state) => state.declare(id, declaration),
            Self::BlendNetwork(state) => state.declare(id, declaration),
        }
    }

    fn active(
        &mut self,
        active: &SDPActiveOp,
        block_number: BlockNumber,
        locked_notes: &LockedNotes,
        sig: &ZkSignature,
        tx_hash: TxHash,
    ) -> Result<(), Error> {
        match self {
            Self::DataAvailability(state) => {
                state.active(active, block_number, locked_notes, sig, tx_hash)
            }
            Self::BlendNetwork(state) => {
                state.active(active, block_number, locked_notes, sig, tx_hash)
            }
        }
    }

    fn withdraw(
        &mut self,
        withdraw: &SDPWithdrawOp,
        block_number: BlockNumber,
        locked_notes: &mut LockedNotes,
        sig: &ZkSignature,
        tx_hash: TxHash,
        config: &ServiceParameters,
    ) -> Result<(), Error> {
        match self {
            Self::DataAvailability(state) => {
                state.withdraw(withdraw, block_number, locked_notes, sig, tx_hash, config)
            }
            Self::BlendNetwork(state) => {
                state.withdraw(withdraw, block_number, locked_notes, sig, tx_hash, config)
            }
        }
    }

    fn contains(&self, declaration_id: &DeclarationId) -> bool {
        match self {
            Self::DataAvailability(state) => state.contains(declaration_id),
            Self::BlendNetwork(state) => state.contains(declaration_id),
        }
    }

    const fn active_session(&self) -> &SessionState {
        match self {
            Self::DataAvailability(state) => &state.active,
            Self::BlendNetwork(state) => &state.active,
        }
    }

    #[cfg(test)]
    const fn forming_session(&self) -> &SessionState {
        match self {
            Self::DataAvailability(state) => &state.forming,
            Self::BlendNetwork(state) => &state.forming,
        }
    }

    const fn declarations(&self) -> &Declarations {
        match self {
            Self::DataAvailability(state) => &state.declarations,
            Self::BlendNetwork(state) => &state.declarations,
        }
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub service_params: std::sync::Arc<HashMap<ServiceType, ServiceParameters>>,
    pub service_rewards_params: ServiceRewardsParameters,
    pub min_stake: MinStake,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceRewardsParameters {
    pub blend: blend::RewardsParameters,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    // #[error("Invalid Sdp state transition: {0:?}")]
    // SdpStateError(#[from] DeclarationStateError),
    #[error("Sdp declaration id not found: {0:?}")]
    DeclarationNotFound(DeclarationId),
    #[error("Locked period did not pass yet")]
    WithdrawalWhileLocked,
    #[error("Invalid sdp message nonce: {0:?}")]
    InvalidNonce(Nonce),
    #[error("Service not found: {0:?}")]
    ServiceNotFound(ServiceType),
    #[error("Duplicate sdp declaration id: {0:?}")]
    DuplicateDeclaration(DeclarationId),
    #[error("Active session for service {0:?} not found")]
    ActiveSessionNotFound(ServiceType),
    #[error("Forming session for service {0:?} not found")]
    FormingSessionNotFound(ServiceType),
    #[error("Session parameters for {0:?} not found")]
    SessionParamsNotFound(ServiceType),
    #[error("Service parameters are missing for {0:?}")]
    ServiceParamsNotFound(ServiceType),
    #[error("Can't update genesis state during different block number")]
    NotGenesisBlock,
    #[error("Time travel detected, current: {current:?}, incoming: {incoming:?}")]
    TimeTravel {
        current: BlockNumber,
        incoming: BlockNumber,
    },
    #[error("Something went wrong while locking/unlocking a note: {0:?}")]
    LockingError(#[from] locked_notes::Error),
    #[error("Invalid signature")]
    InvalidSignature,
    #[error("Note not found: {0:?}")]
    NoteNotFound(NoteId),
    #[error("Invalid proof")]
    InvalidProof,
    #[error("Error while computing rewards: {0:?}")]
    RewardsError(#[from] RewardsError),
}

// State at the beginning of this session
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionState {
    pub declarations: Declarations,
    pub session_n: u64,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ServiceState<R: Rewards> {
    // state of declarations at block b
    declarations: Declarations,
    // (current) active session
    // snapshot of `declarations` at the start of block ((b // config.session_duration) - 1) *
    // config.session_duration
    active: SessionState,
    // new forming session, overlaps with `declarations` until the next session boundary
    // snapshot of `declarations` at the start of block (b // config.session_duration) *
    // config.session_duration
    forming: SessionState,
    // rewards calculation and tracking for this service
    rewards: R,
}

impl SessionState {
    fn update<R: Rewards>(
        &self,
        service_state: &ServiceState<R>,
        block_number: u64,
        config: &ServiceParameters,
    ) -> Self {
        if self.session_n.saturating_sub(1) * config.session_duration > block_number {
            return Self {
                session_n: self.session_n,
                declarations: service_state.declarations.clone(),
            };
        }
        self.clone()
    }
}

const fn is_active(
    declaration: &Declaration,
    current_block: u64,
    config: &ServiceParameters,
) -> bool {
    declaration.active
        + (config.inactivity_period + config.retention_period) * config.session_duration
        >= current_block
}

impl<R: Rewards> ServiceState<R> {
    fn try_apply_header(
        mut self,
        block_number: u64,
        epoch_state: &EpochState,
        config: &ServiceParameters,
    ) -> (Self, Vec<Utxo>) {
        let current_session = config.session_for_block(block_number);
        let reward_utxos;

        // shift all session!
        if current_session == self.active.session_n + 1 {
            // Remove expired declarations based on retention_period
            // This essentially duplicates the declaration set so it's only triggered at
            // session boundaries
            self.declarations = self
                .declarations
                .iter()
                .filter(|(_id, declaration)| is_active(declaration, block_number, config))
                .map(|(id, declaration)| (*id, declaration.clone()))
                .collect();

            // Update rewards with current session state and distribute rewards
            (self.rewards, reward_utxos) =
                self.rewards
                    .update_session(&self.active, epoch_state, config);
            self.active = self.forming.clone();
            self.forming = SessionState {
                declarations: self.declarations.clone(),
                session_n: self.forming.session_n + 1,
            };
        } else {
            assert!(
                current_session < self.active.session_n + 1,
                "Nomos isn't ready for time travel yet"
            );
            self.rewards = self.rewards.update_epoch(epoch_state);
            self.forming = self.forming.update(&self, block_number, config);
            reward_utxos = Vec::new();
        }

        (self, reward_utxos)
    }

    fn declare(&mut self, id: DeclarationId, declaration: Declaration) -> Result<(), Error> {
        if self.declarations.contains_key(&id) {
            return Err(Error::DuplicateDeclaration(id));
        }
        self.declarations = self.declarations.insert(id, declaration);
        Ok(())
    }

    fn active(
        &mut self,
        active: &SDPActiveOp,
        block_number: BlockNumber,
        locked_notes: &LockedNotes,
        sig: &ZkSignature,
        tx_hash: TxHash,
    ) -> Result<(), Error> {
        let Some(declaration) = self.declarations.get_mut(&active.declaration_id) else {
            return Err(Error::DeclarationNotFound(active.declaration_id));
        };

        if active.nonce <= declaration.nonce {
            return Err(Error::InvalidNonce(active.nonce));
        }
        declaration.active = block_number;
        let note = locked_notes
            .get(&declaration.locked_note_id)
            .ok_or(Error::LockingError(locked_notes::Error::NoteNotLocked(
                declaration.locked_note_id,
            )))?;

        if !ZkPublicKey::verify_multi(&[note.pk, declaration.zk_id], &tx_hash.0, sig) {
            return Err(Error::InvalidSignature);
        }

        // TODO: check service specific logic

        // Update rewards with active message metadata
        self.rewards =
            self.rewards
                .update_active(declaration.provider_id, &active.metadata, block_number)?;

        Ok(())
    }

    fn withdraw(
        &mut self,
        withdraw: &SDPWithdrawOp,
        block_number: BlockNumber,
        locked_notes: &mut LockedNotes,
        sig: &ZkSignature,
        tx_hash: TxHash,
        config: &ServiceParameters,
    ) -> Result<(), Error> {
        let Some(declaration) = self.declarations.get(&withdraw.declaration_id) else {
            return Err(Error::DeclarationNotFound(withdraw.declaration_id));
        };
        if withdraw.nonce <= declaration.nonce {
            return Err(Error::InvalidNonce(withdraw.nonce));
        }

        if declaration.created + config.lock_period >= block_number {
            return Err(Error::WithdrawalWhileLocked);
        }

        let note = locked_notes.unlock(declaration.service_type, &declaration.locked_note_id)?;

        if !ZkPublicKey::verify_multi(&[note.pk, declaration.zk_id], &tx_hash.0, sig) {
            return Err(Error::InvalidSignature);
        }
        self.declarations = self.declarations.remove(&withdraw.declaration_id);
        Ok(())
    }

    fn contains(&self, declaration_id: &DeclarationId) -> bool {
        self.declarations.contains_key(declaration_id)
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SdpLedger {
    services: rpds::HashTrieMapSync<ServiceType, Service>,
    locked_notes: LockedNotes,
    block_number: u64,
}

impl SdpLedger {
    #[must_use]
    pub fn new() -> Self {
        Self {
            services: rpds::HashTrieMapSync::new_sync(),
            locked_notes: LockedNotes::new(),
            block_number: 0,
        }
    }

    pub fn from_genesis<'a>(
        config: &Config,
        utxo_tree: &UtxoTree,
        epoch_state: &EpochState,
        tx_hash: TxHash,
        ops: impl Iterator<Item = (&'a SDPDeclareOp, &'a OpProof)> + 'a,
    ) -> Result<Self, Error> {
        let mut sdp = Self::new()
            .with_blend_service(config.service_rewards_params.blend.clone(), epoch_state)
            .with_da_service();

        for (op, proof) in ops {
            let OpProof::ZkAndEd25519Sigs {
                zk_sig,
                ed25519_sig,
            } = proof
            else {
                return Err(Error::InvalidProof);
            };
            let Some((utxo, _)) = utxo_tree.utxos().get(&op.locked_note_id) else {
                return Err(Error::NoteNotFound(op.locked_note_id));
            };
            sdp = sdp.apply_declare_msg(op, utxo.note, zk_sig, ed25519_sig, tx_hash, config)?;
        }

        let blend = sdp
            .services
            .get_mut(&ServiceType::BlendNetwork)
            .expect("SDP initialized with Blend in this method");

        if let Service::BlendNetwork(state) = blend {
            state.active.declarations = state.declarations.clone();
            state.forming.declarations = state.declarations.clone();
        }

        let da = sdp
            .services
            .get_mut(&ServiceType::DataAvailability)
            .expect("SDP initialized with DA in this method");

        if let Service::DataAvailability(state) = da {
            state.active.declarations = state.declarations.clone();
            state.forming.declarations = state.declarations.clone();
        }

        Ok(sdp)
    }

    #[must_use]
    pub fn with_da_service(mut self) -> Self {
        let service = Service::DataAvailability(Self::new_service_state(da::Rewards::default()));
        self.services = self.services.insert(ServiceType::DataAvailability, service);
        self
    }

    #[must_use]
    pub fn with_blend_service(
        mut self,
        rewards_settings: blend::RewardsParameters,
        epoch_state: &EpochState,
    ) -> Self {
        let service = Service::BlendNetwork(Self::new_service_state(blend::Rewards::new(
            rewards_settings,
            epoch_state,
        )));
        self.services = self.services.insert(ServiceType::BlendNetwork, service);
        self
    }

    #[must_use]
    fn new_service_state<R: Rewards>(rewards: R) -> ServiceState<R> {
        ServiceState {
            declarations: rpds::RedBlackTreeMapSync::new_sync(),
            active: SessionState {
                declarations: rpds::RedBlackTreeMapSync::new_sync(),
                session_n: 0,
            },

            forming: SessionState {
                declarations: rpds::RedBlackTreeMapSync::new_sync(),
                session_n: 1,
            },
            rewards,
        }
    }

    pub fn try_apply_header(
        &self,
        config: &Config,
        epoch_state: &EpochState,
    ) -> Result<(Self, Vec<Utxo>), Error> {
        let block_number = self.block_number + 1; // overflow?
        let mut all_reward_utxos = Vec::new();

        let services = self
            .services
            .iter()
            .map(|(service, service_state)| {
                let config = config
                    .service_params
                    .get(service)
                    .ok_or(Error::SessionParamsNotFound(*service))?;
                let (new_state, reward_utxos) =
                    service_state
                        .clone()
                        .try_apply_header(block_number, epoch_state, config);
                all_reward_utxos.extend(reward_utxos.into_iter());
                Ok::<_, Error>((*service, new_state))
            })
            .collect::<Result<_, _>>()?;

        Ok((
            Self {
                block_number,
                services,
                locked_notes: self.locked_notes.clone(),
            },
            all_reward_utxos,
        ))
    }

    pub fn apply_declare_msg(
        mut self,
        op: &SDPDeclareOp,
        note: Note,
        zk_sig: &ZkSignature,
        ed25519_sig: &Ed25519Signature,
        tx_hash: TxHash,
        config: &Config,
    ) -> Result<Self, Error> {
        if !ZkPublicKey::verify_multi(&[note.pk, op.zk_id], &tx_hash.0, zk_sig) {
            return Err(Error::InvalidSignature);
        }
        op.provider_id
            .0
            .verify(tx_hash.as_signing_bytes().as_ref(), ed25519_sig)
            .map_err(|_| Error::InvalidSignature)?;

        let declaration_id = op.id();
        let declaration = Declaration::new(self.block_number, op);
        if let Some(service_state) = self.services.get_mut(&op.service_type) {
            service_state.declare(declaration_id, declaration)?;
            self.locked_notes = self.locked_notes.lock(
                &config.min_stake,
                op.service_type,
                note,
                &op.locked_note_id,
            )?;
        } else {
            return Err(Error::ServiceNotFound(op.service_type));
        }

        Ok(self)
    }

    pub fn apply_active_msg(
        mut self,
        op: &SDPActiveOp,
        zksig: &ZkSignature,
        tx_hash: TxHash,
        config: &Config,
    ) -> Result<Self, Error> {
        let (service, _) = self.get_service(&op.declaration_id, config)?;
        self.services.get_mut(&service).unwrap().active(
            op,
            self.block_number,
            &self.locked_notes,
            zksig,
            tx_hash,
        )?;

        Ok(self)
    }

    pub fn apply_withdrawn_msg(
        mut self,
        op: &SDPWithdrawOp,
        zksig: &ZkSignature,
        tx_hash: TxHash,
        config: &Config,
    ) -> Result<Self, Error> {
        let (service, config) = self.get_service(&op.declaration_id, config)?;
        self.services.get_mut(&service).unwrap().withdraw(
            op,
            self.block_number,
            &mut self.locked_notes,
            zksig,
            tx_hash,
            config,
        )?;

        Ok(self)
    }

    #[must_use]
    pub const fn locked_notes(&self) -> &LockedNotes {
        &self.locked_notes
    }

    #[must_use]
    pub fn active_session_providers(
        &self,
        service_type: ServiceType,
    ) -> Option<HashMap<ProviderId, ProviderInfo>> {
        let service = self.services.get(&service_type)?;

        let providers = service
            .active_session()
            .declarations
            .iter()
            .map(|(_, declaration)| {
                (
                    declaration.provider_id,
                    ProviderInfo {
                        locators: declaration.locators.clone(),
                        zk_id: declaration.zk_id,
                    },
                )
            })
            .collect();

        Some(providers)
    }

    #[must_use]
    pub fn active_sessions(&self) -> HashMap<ServiceType, SessionNumber> {
        self.services
            .iter()
            .map(|(service_type, service)| (*service_type, service.active_session().session_n))
            .collect()
    }

    #[must_use]
    pub fn declarations(&self) -> Vec<(DeclarationId, Declaration)> {
        self.services
            .iter()
            .flat_map(|(_, service_state)| {
                service_state
                    .declarations()
                    .iter()
                    .map(|(declaration_id, declaration)| (*declaration_id, declaration.clone()))
            })
            .collect()
    }

    #[must_use]
    pub fn get_declaration(&self, declaration_id: &DeclarationId) -> Option<&Declaration> {
        self.services.iter().find_map(|(_, service)| {
            let declarations = match service {
                Service::DataAvailability(state) => &state.declarations,
                Service::BlendNetwork(state) => &state.declarations,
            };
            declarations.get(declaration_id)
        })
    }

    fn get_service<'a>(
        &self,
        declaration_id: &DeclarationId,
        config: &'a Config,
    ) -> Result<(ServiceType, &'a ServiceParameters), Error> {
        let service = self
            .services
            .iter()
            .find(|(_, state)| state.contains(declaration_id))
            .map(|(service, _)| *service)
            .ok_or(Error::DeclarationNotFound(*declaration_id))?;

        let params = config
            .service_params
            .get(&service)
            .ok_or(Error::ServiceParamsNotFound(service))?;
        Ok((service, params))
    }

    #[cfg(test)]
    fn get_forming_session(&self, service_type: ServiceType) -> Option<&SessionState> {
        self.services
            .get(&service_type)
            .map(Service::forming_session)
    }

    #[cfg(test)]
    fn get_active_session(&self, service_type: ServiceType) -> Option<&SessionState> {
        self.services
            .get(&service_type)
            .map(Service::active_session)
    }

    #[cfg(test)]
    fn get_declarations(&self, service_type: ServiceType) -> Option<&Declarations> {
        self.services.get(&service_type).map(Service::declarations)
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU64, sync::Arc};

    use groth16::{Field as _, Fr};
    use key_management_system_keys::keys::{Ed25519Key, ZkKey};
    use nomos_core::crypto::ZkHash;
    use nomos_utils::math::NonNegativeF64;
    use num_bigint::BigUint;

    use super::*;
    use crate::cryptarchia::tests::{utxo, utxo_with_sk};

    fn setup() -> Config {
        let mut params = HashMap::new();
        params.insert(
            ServiceType::BlendNetwork,
            ServiceParameters {
                inactivity_period: 20,
                lock_period: 10,
                retention_period: 1000,
                timestamp: 0,
                session_duration: 10,
            },
        );
        params.insert(
            ServiceType::DataAvailability,
            ServiceParameters {
                inactivity_period: 10,
                lock_period: 5,
                retention_period: 100,
                timestamp: 0,
                session_duration: 5,
            },
        );

        Config {
            service_params: Arc::new(params),
            service_rewards_params: ServiceRewardsParameters {
                blend: blend::RewardsParameters {
                    rounds_per_session: NonZeroU64::new(10).unwrap(),
                    message_frequency_per_round: NonNegativeF64::try_from(1.0).unwrap(),
                    num_blend_layers: NonZeroU64::new(3).unwrap(),
                    minimum_network_size: NonZeroU64::new(1).unwrap(),
                },
            },
            min_stake: MinStake {
                threshold: 1,
                timestamp: 0,
            },
        }
    }

    fn create_zk_key(sk: u64) -> ZkKey {
        ZkKey::from(BigUint::from(sk))
    }

    fn create_signing_key() -> Ed25519Key {
        Ed25519Key::from_bytes(&[0; 32])
    }

    fn gc_test_config() -> Config {
        let mut params = HashMap::new();
        params.insert(
            ServiceType::DataAvailability,
            ServiceParameters {
                inactivity_period: 1,
                lock_period: 5,
                retention_period: 4,
                timestamp: 0,
                session_duration: 5,
            },
        );
        params.insert(
            ServiceType::BlendNetwork,
            ServiceParameters {
                inactivity_period: 9,
                lock_period: 5,
                retention_period: 1,
                timestamp: 0,
                session_duration: 5,
            },
        );

        Config {
            service_params: Arc::new(params),
            service_rewards_params: ServiceRewardsParameters {
                blend: blend::RewardsParameters {
                    rounds_per_session: NonZeroU64::new(864_000).unwrap(),
                    message_frequency_per_round: NonNegativeF64::try_from(1.0).unwrap(),
                    num_blend_layers: NonZeroU64::new(3).unwrap(),
                    minimum_network_size: NonZeroU64::new(1).unwrap(),
                },
            },
            min_stake: MinStake {
                threshold: 1,
                timestamp: 0,
            },
        }
    }

    fn apply_declare_with_dummies(
        sdp_ledger: SdpLedger,
        op: &SDPDeclareOp,
        zk_sk: &ZkKey,
        config: &Config,
    ) -> Result<SdpLedger, Error> {
        let (note_sk, utxo) = utxo_with_sk();
        let note = utxo.note;
        let tx_hash = TxHash(Fr::from(0u8));
        let zk_sig = ZkKey::multi_sign(&[note_sk, zk_sk.clone()], &tx_hash.0).unwrap();

        let signing_key = create_signing_key();
        let ed25519_sig = signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref());

        sdp_ledger.apply_declare_msg(op, note, &zk_sig, &ed25519_sig, tx_hash, config)
    }

    fn apply_withdraw_with_dummies(
        sdp_ledger: SdpLedger,
        op: &SDPWithdrawOp,
        note_sk: ZkKey,
        zk_key: ZkKey,
        config: &Config,
    ) -> Result<SdpLedger, Error> {
        let tx_hash = TxHash(Fr::from(1u8));
        let zk_sig = ZkKey::multi_sign(&[note_sk, zk_key], &tx_hash.0).unwrap();

        sdp_ledger.apply_withdrawn_msg(op, &zk_sig, tx_hash, config)
    }

    fn dummy_epoch_state() -> EpochState {
        EpochState {
            epoch: 0.into(),
            nonce: ZkHash::ZERO,
            utxos: UtxoTree::default(),
            total_stake: 100,
        }
    }

    #[test]
    fn test_update_active_provider() {
        let config = setup();
        let service_a = ServiceType::BlendNetwork;
        let utxo = utxo();
        let note_id = utxo.id();
        let signing_key = create_signing_key();
        let zk_key = create_zk_key(0);

        let op = &SDPDeclareOp {
            service_type: service_a,
            locked_note_id: note_id,
            zk_id: zk_key.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: Vec::new(),
        };
        let declaration_id = op.id();

        // Initialize ledger with service config
        let epoch_state = dummy_epoch_state();
        let sdp_ledger = SdpLedger::new()
            .with_blend_service(config.service_rewards_params.blend.clone(), &epoch_state);

        // Apply declare at block 0
        let sdp_ledger = apply_declare_with_dummies(sdp_ledger, op, &zk_key, &config).unwrap();

        // Declaration is in service_state.declarations but not in sessions yet
        let declarations = sdp_ledger.get_declarations(service_a).unwrap();
        assert!(declarations.contains_key(&declaration_id));

        // Apply headers to reach block 10 (session boundary)
        let mut sdp_ledger = sdp_ledger;
        for _ in 0..10 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }

        // At block 10, declaration enters forming session 2
        let forming_session = sdp_ledger.get_forming_session(service_a).unwrap();
        assert_eq!(forming_session.session_n, 2);
        assert!(forming_session.declarations.contains_key(&declaration_id));
        assert_eq!(forming_session.declarations.size(), 1);
    }

    #[test]
    fn test_withdraw_provider() {
        let config = setup();
        let service_a = ServiceType::BlendNetwork;
        let (utxo_sk, utxo) = utxo_with_sk();
        let note_id = utxo.id();
        let signing_key = create_signing_key();
        let zk_key = create_zk_key(1);

        let declare_op = &SDPDeclareOp {
            service_type: service_a,
            locked_note_id: note_id,
            zk_id: zk_key.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: Vec::new(),
        };
        let declaration_id = declare_op.id();

        // Initialize ledger with service config and declare
        let epoch_state = dummy_epoch_state();
        let sdp_ledger = SdpLedger::new()
            .with_blend_service(config.service_rewards_params.blend.clone(), &epoch_state);

        let sdp_ledger =
            apply_declare_with_dummies(sdp_ledger, declare_op, &zk_key, &config).unwrap();

        // Verify declaration is present
        let declarations = sdp_ledger.get_declarations(service_a).unwrap();
        assert!(declarations.contains_key(&declaration_id));

        // Move forward enough blocks to satisfy lock_period
        let mut sdp_ledger = sdp_ledger;
        for _ in 0..11 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }

        // Withdraw the declaration
        let withdraw_op = &SDPWithdrawOp {
            declaration_id,
            nonce: 1,
            locked_note_id: note_id,
        };
        let sdp_ledger =
            apply_withdraw_with_dummies(sdp_ledger, withdraw_op, utxo_sk, zk_key, &config).unwrap();

        // Verify declaration is removed
        let declarations = sdp_ledger.get_declarations(service_a).unwrap();
        assert!(!declarations.contains_key(&declaration_id));
        assert!(declarations.is_empty());
    }

    #[test]
    fn test_promote_session_with_updated_provider() {
        let config = setup();
        let service_a = ServiceType::BlendNetwork;
        let utxo = utxo();
        let note_id = utxo.id();
        let signing_key = create_signing_key();
        let zk_key = create_zk_key(0);

        let op = &SDPDeclareOp {
            service_type: service_a,
            locked_note_id: note_id,
            zk_id: zk_key.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: Vec::new(),
        };
        let declaration_id = op.id();

        // Initialize ledger with service config
        let epoch_state = dummy_epoch_state();
        let sdp_ledger = SdpLedger::new()
            .with_blend_service(config.service_rewards_params.blend.clone(), &epoch_state);

        // Declare at block 0
        let sdp_ledger = apply_declare_with_dummies(sdp_ledger, op, &zk_key, &config).unwrap();

        // Apply headers to reach block 10 (session boundary for session_duration=10)
        let mut sdp_ledger = sdp_ledger;
        for _ in 0..10 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }

        // At block 10: active becomes session 1 (was empty forming), forming becomes
        // session 2 (snapshot at block 10)
        let active_session = sdp_ledger.get_active_session(service_a).unwrap();
        assert_eq!(active_session.session_n, 1);
        assert!(active_session.declarations.is_empty()); // Active session 1 is empty

        // Check forming session is now session 2 and contains declaration
        let forming_session = sdp_ledger.get_forming_session(service_a).unwrap();
        assert_eq!(forming_session.session_n, 2);
        assert!(forming_session.declarations.contains_key(&declaration_id));

        // Continue to block 20 to see declaration become active
        for _ in 0..10 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }

        // At block 20: active becomes session 2 (with declaration)
        let active_session = sdp_ledger.get_active_session(service_a).unwrap();
        assert_eq!(active_session.session_n, 2);
        assert!(active_session.declarations.contains_key(&declaration_id));
    }

    #[test]
    fn test_no_promotion() {
        let config = setup();
        let service_a = ServiceType::BlendNetwork;

        // Initialize ledger with service config
        let epoch_state = dummy_epoch_state();
        let mut sdp_ledger = SdpLedger::new()
            .with_blend_service(config.service_rewards_params.blend.clone(), &epoch_state);

        // Apply headers to reach block 9 (still in session 0, promotion happens at
        // block 10)
        for _ in 0..9 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }

        // Check active session is still session 0 with no declarations
        let active_session = sdp_ledger.get_active_session(service_a).unwrap();
        assert_eq!(active_session.session_n, 0);
        assert!(active_session.declarations.is_empty());

        // Check forming session is still session 1
        let forming_session = sdp_ledger.get_forming_session(service_a).unwrap();
        assert_eq!(forming_session.session_n, 1);
    }

    #[test]
    fn test_promote_one_service() {
        let config = setup();
        let service_a = ServiceType::BlendNetwork; // session_duration = 10
        let service_b = ServiceType::DataAvailability; // session_duration = 5

        // Initialize ledger with both services
        let epoch_state = dummy_epoch_state();
        let mut sdp_ledger = SdpLedger::new()
            .with_blend_service(config.service_rewards_params.blend.clone(), &epoch_state)
            .with_da_service();

        // Apply headers to reach block 6
        // At block 5, DataAvailability promotes (5/5=1, active.session_n=0, so 1==0+1)
        // At block 6, BlendNetwork hasn't promoted yet (6/10=0, active.session_n=0, so
        // 0!=0+1)
        for _ in 0..6 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }

        // Check DataAvailability is promoted to session 1
        let active_session_b = sdp_ledger.get_active_session(service_b).unwrap();
        assert_eq!(active_session_b.session_n, 1);
        let forming_session_b = sdp_ledger.get_forming_session(service_b).unwrap();
        assert_eq!(forming_session_b.session_n, 2);

        // Check BlendNetwork is still in session 0
        let active_session_a = sdp_ledger.get_active_session(service_a).unwrap();
        assert_eq!(active_session_a.session_n, 0);
        let forming_session_a = sdp_ledger.get_forming_session(service_a).unwrap();
        assert_eq!(forming_session_a.session_n, 1);
    }

    #[test]
    fn test_new_declarations_becoming_active_after_session_boundary() {
        let config = setup();
        let service_a = ServiceType::BlendNetwork;
        let signing_key = create_signing_key();
        let zk_key = create_zk_key(0);

        // Initialize ledger
        let epoch_state = dummy_epoch_state();
        let mut sdp_ledger = SdpLedger::new()
            .with_blend_service(config.service_rewards_params.blend.clone(), &epoch_state);

        // SESSION 0: Add a declaration at block 5
        for _ in 0..5 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }

        let declare_op = &SDPDeclareOp {
            service_type: service_a,
            locked_note_id: utxo().id(),
            zk_id: zk_key.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: Vec::new(),
        };
        let declaration_id = declare_op.id();

        sdp_ledger = apply_declare_with_dummies(sdp_ledger, declare_op, &zk_key, &config).unwrap();

        // Move to block 9 (last block of session 0)
        for _ in 6..10 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }
        assert_eq!(sdp_ledger.block_number, 9);

        // Declaration is not in active or forming sessions yet
        let active_session = sdp_ledger.get_active_session(service_a).unwrap();
        assert_eq!(active_session.session_n, 0);
        assert!(!active_session.declarations.contains_key(&declaration_id));

        let forming_session = sdp_ledger.get_forming_session(service_a).unwrap();
        assert_eq!(forming_session.session_n, 1);
        assert!(forming_session.declarations.is_empty());

        // SESSION 1: Cross session boundary to block 10
        (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        assert_eq!(sdp_ledger.block_number, 10);

        // Active session 1 is empty (was the empty forming session 1)
        let active_session = sdp_ledger.get_active_session(service_a).unwrap();
        assert_eq!(active_session.session_n, 1);
        assert!(active_session.declarations.is_empty());

        // Forming session 2 now has the declaration (snapshot from block 10)
        let forming_session = sdp_ledger.get_forming_session(service_a).unwrap();
        assert_eq!(forming_session.session_n, 2);
        assert!(forming_session.declarations.contains_key(&declaration_id));

        // SESSION 2: Cross to block 20
        for _ in 11..20 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }
        (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        assert_eq!(sdp_ledger.block_number, 20);

        // Now the declaration is active in session 2
        let active_session = sdp_ledger.get_active_session(service_a).unwrap();
        assert_eq!(active_session.session_n, 2);
        assert!(active_session.declarations.contains_key(&declaration_id));
    }

    #[test]
    fn test_declaration_snapshot_timing() {
        let config = setup();
        let service_a = ServiceType::BlendNetwork;
        let signing_key = create_signing_key();
        let zk_key_1 = create_zk_key(1);

        let epoch_state = dummy_epoch_state();
        let mut sdp_ledger = SdpLedger::new()
            .with_blend_service(config.service_rewards_params.blend.clone(), &epoch_state);

        // Add declaration at block 0
        let declare_op_1 = &SDPDeclareOp {
            service_type: service_a,
            locked_note_id: utxo().id(),
            zk_id: zk_key_1.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: Vec::new(),
        };
        let declaration_id_1 = declare_op_1.id();

        sdp_ledger =
            apply_declare_with_dummies(sdp_ledger, declare_op_1, &zk_key_1, &config).unwrap();

        // Move to block 9 (last block before session boundary)
        for _ in 1..10 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }

        // Save state at block 9
        let sdp_ledger_block_9 = sdp_ledger.clone();

        // Add another declaration at block 10 (after session boundary)
        (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        assert_eq!(sdp_ledger.block_number, 10);

        let zk_key_2 = create_zk_key(2);
        let declare_op_2 = &SDPDeclareOp {
            service_type: service_a,
            locked_note_id: utxo().id(),
            zk_id: zk_key_2.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: Vec::new(),
        };
        let declaration_id_2 = declare_op_2.id();

        sdp_ledger =
            apply_declare_with_dummies(sdp_ledger, declare_op_2, &zk_key_2, &config).unwrap();

        // Jump to session 2 (block 20)
        for _ in 11..20 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }
        (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();

        // Active session (session 2) should contain both declarations
        let active_session = sdp_ledger.get_active_session(service_a).unwrap();
        assert!(active_session.declarations.contains_key(&declaration_id_1));
        assert!(!active_session.declarations.contains_key(&declaration_id_2));

        // Now test from the block 9 state - jumping directly to block 20
        let mut sdp_ledger_from_9 = sdp_ledger_block_9;
        for _ in 10..20 {
            (sdp_ledger_from_9, _) = sdp_ledger_from_9
                .try_apply_header(&config, &epoch_state)
                .unwrap();
        }
        (sdp_ledger_from_9, _) = sdp_ledger_from_9
            .try_apply_header(&config, &epoch_state)
            .unwrap();

        // Active session should only contain declaration_id_1
        // because declaration_id_2 was never added in this timeline
        let active_session_from_9 = sdp_ledger_from_9.get_active_session(service_a).unwrap();
        assert!(
            active_session_from_9
                .declarations
                .contains_key(&declaration_id_1)
        );
        assert!(
            !active_session_from_9
                .declarations
                .contains_key(&declaration_id_2)
        );
    }

    #[test]
    fn test_session_jump() {
        let config = setup();
        let service_a = ServiceType::BlendNetwork;
        let signing_key = create_signing_key();
        let zk_key = create_zk_key(0);

        let epoch_state = dummy_epoch_state();
        let mut sdp_ledger = SdpLedger::new()
            .with_blend_service(config.service_rewards_params.blend.clone(), &epoch_state);

        // Add declaration at block 3
        for _ in 0..3 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }

        let declare_op = &SDPDeclareOp {
            service_type: service_a,
            locked_note_id: utxo().id(),
            zk_id: zk_key.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: Vec::new(),
        };
        let declaration_id = declare_op.id();

        sdp_ledger = apply_declare_with_dummies(sdp_ledger, declare_op, &zk_key, &config).unwrap();

        // Jump directly from block 3 to block 25 (skipping session 1 entirely)
        for _ in 4..25 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }
        (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        assert_eq!(sdp_ledger.block_number, 25);

        // Declaration snapshots should be taken from the last known state
        // Active session (session 2, which started at block 20) should contain the
        // declaration
        let active_session = sdp_ledger.get_active_session(service_a).unwrap();
        assert_eq!(active_session.session_n, 2);
        assert!(active_session.declarations.contains_key(&declaration_id));

        // Forming session (session 3) should also contain the declaration
        let forming_session = sdp_ledger.get_forming_session(service_a).unwrap();
        assert_eq!(forming_session.session_n, 3);
        assert!(forming_session.declarations.contains_key(&declaration_id));
    }

    #[test]
    #[expect(clippy::cognitive_complexity, reason = "sessions are complex :)")]
    fn test_session_boundary() {
        // Test a declaration at block 9 is available in session 2 but a declaration in
        // block 10 is not
        let config = setup();
        let service_a = ServiceType::BlendNetwork;
        let signing_key = create_signing_key();
        let zk_key_1 = create_zk_key(1);

        let epoch_state = dummy_epoch_state();
        let mut sdp_ledger = SdpLedger::new()
            .with_blend_service(config.service_rewards_params.blend.clone(), &epoch_state);

        // Move to block 9 (last block of session 0)
        for _ in 0..9 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }
        assert_eq!(sdp_ledger.block_number, 9);

        let active_session = sdp_ledger.get_active_session(service_a).unwrap();
        assert_eq!(active_session.session_n, 0);
        assert!(active_session.declarations.is_empty());

        let forming_session = sdp_ledger.get_forming_session(service_a).unwrap();
        assert_eq!(forming_session.session_n, 1);
        assert!(forming_session.declarations.is_empty());

        // Create first declaration at block 9
        let declare_op_1 = &SDPDeclareOp {
            service_type: service_a,
            locked_note_id: utxo().id(),
            zk_id: zk_key_1.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: Vec::new(),
        };
        let declaration_id_1 = declare_op_1.id();

        sdp_ledger =
            apply_declare_with_dummies(sdp_ledger, declare_op_1, &zk_key_1, &config).unwrap();

        // Cross to block 10 (session boundary - start of session 1)
        // At this point, the snapshot for forming session 2 is taken
        (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        assert_eq!(sdp_ledger.block_number, 10);

        let active_session = sdp_ledger.get_active_session(service_a).unwrap();
        assert_eq!(active_session.session_n, 1);
        assert!(active_session.declarations.is_empty());

        // Forming session 2 should contain declaration_1 (made at block 9)
        let forming_session = sdp_ledger.get_forming_session(service_a).unwrap();
        assert_eq!(forming_session.session_n, 2);
        assert!(forming_session.declarations.contains_key(&declaration_id_1));

        // Create second declaration at block 10 (first block of session 1)
        let zk_key_2 = create_zk_key(2);
        let declare_op_2 = &SDPDeclareOp {
            service_type: service_a,
            locked_note_id: utxo().id(),
            zk_id: zk_key_2.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: Vec::new(),
        };
        let declaration_id_2 = declare_op_2.id();

        sdp_ledger =
            apply_declare_with_dummies(sdp_ledger, declare_op_2, &zk_key_2, &config).unwrap();

        // Forming session 2 still only has declaration_1 (snapshot was already taken at
        // block 10)
        let forming_session = sdp_ledger.get_forming_session(service_a).unwrap();
        assert_eq!(forming_session.session_n, 2);
        assert!(forming_session.declarations.contains_key(&declaration_id_1));
        assert!(!forming_session.declarations.contains_key(&declaration_id_2));

        // Jump to block 20 (start of session 2)
        for _ in 11..20 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }
        (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        assert_eq!(sdp_ledger.block_number, 20);

        // Active session 2 has declaration_1 (from block 9)
        let active_session = sdp_ledger.get_active_session(service_a).unwrap();
        assert_eq!(active_session.session_n, 2);
        assert!(active_session.declarations.contains_key(&declaration_id_1));
        assert!(!active_session.declarations.contains_key(&declaration_id_2));

        // Forming session 3 has both declarations (snapshot from block 20)
        let forming_session = sdp_ledger.get_forming_session(service_a).unwrap();
        assert_eq!(forming_session.session_n, 3);
        assert!(forming_session.declarations.contains_key(&declaration_id_1));
        assert!(forming_session.declarations.contains_key(&declaration_id_2));

        // Jump to block 30 (start of session 3)
        for _ in 21..30 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }
        (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        assert_eq!(sdp_ledger.block_number, 30);

        // Active session 3 now has both declarations
        let active_session = sdp_ledger.get_active_session(service_a).unwrap();
        assert_eq!(active_session.session_n, 3);
        assert!(active_session.declarations.contains_key(&declaration_id_1));
        assert!(active_session.declarations.contains_key(&declaration_id_2));
    }

    #[test]
    fn test_garbage_collection_different_services() {
        // Tests: multiple services with different retention periods
        // blocks DA config: retention_period=10, session_duration=5 → effective
        let config = gc_test_config();
        let service_da = ServiceType::DataAvailability;
        let service_bn = ServiceType::BlendNetwork;
        let signing_key = create_signing_key();
        let zk_key = create_zk_key(0);
        let zk_key_2 = create_zk_key(1);

        let epoch_state = dummy_epoch_state();
        let mut sdp_ledger = SdpLedger::new()
            .with_da_service()
            .with_blend_service(config.service_rewards_params.blend.clone(), &epoch_state);

        // Create 1 Blend declaration at block 0
        let declare_op_bn = &SDPDeclareOp {
            service_type: service_bn,
            locked_note_id: utxo().id(),
            zk_id: zk_key.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: Vec::new(),
        };
        let declaration_id_bn = declare_op_bn.id();

        // Create 2 DA declarations at block 0
        let declare_op_da = &SDPDeclareOp {
            service_type: service_da,
            locked_note_id: utxo().id(),
            zk_id: zk_key.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: Vec::new(),
        };
        let declaration_id_da = declare_op_da.id();
        let (sk, utxo) = utxo_with_sk();
        let declare_op_da_2 = &SDPDeclareOp {
            service_type: service_da,
            locked_note_id: utxo.id(),
            zk_id: zk_key_2.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: Vec::new(),
        };
        let declaration_id_da_2 = declare_op_da_2.id();

        sdp_ledger =
            apply_declare_with_dummies(sdp_ledger, declare_op_bn, &zk_key, &config).unwrap();
        sdp_ledger =
            apply_declare_with_dummies(sdp_ledger, declare_op_da, &zk_key, &config).unwrap();
        sdp_ledger =
            apply_declare_with_dummies(sdp_ledger, declare_op_da_2, &zk_key_2, &config).unwrap();

        // Move to block 25 (past DA's 20-block retention, at session boundary)
        for _ in 1..26 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }

        // Both declarations are still present, DA expires at 26 but next session
        // boundary is at 30
        let declarations_bn = sdp_ledger.get_declarations(service_bn).unwrap();
        assert!(declarations_bn.contains_key(&declaration_id_bn));
        let declarations_da = sdp_ledger.get_declarations(service_da).unwrap();
        assert!(declarations_da.contains_key(&declaration_id_da));
        assert!(declarations_da.contains_key(&declaration_id_da_2));

        // Send active message to update the declaration's active field to block 25
        let active_op = SDPActiveOp {
            declaration_id: declaration_id_da_2,
            nonce: 1,
            metadata: nomos_core::sdp::ActivityMetadata::DataAvailability(
                nomos_core::sdp::da::ActivityProof {
                    current_session: 4,
                    previous_session_opinions: vec![0b00_11u8],
                    current_session_opinions: vec![0b00_11u8],
                },
            ),
        };

        let tx_hash = TxHash(Fr::from(2u8));
        let zk_sig = ZkKey::multi_sign(&[sk, zk_key_2], &tx_hash.0).unwrap();

        sdp_ledger = sdp_ledger
            .apply_active_msg(&active_op, &zk_sig, tx_hash, &config)
            .unwrap();

        // Move to block 30
        for _ in 26..31 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }

        assert_eq!(sdp_ledger.block_number, 30);
        let declarations_bn = sdp_ledger.get_declarations(service_bn).unwrap();
        assert!(declarations_bn.contains_key(&declaration_id_bn));
        let declarations_da = sdp_ledger.get_declarations(service_da).unwrap();
        assert!(!declarations_da.contains_key(&declaration_id_da));
        // Second DA declaration should still be present due to active msg
        assert!(declarations_da.contains_key(&declaration_id_da_2));

        // Move to block 55
        for _ in 31..56 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }

        assert_eq!(sdp_ledger.block_number, 55);
        // Now also BN declaration and second DA declaration should be removed
        let declarations_bn = sdp_ledger.get_declarations(service_bn).unwrap();
        assert!(!declarations_bn.contains_key(&declaration_id_bn));
        let declarations_da = sdp_ledger.get_declarations(service_da).unwrap();
        assert!(!declarations_da.contains_key(&declaration_id_da));
        assert!(!declarations_da.contains_key(&declaration_id_da_2));
    }
}
