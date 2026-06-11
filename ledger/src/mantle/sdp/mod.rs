pub mod rewards;

use std::{collections::HashMap, marker::PhantomData};

use lb_blend_message::crypto::proofs::RealProofsVerifier;
use lb_core::{
    block::BlockNumber,
    events::Events,
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
        ActivityMetadata, Declaration, DeclarationId, MinStake, Nonce, ProviderId,
        ServiceParameters, ServiceType,
        locked_notes::{self, LockedNotes},
    },
};
use lb_cryptarchia_engine::Epoch;
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
        last_epoch_state: &EpochState,
        epoch_state: &EpochState,
        locked_notes: &mut LockedNotes,
        config: &ServiceParameters,
        rewards_params: &ServiceRewardsParameters,
    ) -> (Self, Vec<Utxo>) {
        match self {
            Self::BlendNetwork(state) => {
                let (new_state, utxos) = state.try_apply_header(
                    last_epoch_state,
                    epoch_state,
                    locked_notes,
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

    #[expect(
        clippy::unnecessary_wraps,
        reason = "TODO: enable this after making the `rewards` module stable"
    )]
    pub const fn update_rewards(
        &mut self,
        _provider_id: ProviderId,
        _metadata: &ActivityMetadata,
        _rewards_params: &ServiceRewardsParameters,
    ) -> Result<(), Error> {
        match self {
            Self::BlendNetwork(_state) => {
                // TODO: enable this after making the `rewards` module stable
                // state.rewards =
                //     state
                //         .rewards
                //         .update_active(provider_id, metadata, &rewards_params.blend)?;
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
    #[error("Epoch parameters for {0:?} not found")]
    EpochParamsNotFound(ServiceType),
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ServiceState<R: Rewards> {
    /// Declarations accumulated until the current block.
    declarations: Declarations,
    // TODO: enable this after making the `rewards` module stable
    // Rewards calculation and tracking for this service
    // pub rewards: R,
    _phantom: PhantomData<R>,
}

impl<R: Rewards> ServiceState<R> {
    fn try_apply_header(
        mut self,
        last_epoch_state: &EpochState,
        epoch_state: &EpochState,
        locked_notes: &mut LockedNotes,
        service_params: &ServiceParameters,
        _rewards_params: &R::Params,
    ) -> (Self, Vec<Utxo>) {
        let reward_utxos = Vec::new();

        if last_epoch_state.epoch() < epoch_state.epoch() {
            // Unlock notes from withdrawn declarations if possible
            self.unlock_notes_from_withdrawn_declarations(locked_notes, epoch_state.epoch());

            // Garbage collect declarations
            self.gc_declarations(epoch_state.epoch(), service_params);

            // Update rewards with current epoch state and distribute rewards
            // TODO: enable this after making the `rewards` module stable
            // if last_epoch_state.epoch() < epoch_state.epoch() {
            //     (self.rewards, reward_utxos) = self.rewards.update_epoch(
            //         last_epoch_state,
            //         epoch_state,
            //         service_params,
            //         rewards_params,
            //     );
            // }
        }

        (self, reward_utxos)
    }

    /// Unlock notes from withdrawn declarations whose withdrawn epoch has been
    /// reached.
    fn unlock_notes_from_withdrawn_declarations(
        &self,
        locked_notes: &mut LockedNotes,
        epoch: Epoch,
    ) {
        self.declarations.iter().for_each(|(_, declaration)| {
            if let Some(withdrawn) = declaration.withdrawn
                && epoch >= withdrawn
                && locked_notes
                    .is_locked_for_service(&declaration.locked_note_id, &declaration.service_type)
            {
                locked_notes
                    .unlock(declaration.service_type, &declaration.locked_note_id)
                    .expect("unlocking note from withdrawn declaraion must be successful if it hasn't been unlocked yet");
            }
        });
    }

    /// Garbage collect declarations that have been withdrawn or inactive,
    /// if the retention period has passed.
    fn gc_declarations(&mut self, epoch: Epoch, service_params: &ServiceParameters) {
        let expired: Vec<DeclarationId> = self
            .declarations
            .iter()
            .filter(|(_id, declaration)| Self::is_expired(declaration, epoch, service_params))
            .map(|(id, declaration)| {
                warn!(
                    ?declaration,
                    ?epoch,
                    ?service_params,
                    "removing an expired declaration"
                );
                *id
            })
            .collect();
        for id in &expired {
            self.declarations.remove_mut(id);
        }
    }

    /// Returns true if the declaration has been withdrawn or inactive,
    /// and if the retention period has passed.
    fn is_expired(
        declaration: &Declaration,
        current_epoch: Epoch,
        config: &ServiceParameters,
    ) -> bool {
        let withdrawn = declaration
            .withdrawn
            .is_some_and(|withdrawn| withdrawn + config.retention_period < current_epoch);
        let inactive =
            declaration.active + config.inactivity_period + config.retention_period < current_epoch;
        withdrawn || inactive
    }

    #[expect(
        clippy::unused_self,
        reason = "TODO: enable this after making the `rewards` module stable"
    )]
    const fn add_income(&self, _income: Value) {
        // TODO: enable this after making the `rewards` module stable
        // self.rewards = self.rewards.add_income(income);
    }

    fn contains(&self, declaration_id: &DeclarationId) -> bool {
        self.declarations.contains_key(declaration_id)
    }
}

/// A SDP state of the mantle ledger
///
/// NOTE: Most collection fields in this struct should use `rpds`
/// since we keep a copy of this state for each block.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SdpLedger {
    services: rpds::HashTrieMapSync<ServiceType, Service>,
    locked_notes: LockedNotes,
    // The epoch when this ledger was created
    epoch: Epoch,
}

impl SdpLedger {
    #[must_use]
    pub fn new(epoch: Epoch) -> Self {
        Self {
            services: rpds::HashTrieMapSync::new_sync(),
            locked_notes: LockedNotes::new(),
            epoch,
        }
    }

    pub fn from_genesis<'a>(
        config: &Config,
        utxo_tree: &UtxoTree,
        epoch_state: &EpochState,
        ops: impl Iterator<Item = (&'a SDPDeclareOp, &'a OpProof)> + 'a,
    ) -> Result<(Self, Events), Error> {
        let mut sdp = Self::new(epoch_state.epoch())
            .with_blend_service(&config.service_rewards_params.blend, epoch_state);

        let mut all_events = Events::new();
        for (op, _) in ops {
            let (result, events) = sdp.try_apply_genesis_sdp_declaration(utxo_tree, op, config)?;
            sdp = result;
            all_events.extend(events);
        }

        Ok((sdp, all_events))
    }

    #[must_use]
    pub fn with_blend_service(
        mut self,
        _rewards_settings: &blend::RewardsParameters,
        epoch_state: &EpochState,
    ) -> Self {
        assert_eq!(
            epoch_state.epoch, self.epoch,
            "TODO: refactor to remove this assertion"
        );
        let service = Service::BlendNetwork(Self::new_service_state(
            // TODO: enable this after making the `rewards` module stable
            // blend::Rewards::new(
            //     rewards_settings,
            //     epoch_state,
            // )
        ));
        self.services = self.services.insert(ServiceType::BlendNetwork, service);
        self
    }

    #[must_use]
    fn new_service_state<R: Rewards>(// TODO: enabled this after making the `rewards` module stable
        //rewards: R
    ) -> ServiceState<R> {
        ServiceState {
            declarations: rpds::RedBlackTreeMapSync::new_sync(),
            _phantom: PhantomData,
        }
    }

    pub fn try_apply_header(
        &self,
        config: &Config,
        last_epoch_state: &EpochState,
        epoch_state: &EpochState,
    ) -> Result<(Self, Vec<Utxo>), Error> {
        let mut all_reward_utxos = Vec::new();
        let mut locked_notes = self.locked_notes().clone();

        let services = self
            .services
            .iter()
            .map(|(service, service_state)| {
                let service_params = config
                    .service_params
                    .get(service)
                    .ok_or(Error::EpochParamsNotFound(*service))?;
                let (new_state, reward_utxos) = service_state.clone().try_apply_header(
                    last_epoch_state,
                    epoch_state,
                    &mut locked_notes,
                    service_params,
                    &config.service_rewards_params,
                );
                all_reward_utxos.extend(reward_utxos);
                Ok::<_, Error>((*service, new_state))
            })
            .collect::<Result<_, _>>()?;

        Ok((
            Self {
                epoch: epoch_state.epoch(),
                services,
                locked_notes,
            },
            all_reward_utxos,
        ))
    }

    pub fn try_apply_genesis_sdp_declaration(
        mut self,
        utxo_tree: &UtxoTree,
        op: &SDPDeclareOp,
        config: &Config,
    ) -> Result<(Self, Events), Error> {
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
        let (result, events) =
            <SDPDeclareOp as Operation<SDPDeclareGenesisValidationContext>>::execute(
                op,
                SDPDeclareExecutionContext {
                    utxo_tree: utxo_tree.clone(),
                    epoch: self.epoch,
                    declarations: service_state.declarations_clone(),
                    locked_notes: self.locked_notes.clone(),
                    min_stake: config.min_stake,
                },
            )?;

        self.locked_notes = result.locked_notes;
        service_state.update_declarations(result.declarations);
        Ok((self, events))
    }

    pub fn try_apply_sdp_declaration(
        mut self,
        utxo_tree: &UtxoTree,
        op: &SDPDeclareOp,
        zk_sig: &ZkSignature,
        ed25519_sig: &Ed25519Signature,
        tx_hash: TxHash,
        config: &Config,
    ) -> Result<(Self, Events), Error> {
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
        let (result, events) = <SDPDeclareOp as Operation<SDPDeclareValidationContext>>::execute(
            op,
            SDPDeclareExecutionContext {
                utxo_tree: utxo_tree.clone(),
                epoch: self.epoch,
                declarations: service_state.declarations_clone(),
                locked_notes: self.locked_notes.clone(),
                min_stake: config.min_stake,
            },
        )?;

        self.locked_notes = result.locked_notes;
        service_state.update_declarations(result.declarations);
        Ok((self, events))
    }

    pub fn apply_active_msg(
        mut self,
        op: &SDPActiveOp,
        zksig: &ZkSignature,
        tx_hash: TxHash,
        config: &Config,
    ) -> Result<(Self, Events), Error> {
        let (service, _) = self.get_service(&op.declaration_id, config)?;
        let Some(service_state) = self.services.get_mut(&service) else {
            return Err(Error::ServiceNotFound(service));
        };

        //Validate SDP Active
        op.validate(&SDPActiveValidationContext {
            declarations: service_state.declarations(),
            tx_hash: &tx_hash,
            active_sig: zksig,
            epoch: self.epoch,
        })?;

        // Execute SDP Active
        let (result, events) = op.execute(SDPActiveExecutionContext {
            epoch: self.epoch,
            declarations: service_state.declarations_clone(),
        })?;

        let provider_id = result
            .declarations
            .get(&op.declaration_id)
            .expect("the declaration should be in the list after execution")
            .provider_id;

        service_state.update_declarations(result.declarations);
        service_state.update_rewards(provider_id, &op.metadata, &config.service_rewards_params)?;

        Ok((self, events))
    }

    pub fn apply_withdrawn_msg(
        mut self,
        op: &SDPWithdrawOp,
        zksig: &ZkSignature,
        tx_hash: TxHash,
        config: &Config,
    ) -> Result<(Self, Events), Error> {
        let (service, _) = self.get_service(&op.declaration_id, config)?;
        let Some(service_state) = self.services.get_mut(&service) else {
            return Err(Error::ServiceNotFound(service));
        };

        // Validate SDP Withdraw
        op.validate(&SDPWithdrawValidationContext {
            declarations: service_state.declarations(),
            epoch: self.epoch,
            locked_notes: &self.locked_notes,
            tx_hash: &tx_hash,
            sdp_withdraw_sig: zksig,
        })?;

        // Execute SDP Withdraw
        let (result, events) = op.execute(SDPWithdrawExecutionContext {
            declarations: service_state.declarations_clone(),
            locked_notes: self.locked_notes.clone(),
            epoch: self.epoch,
        })?;

        self.locked_notes = result.locked_notes;
        service_state.update_declarations(result.declarations);

        Ok((self, events))
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

    /// Declarations of all services, which have been accumulated until the
    /// current block.
    #[must_use]
    pub fn declarations(&self) -> lb_core::sdp::Declarations {
        self.services
            .iter()
            .map(|(service_type, service_state)| {
                (
                    *service_type,
                    service_state
                        .declarations()
                        .iter()
                        .map(|(declaration_id, declaration)| (*declaration_id, declaration.clone()))
                        .collect(),
                )
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
    fn get_declarations(&self, service_type: ServiceType) -> Option<&Declarations> {
        self.services.get(&service_type).map(Service::declarations)
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU64, sync::Arc};

    use lb_blend_proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection};
    use lb_core::{crypto::ZkHash, mantle::ledger::Utxos, sdp::Locator};
    use lb_groth16::{AdditiveGroup as _, Fr};
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};
    use lb_utils::math::NonNegativeF64;
    use num_bigint::BigUint;

    use super::*;
    use crate::cryptarchia::tests::utxo_with_sk;

    fn setup(service_params: ServiceParameters) -> Config {
        let mut params = HashMap::new();
        params.insert(ServiceType::BlendNetwork, service_params);
        Config {
            service_params: Arc::new(params),
            service_rewards_params: ServiceRewardsParameters {
                blend: blend::RewardsParameters {
                    rounds_per_epoch: NonZeroU64::new(10).unwrap(),
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

        sdp_ledger
            .try_apply_sdp_declaration(utxos, op, &zk_sig, &ed25519_sig, tx_hash, config)
            .map(|(sdp_ledger, _)| sdp_ledger)
    }

    fn apply_active_with_dummies(
        sdp_ledger: SdpLedger,
        op: &SDPActiveOp,
        zk_sk: ZkKey,
        config: &Config,
    ) -> Result<SdpLedger, Error> {
        let tx_hash = TxHash([2u8; 32]);
        let zk_sig = ZkKey::multi_sign(&[zk_sk], &tx_hash.to_fr()).unwrap();
        sdp_ledger
            .apply_active_msg(op, &zk_sig, tx_hash, config)
            .map(|(sdp_ledger, _)| sdp_ledger)
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

        sdp_ledger
            .apply_withdrawn_msg(op, &zk_sig, tx_hash, config)
            .map(|(sdp_ledger, _)| sdp_ledger)
    }

    fn dummy_epoch_state(epoch: Epoch, rewards_settings: &blend::RewardsParameters) -> EpochState {
        let mut epoch_state = EpochState {
            epoch,
            nonce: ZkHash::ZERO,
            utxos: UtxoTree::default(),
            total_stake: 100,
            lottery_0: Fr::ZERO,
            lottery_1: Fr::ZERO,
            sdp: SdpLedger::new(epoch),
        };

        epoch_state.sdp = epoch_state
            .sdp
            .clone()
            .with_blend_service(rewards_settings, &epoch_state);

        epoch_state
    }

    fn next_epoch_state(epoch: Epoch, last_epoch_state: EpochState) -> EpochState {
        EpochState {
            epoch,
            nonce: ZkHash::from(epoch.into_inner()),
            ..last_epoch_state
        }
    }

    /// A provider that hasn't submit a new active message during
    /// `inactivity_period + retention_period` epochs must be removed.
    #[test]
    fn gc_inactive_declaration() {
        let config = setup(ServiceParameters {
            // Set inactivity/retention periods very short to check that
            // declaration is NOT removed before an activity message is submitted.
            inactivity_period: 1.into(),
            retention_period: 1.into(),
            epoch: 0.into(),
        });

        // Init ledger with no declaration
        let epoch0 = dummy_epoch_state(0.into(), &config.service_rewards_params.blend);
        let mut ledger = epoch0.sdp.clone();

        // Move forward to the epoch 1
        let mut last_epoch_state = epoch0.clone();
        let new_epoch_state = next_epoch_state(1.into(), epoch0);
        (ledger, _) = ledger
            .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
            .unwrap();
        last_epoch_state = new_epoch_state;

        // Add a declaration at epoch 1
        let (_utxo_sk, utxo) = utxo_with_sk();
        let note_id = utxo.id();
        let signing_key = create_signing_key();
        let zk_key = create_zk_key(1);
        let declare_op = &SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locked_note_id: note_id,
            zk_id: zk_key.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: "/ip4/1.1.1.1/udp/0".parse::<Locator>().unwrap().into(),
        };
        let declaration_id = declare_op.id();
        let mut ledger = apply_declare_with_dummies(
            &utxo_tree(vec![utxo]),
            ledger,
            declare_op,
            &zk_key,
            &config,
        )
        .unwrap();
        let declarations = ledger.get_declarations(ServiceType::BlendNetwork).unwrap();
        assert!(declarations.contains_key(&declaration_id));

        // Move forward to the epoch 4 where the provider can submit an activity
        // message.
        // (The provider is expected to provide the service from epoch 3)
        for epoch in 2..=4 {
            let new_epoch_state = next_epoch_state(epoch.into(), last_epoch_state.clone());
            (ledger, _) = ledger
                .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
                .unwrap();
            last_epoch_state = new_epoch_state;
        }
        // Check that the declaration is still present.
        let declarations = ledger.get_declarations(ServiceType::BlendNetwork).unwrap();
        assert!(declarations.contains_key(&declaration_id));

        // Submit an activity message at epoch 4
        let active_op = SDPActiveOp {
            declaration_id,
            nonce: 1,
            metadata: ActivityMetadata::Blend(Box::new(lb_core::sdp::blend::ActivityProof {
                epoch: 3.into(), // proving activity from epoch 3
                signing_key: signing_key.public_key(),
                proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0; _]).into(),
                proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([1; _]).into(),
            })),
        };
        let mut ledger = apply_active_with_dummies(ledger, &active_op, zk_key, &config).unwrap();
        let declaration = ledger.get_declarations(ServiceType::BlendNetwork).unwrap();
        assert_eq!(
            declaration.get(&declaration_id).unwrap().active,
            Epoch::new(4) // epoch when the activity message is submitted/accepted
        );

        // Move forward to the epoch 6. The declaration should be still present
        // because the activity message was accepted at epoch 4.
        for epoch in 5..=6 {
            let new_epoch_state = next_epoch_state(epoch.into(), last_epoch_state.clone());
            (ledger, _) = ledger
                .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
                .unwrap();
            last_epoch_state = new_epoch_state;
        }
        let declarations = ledger.get_declarations(ServiceType::BlendNetwork).unwrap();
        assert!(declarations.contains_key(&declaration_id));

        // Before moving to epoch 7 where declaration will be removed,
        // applying another header within the same epoch 6 must be a no-op
        // (GC and unlock are gated to epoch transitions only).
        let ledger_before = ledger.clone();
        (ledger, _) = ledger
            .try_apply_header(&config, &last_epoch_state, &last_epoch_state)
            .unwrap();
        assert_eq!(
            ledger, ledger_before,
            "within-epoch try_apply_header must not change ledger state"
        );

        // Move forward to epoch 7 where declaration should be removed
        // because no activity message has been submitted since epoch 4
        let new_epoch_state = next_epoch_state(7.into(), last_epoch_state.clone());
        (ledger, _) = ledger
            .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
            .unwrap();
        let declarations = ledger.get_declarations(ServiceType::BlendNetwork).unwrap();
        assert!(!declarations.contains_key(&declaration_id));
    }

    #[test]
    fn test_withdraw_provider() {
        let config = setup(ServiceParameters {
            // inactivity/retention periods should be long enough
            // for this test to avoid the declaration being removed due to
            // inacitivity before we can test the withdraw logic.
            inactivity_period: 20.into(),
            retention_period: 20.into(),
            epoch: 0.into(),
        });

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
            locators: "/ip4/1.1.1.1/udp/0".parse::<Locator>().unwrap().into(),
        };
        let declaration_id = declare_op.id();

        // Initialize ledger with service config and declare
        let epoch0 = dummy_epoch_state(0.into(), &config.service_rewards_params.blend);
        let sdp_ledger = epoch0.sdp.clone();

        let utxo_tree = utxo_tree(vec![utxo]);
        let sdp_ledger =
            apply_declare_with_dummies(&utxo_tree, sdp_ledger, declare_op, &zk_key, &config)
                .unwrap();

        // Verify declaration is present
        assert!(sdp_ledger.get_declaration(&declaration_id).is_some());

        // Withdraw the declaration
        let withdraw_op = &SDPWithdrawOp {
            declaration_id,
            nonce: 1,
            locked_note_id: note_id,
        };
        let sdp_ledger =
            apply_withdraw_with_dummies(sdp_ledger, withdraw_op, utxo_sk, zk_key, &config).unwrap();

        let withdrawn_epoch = sdp_ledger.get_declaration(&declaration_id)
            .expect("declaration must still exist even after withdrawal because GC shouldn't remove it immediately")
            .withdrawn
            .expect("withdraw epoch must be set after withdraw tx is accepted");

        // Move forward epochs until withdrawn_epoch is reached,
        // and check that the note has been unlocked.
        let mut sdp_ledger = sdp_ledger;
        let mut last_epoch_state = epoch0;
        for epoch in 1..=withdrawn_epoch.into_inner() {
            let new_epoch_state = next_epoch_state(epoch.into(), last_epoch_state.clone());
            (sdp_ledger, _) = sdp_ledger
                .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
                .unwrap();
            last_epoch_state = new_epoch_state;
        }
        assert!(
            sdp_ledger.get_declaration(&declaration_id).is_some(),
            "declaration must still exist because GC shouldn't remove it until snapshot_finalization + retention_period has passed"
        );
        assert!(
            !sdp_ledger
                .locked_notes()
                .is_locked_for_service(&declare_op.locked_note_id, &ServiceType::BlendNetwork),
            "the provider's note must be unlocked once withdrawn_epoch is reached"
        );

        // Move forward epochs just before the `snapshot_finalization +
        // retention_period` has elapsed, and check that the declaration hasn't
        // been removed yet (boundary check).
        let retention_period = config
            .service_params
            .get(&ServiceType::BlendNetwork)
            .unwrap()
            .retention_period;
        let target_epoch = withdrawn_epoch + retention_period + Epoch::new(1);
        for epoch in (withdrawn_epoch.into_inner() + 1)..target_epoch.into_inner() {
            let new_epoch_state = next_epoch_state(epoch.into(), last_epoch_state.clone());
            (sdp_ledger, _) = sdp_ledger
                .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
                .unwrap();
            last_epoch_state = new_epoch_state;
        }
        assert!(
            sdp_ledger.get_declaration(&declaration_id).is_some(),
            "declaration must still exist because GC shouldn't remove it until snapshot_finalization + retention_period has passed"
        );

        // Move forward one more epoch. Now, `snapshot_finalization + retention_period`
        // has passed. Check that the declaration has been removed.
        let new_epoch_state = next_epoch_state(target_epoch, last_epoch_state.clone());
        (sdp_ledger, _) = sdp_ledger
            .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
            .unwrap();
        assert!(
            sdp_ledger.get_declaration(&declaration_id).is_none(),
            "declaration should have been removed"
        );
    }
}
