pub mod rewards;

use std::collections::HashMap;

use lb_blend_message::crypto::proofs::RealProofsVerifier;
use lb_core::{
    block::BlockNumber,
    mantle::{
        NoteId, OpProof, TxHash, Utxo, Value,
        ledger::Operation,
        ops::sdp::{
            SDPActiveExecutionContext, SDPActiveOp, SDPActiveValidationContext,
            SDPDeclareExecutionContext, SDPDeclareOp, SDPDeclareValidationContext,
            SDPWithdrawExecutionContext, SDPWithdrawOp, SDPWithdrawValidationContext,
            declare::SDPDeclareGenesisValidationContext,
        },
    },
    sdp::{
        ActivityMetadata, Declaration, DeclarationId, MinStake, Nonce, ProviderId, ProviderInfo,
        ServiceParameters, ServiceType, SessionNumber,
        locked_notes::{self, LockedNotes},
    },
};
use lb_key_management_system_keys::keys::{Ed25519Signature, ZkSignature};
use rewards::{Error as RewardsError, Rewards};
use tracing::warn;

use crate::{EpochState, UtxoTree, mantle::sdp::rewards::blend};

type Declarations = rpds::RedBlackTreeMapSync<DeclarationId, Declaration>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
enum Service {
    BlendNetwork(ServiceState<blend::Rewards<RealProofsVerifier>>),
}

impl Service {
    fn try_apply_header(
        self,
        block_number: BlockNumber,
        epoch_state: &EpochState,
        config: &ServiceParameters,
        rewards_params: &ServiceRewardsParameters,
    ) -> (Self, Vec<Utxo>) {
        match self {
            Self::BlendNetwork(state) => {
                let (new_state, utxos) = state.try_apply_header(
                    block_number,
                    epoch_state,
                    config,
                    &rewards_params.blend,
                );
                (Self::BlendNetwork(new_state), utxos)
            }
        }
    }

    fn contains(&self, declaration_id: &DeclarationId) -> bool {
        match self {
            Self::BlendNetwork(state) => state.contains(declaration_id),
        }
    }

    const fn active_session(&self) -> &SessionState {
        match self {
            Self::BlendNetwork(state) => &state.active,
        }
    }

    #[cfg(test)]
    const fn next_session(&self) -> &SessionState {
        match self {
            Self::BlendNetwork(state) => &state.next,
        }
    }

    const fn declarations(&self) -> &Declarations {
        match self {
            Self::BlendNetwork(state) => &state.declarations,
        }
    }

    pub fn declarations_clone(&self) -> Declarations {
        match self {
            Self::BlendNetwork(state) => state.declarations.clone(),
        }
    }

    pub fn update_declarations(&mut self, declarations: Declarations) {
        match self {
            Self::BlendNetwork(state) => state.declarations = declarations,
        }
    }

    pub fn update_rewards(
        &mut self,
        provider_id: ProviderId,
        metadata: &ActivityMetadata,
        block_number: BlockNumber,
        rewards_params: &ServiceRewardsParameters,
    ) -> Result<(), Error> {
        match self {
            Self::BlendNetwork(state) => {
                state.rewards = state.rewards.update_active(
                    provider_id,
                    metadata,
                    block_number,
                    &rewards_params.blend,
                )?;
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub service_params: std::sync::Arc<HashMap<ServiceType, ServiceParameters>>,
    pub service_rewards_params: ServiceRewardsParameters,
    pub min_stake: MinStake,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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
    #[error(
        "Invalid sdp message nonce: message_nonce={message_nonce:?}, declaration_nonce={declaration_nonce:?}"
    )]
    InvalidNonce {
        message_nonce: Nonce,
        declaration_nonce: Nonce,
    },
    #[error("Service not found: {0:?}")]
    ServiceNotFound(ServiceType),
    #[error("Duplicate sdp declaration id: {0:?}")]
    DuplicateDeclaration(DeclarationId),
    #[error("Active session for service {0:?} not found")]
    ActiveSessionNotFound(ServiceType),
    #[error("Next session for service {0:?} not found")]
    NextSessionNotFound(ServiceType),
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
    #[error(transparent)]
    SdpOp(#[from] lb_core::mantle::ops::sdp::SdpError),
}

// State at the beginning of this session
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionState {
    pub declarations: Declarations,
    pub session_n: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ServiceState<R: Rewards> {
    // state of declarations at block b
    declarations: Declarations,
    // (current) active session.
    // snapshot of `declarations` at the start of block ((b // config.session_duration) - 1) *
    // config.session_duration
    active: SessionState,
    // next session.
    // snapshot of `declarations` at the start of block (b // config.session_duration) *
    // config.session_duration
    next: SessionState,
    // rewards calculation and tracking for this service
    pub rewards: R,
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
        service_params: &ServiceParameters,
        rewards_params: &R::Params,
    ) -> (Self, Vec<Utxo>) {
        let current_session = service_params.session_for_block(block_number);
        let reward_utxos;

        // shift all session!
        if current_session == self.active.session_n + 1 {
            // Remove expired declarations based on retention_period
            // This essentially duplicates the declaration set so it's only triggered at
            // session boundaries
            self.declarations = self
                .declarations
                .iter()
                .filter(|(_id, declaration)| {
                    let active = is_active(declaration, block_number, service_params);
                    if !active {
                        warn!(
                            provider_id = ?declaration.provider_id,
                            latest_active_block = declaration.active,
                            current_block = block_number,
                            "removing declaration due to inactivity+retention"
                        );
                    }
                    active
                })
                .map(|(id, declaration)| (*id, declaration.clone()))
                .collect();

            // Update rewards with current session state and distribute rewards
            (self.rewards, reward_utxos) = self.rewards.update_session(
                &self.active,
                epoch_state,
                service_params,
                rewards_params,
            );
            self.active = self.next.clone();
            self.next = SessionState {
                declarations: self.declarations.clone(),
                session_n: self.next.session_n + 1,
            };
        } else {
            assert!(
                current_session == self.active.session_n,
                "Logos blockchain isn't ready for time travel yet: session_of_block={current_session}, active_session={}",
                self.active.session_n
            );
            self.rewards = self.rewards.update_epoch(epoch_state, rewards_params);
            reward_utxos = Vec::new();
        }

        (self, reward_utxos)
    }

    fn add_income(&mut self, income: Value) {
        self.rewards = self.rewards.add_income(income);
    }

    fn contains(&self, declaration_id: &DeclarationId) -> bool {
        self.declarations.contains_key(declaration_id)
    }
}

/// A SDP state of the mantle ledger
///
/// NOTE: Most collection fields in this struct should use `rpds`
/// since we keep a copy of this state for each block.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
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
        ops: impl Iterator<Item = (&'a SDPDeclareOp, &'a OpProof)> + 'a,
    ) -> Result<Self, Error> {
        let mut sdp =
            Self::new().with_blend_service(&config.service_rewards_params.blend, epoch_state);

        for (op, _) in ops {
            sdp = sdp.try_apply_genesis_sdp_declaration(utxo_tree, op, config)?;
        }

        let blend = sdp
            .services
            .get_mut(&ServiceType::BlendNetwork)
            .expect("SDP initialized with Blend in this method");

        let Service::BlendNetwork(state) = blend;
        state.active.declarations = state.declarations.clone();
        state.next.declarations = state.declarations.clone();

        Ok(sdp)
    }

    #[must_use]
    pub fn with_blend_service(
        mut self,
        rewards_settings: &blend::RewardsParameters,
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

            next: SessionState {
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
                let service_params = config
                    .service_params
                    .get(service)
                    .ok_or(Error::SessionParamsNotFound(*service))?;
                let (new_state, reward_utxos) = service_state.clone().try_apply_header(
                    block_number,
                    epoch_state,
                    service_params,
                    &config.service_rewards_params,
                );
                all_reward_utxos.extend(reward_utxos);
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

    pub fn try_apply_genesis_sdp_declaration(
        mut self,
        utxo_tree: &UtxoTree,
        op: &SDPDeclareOp,
        config: &Config,
    ) -> Result<Self, Error> {
        let Some(service_state) = self.services.get_mut(&op.service_type) else {
            return Err(Error::ServiceNotFound(op.service_type));
        };

        // Validate SDP Declare
        op.validate(&SDPDeclareGenesisValidationContext {
            utxo_tree,
            locked_notes: &self.locked_notes,
            declarations: service_state.declarations(),
            min_stake: &config.min_stake,
        })?;

        // Execute SDP Declare
        let result = <SDPDeclareOp as Operation<SDPDeclareGenesisValidationContext>>::execute(
            op,
            SDPDeclareExecutionContext {
                utxo_tree: utxo_tree.clone(),
                block_number: self.block_number,
                declarations: service_state.declarations_clone(),
                locked_notes: self.locked_notes.clone(),
                min_stake: config.min_stake,
            },
        )?;

        self.locked_notes = result.locked_notes;
        service_state.update_declarations(result.declarations);
        Ok(self)
    }

    pub fn try_apply_sdp_declaration(
        mut self,
        utxo_tree: &UtxoTree,
        op: &SDPDeclareOp,
        zk_sig: &ZkSignature,
        ed25519_sig: &Ed25519Signature,
        tx_hash: TxHash,
        config: &Config,
    ) -> Result<Self, Error> {
        let Some(service_state) = self.services.get_mut(&op.service_type) else {
            return Err(Error::ServiceNotFound(op.service_type));
        };

        // Validate SDP Declare
        op.validate(&SDPDeclareValidationContext {
            utxo_tree,
            locked_notes: &self.locked_notes,
            tx_hash: &tx_hash,
            declare_zk_sig: zk_sig,
            declare_eddsa_sig: ed25519_sig,
            declarations: service_state.declarations(),
            min_stake: &config.min_stake,
        })?;

        // Execute SDP Declare
        let result = <SDPDeclareOp as Operation<SDPDeclareValidationContext>>::execute(
            op,
            SDPDeclareExecutionContext {
                utxo_tree: utxo_tree.clone(),
                block_number: self.block_number,
                declarations: service_state.declarations_clone(),
                locked_notes: self.locked_notes.clone(),
                min_stake: config.min_stake,
            },
        )?;

        self.locked_notes = result.locked_notes;
        service_state.update_declarations(result.declarations);
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
        let Some(service_state) = self.services.get_mut(&service) else {
            return Err(Error::ServiceNotFound(service));
        };

        //Validate SDP Active
        op.validate(&SDPActiveValidationContext {
            declarations: service_state.declarations(),
            tx_hash: &tx_hash,
            active_sig: zksig,
        })?;

        // Execute SDP Active
        let result = op.execute(SDPActiveExecutionContext {
            block_number: self.block_number,
            declarations: service_state.declarations_clone(),
        })?;

        let provider_id = result
            .declarations
            .get(&op.declaration_id)
            .expect("the declaration should be in the list after execution")
            .provider_id;

        service_state.update_declarations(result.declarations);
        service_state.update_rewards(
            provider_id,
            &op.metadata,
            self.block_number,
            &config.service_rewards_params,
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
        let Some(service_state) = self.services.get_mut(&service) else {
            return Err(Error::ServiceNotFound(service));
        };

        // Validate SDP Withdraw
        op.validate(&SDPWithdrawValidationContext {
            lock_period: &config.lock_period,
            declarations: service_state.declarations(),
            block_number: &self.block_number,
            locked_notes: &self.locked_notes,
            tx_hash: &tx_hash,
            sdp_withdraw_sig: zksig,
        })?;

        // Execute SDP Withdraw
        let result = op.execute(SDPWithdrawExecutionContext {
            block_number: self.block_number,
            declarations: service_state.declarations_clone(),
            locked_notes: self.locked_notes.clone(),
        })?;

        self.locked_notes = result.locked_notes;
        service_state.update_declarations(result.declarations);

        Ok(self)
    }

    pub fn add_blend_income(&mut self, income: Value) {
        if let Some(Service::BlendNetwork(state)) =
            self.services.get_mut(&ServiceType::BlendNetwork)
        {
            state.add_income(income);
        }
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
    fn get_next_session(&self, service_type: ServiceType) -> Option<&SessionState> {
        self.services.get(&service_type).map(Service::next_session)
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

    use lb_core::{crypto::ZkHash, mantle::ledger::Utxos};
    use lb_groth16::{Field as _, Fr};
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};
    use lb_utils::math::NonNegativeF64;
    use num_bigint::BigUint;

    use super::*;
    use crate::cryptarchia::tests::{utxo, utxo_with_sk};

    fn setup() -> Config {
        let mut params = HashMap::new();
        params.insert(
            ServiceType::BlendNetwork,
            ServiceParameters {
                inactivity_period: 1,
                lock_period: 10,
                retention_period: 1,
                timestamp: 0,
                session_duration: 10,
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
                    data_replication_factor: 0,
                    activity_threshold_sensitivity: 1,
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

    fn utxo_tree(utxos: Vec<Utxo>) -> Utxos {
        let mut utxo_tree = Utxos::new();
        for utxo in utxos {
            (utxo_tree, _) = utxo_tree.insert(utxo.id(), utxo);
        }
        utxo_tree
    }

    fn apply_declare_with_dummies(
        utxos: &Utxos,
        sdp_ledger: SdpLedger,
        op: &SDPDeclareOp,
        zk_sk: &ZkKey,
        config: &Config,
    ) -> Result<SdpLedger, Error> {
        let (note_sk, _) = utxo_with_sk();
        let tx_hash = TxHash([0u8; 32]);
        let zk_sig = ZkKey::multi_sign(&[note_sk, zk_sk.clone()], &tx_hash.to_fr()).unwrap();

        let signing_key = create_signing_key();
        let ed25519_sig = signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref());

        sdp_ledger.try_apply_sdp_declaration(utxos, op, &zk_sig, &ed25519_sig, tx_hash, config)
    }

    fn apply_withdraw_with_dummies(
        sdp_ledger: SdpLedger,
        op: &SDPWithdrawOp,
        note_sk: ZkKey,
        zk_key: ZkKey,
        config: &Config,
    ) -> Result<SdpLedger, Error> {
        let tx_hash = TxHash([1u8; 32]);
        let zk_sig = ZkKey::multi_sign(&[note_sk, zk_key], &tx_hash.to_fr()).unwrap();

        sdp_ledger.apply_withdrawn_msg(op, &zk_sig, tx_hash, config)
    }

    fn dummy_epoch_state() -> EpochState {
        EpochState {
            epoch: 0.into(),
            nonce: ZkHash::ZERO,
            utxos: UtxoTree::default(),
            total_stake: 100,
            lottery_0: Fr::ZERO,
            lottery_1: Fr::ZERO,
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
        let sdp_ledger =
            SdpLedger::new().with_blend_service(&config.service_rewards_params.blend, &epoch_state);

        // Apply declare at block 0
        let utxo_tree = utxo_tree(vec![utxo]);
        let sdp_ledger =
            apply_declare_with_dummies(&utxo_tree, sdp_ledger, op, &zk_key, &config).unwrap();

        // Declaration is in service_state.declarations but not in sessions yet
        let declarations = sdp_ledger.get_declarations(service_a).unwrap();
        assert!(declarations.contains_key(&declaration_id));

        // Apply headers to reach block 10 (session boundary)
        let mut sdp_ledger = sdp_ledger;
        for _ in 0..10 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }

        // At block 10, declaration enters the next session 2
        let next_session = sdp_ledger.get_next_session(service_a).unwrap();
        assert_eq!(next_session.session_n, 2);
        assert!(next_session.declarations.contains_key(&declaration_id));
        assert_eq!(next_session.declarations.size(), 1);
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
        let sdp_ledger =
            SdpLedger::new().with_blend_service(&config.service_rewards_params.blend, &epoch_state);

        let utxo_tree = utxo_tree(vec![utxo]);
        let sdp_ledger =
            apply_declare_with_dummies(&utxo_tree, sdp_ledger, declare_op, &zk_key, &config)
                .unwrap();

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
        let sdp_ledger =
            SdpLedger::new().with_blend_service(&config.service_rewards_params.blend, &epoch_state);

        // Declare at block 0
        let utxo_tree = utxo_tree(vec![utxo]);
        let sdp_ledger =
            apply_declare_with_dummies(&utxo_tree, sdp_ledger, op, &zk_key, &config).unwrap();

        // Apply headers to reach block 10 (session boundary for session_duration=10)
        let mut sdp_ledger = sdp_ledger;
        for _ in 0..10 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }

        // At block 10: `active` becomes session 1 (was empty `next`),
        // `next` becomes session 2 (snapshot at block 10)
        let active_session = sdp_ledger.get_active_session(service_a).unwrap();
        assert_eq!(active_session.session_n, 1);
        assert!(active_session.declarations.is_empty()); // Active session 1 is empty

        // Check next session is now session 2 and contains declaration
        let next_session = sdp_ledger.get_next_session(service_a).unwrap();
        assert_eq!(next_session.session_n, 2);
        assert!(next_session.declarations.contains_key(&declaration_id));

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
        let mut sdp_ledger =
            SdpLedger::new().with_blend_service(&config.service_rewards_params.blend, &epoch_state);

        // Apply headers to reach block 9 (still in session 0, promotion happens at
        // block 10)
        for _ in 0..9 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }

        // Check active session is still session 0 with no declarations
        let active_session = sdp_ledger.get_active_session(service_a).unwrap();
        assert_eq!(active_session.session_n, 0);
        assert!(active_session.declarations.is_empty());

        // Check next session is still session 1
        let next_session = sdp_ledger.get_next_session(service_a).unwrap();
        assert_eq!(next_session.session_n, 1);
    }

    #[test]
    fn test_promote_one_service() {
        let config = setup();
        let service = ServiceType::BlendNetwork; // session_duration = 10

        // Initialize ledger with Blend service
        let epoch_state = dummy_epoch_state();
        let mut sdp_ledger =
            SdpLedger::new().with_blend_service(&config.service_rewards_params.blend, &epoch_state);

        // Apply headers to reach block 10 (session boundary for BlendNetwork)
        for _ in 0..10 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }

        // Check BlendNetwork is promoted to session 1
        let active_session = sdp_ledger.get_active_session(service).unwrap();
        assert_eq!(active_session.session_n, 1);
        let next_session = sdp_ledger.get_next_session(service).unwrap();
        assert_eq!(next_session.session_n, 2);
    }

    #[test]
    fn test_new_declarations_becoming_active_after_session_boundary() {
        let config = setup();
        let service_a = ServiceType::BlendNetwork;
        let signing_key = create_signing_key();
        let zk_key = create_zk_key(0);

        // Initialize ledger
        let epoch_state = dummy_epoch_state();
        let mut sdp_ledger =
            SdpLedger::new().with_blend_service(&config.service_rewards_params.blend, &epoch_state);

        // SESSION 0: Add a declaration at block 5
        for _ in 0..5 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }

        let utxo = utxo();
        let declare_op = &SDPDeclareOp {
            service_type: service_a,
            locked_note_id: utxo.id(),
            zk_id: zk_key.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: Vec::new(),
        };
        let declaration_id = declare_op.id();

        let utxo_tree = utxo_tree(vec![utxo]);
        sdp_ledger =
            apply_declare_with_dummies(&utxo_tree, sdp_ledger, declare_op, &zk_key, &config)
                .unwrap();

        // Move to block 9 (last block of session 0)
        for _ in 6..10 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }
        assert_eq!(sdp_ledger.block_number, 9);

        // Declaration is not in active or next sessions yet
        let active_session = sdp_ledger.get_active_session(service_a).unwrap();
        assert_eq!(active_session.session_n, 0);
        assert!(!active_session.declarations.contains_key(&declaration_id));

        let next_session = sdp_ledger.get_next_session(service_a).unwrap();
        assert_eq!(next_session.session_n, 1);
        assert!(next_session.declarations.is_empty());

        // SESSION 1: Cross session boundary to block 10
        (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        assert_eq!(sdp_ledger.block_number, 10);

        // Active session 1 is empty (was the empty next session 1)
        let active_session = sdp_ledger.get_active_session(service_a).unwrap();
        assert_eq!(active_session.session_n, 1);
        assert!(active_session.declarations.is_empty());

        // Next session 2 now has the declaration (snapshot from block 10)
        let next_session = sdp_ledger.get_next_session(service_a).unwrap();
        assert_eq!(next_session.session_n, 2);
        assert!(next_session.declarations.contains_key(&declaration_id));

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
        let mut sdp_ledger =
            SdpLedger::new().with_blend_service(&config.service_rewards_params.blend, &epoch_state);

        // Add declaration at block 0
        let utxo_1 = utxo();
        let declare_op_1 = &SDPDeclareOp {
            service_type: service_a,
            locked_note_id: utxo_1.id(),
            zk_id: zk_key_1.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: Vec::new(),
        };
        let declaration_id_1 = declare_op_1.id();

        let utxo_tree_1 = utxo_tree(vec![utxo_1]);
        sdp_ledger =
            apply_declare_with_dummies(&utxo_tree_1, sdp_ledger, declare_op_1, &zk_key_1, &config)
                .unwrap();

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
        let utxo_2 = utxo();
        let declare_op_2 = &SDPDeclareOp {
            service_type: service_a,
            locked_note_id: utxo_2.id(),
            zk_id: zk_key_2.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: Vec::new(),
        };
        let declaration_id_2 = declare_op_2.id();

        let utxo_tree_2 = utxo_tree(vec![utxo_1, utxo_2]);
        sdp_ledger =
            apply_declare_with_dummies(&utxo_tree_2, sdp_ledger, declare_op_2, &zk_key_2, &config)
                .unwrap();

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
        let mut sdp_ledger =
            SdpLedger::new().with_blend_service(&config.service_rewards_params.blend, &epoch_state);

        // Add declaration at block 3
        for _ in 0..3 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }

        let utxo = utxo();
        let declare_op = &SDPDeclareOp {
            service_type: service_a,
            locked_note_id: utxo.id(),
            zk_id: zk_key.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: Vec::new(),
        };
        let declaration_id = declare_op.id();

        let utxo_tree = utxo_tree(vec![utxo]);
        sdp_ledger =
            apply_declare_with_dummies(&utxo_tree, sdp_ledger, declare_op, &zk_key, &config)
                .unwrap();

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

        // Next session (session 3) should also contain the declaration
        let next_session = sdp_ledger.get_next_session(service_a).unwrap();
        assert_eq!(next_session.session_n, 3);
        assert!(next_session.declarations.contains_key(&declaration_id));
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
        let mut sdp_ledger =
            SdpLedger::new().with_blend_service(&config.service_rewards_params.blend, &epoch_state);

        // Move to block 9 (last block of session 0)
        for _ in 0..9 {
            (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        }
        assert_eq!(sdp_ledger.block_number, 9);

        let active_session = sdp_ledger.get_active_session(service_a).unwrap();
        assert_eq!(active_session.session_n, 0);
        assert!(active_session.declarations.is_empty());

        let next_session = sdp_ledger.get_next_session(service_a).unwrap();
        assert_eq!(next_session.session_n, 1);
        assert!(next_session.declarations.is_empty());

        // Create first declaration at block 9
        let utxo_1 = utxo();
        let declare_op_1 = &SDPDeclareOp {
            service_type: service_a,
            locked_note_id: utxo_1.id(),
            zk_id: zk_key_1.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: Vec::new(),
        };
        let declaration_id_1 = declare_op_1.id();

        let utxo_tree_1 = utxo_tree(vec![utxo_1]);
        sdp_ledger =
            apply_declare_with_dummies(&utxo_tree_1, sdp_ledger, declare_op_1, &zk_key_1, &config)
                .unwrap();

        // Cross to block 10 (session boundary - start of session 1)
        // At this point, the snapshot for next session 2 is taken
        (sdp_ledger, _) = sdp_ledger.try_apply_header(&config, &epoch_state).unwrap();
        assert_eq!(sdp_ledger.block_number, 10);

        let active_session = sdp_ledger.get_active_session(service_a).unwrap();
        assert_eq!(active_session.session_n, 1);
        assert!(active_session.declarations.is_empty());

        // Next session 2 should contain declaration_1 (made at block 9)
        let next_session = sdp_ledger.get_next_session(service_a).unwrap();
        assert_eq!(next_session.session_n, 2);
        assert!(next_session.declarations.contains_key(&declaration_id_1));

        // Create second declaration at block 10 (first block of session 1)
        let zk_key_2 = create_zk_key(2);
        let utxo_2 = utxo();
        let declare_op_2 = &SDPDeclareOp {
            service_type: service_a,
            locked_note_id: utxo_2.id(),
            zk_id: zk_key_2.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: Vec::new(),
        };
        let declaration_id_2 = declare_op_2.id();

        let utxo_tree_2 = utxo_tree(vec![utxo_1, utxo_2]);
        sdp_ledger =
            apply_declare_with_dummies(&utxo_tree_2, sdp_ledger, declare_op_2, &zk_key_2, &config)
                .unwrap();

        // Next session 2 still only has declaration_1 (snapshot was already taken at
        // block 10)
        let next_session = sdp_ledger.get_next_session(service_a).unwrap();
        assert_eq!(next_session.session_n, 2);
        assert!(next_session.declarations.contains_key(&declaration_id_1));
        assert!(!next_session.declarations.contains_key(&declaration_id_2));

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

        // Next session 3 has both declarations (snapshot from block 20)
        let next_session = sdp_ledger.get_next_session(service_a).unwrap();
        assert_eq!(next_session.session_n, 3);
        assert!(next_session.declarations.contains_key(&declaration_id_1));
        assert!(next_session.declarations.contains_key(&declaration_id_2));

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
}
