pub mod rewards;
#[cfg(test)]
pub(crate) mod test_utils;

use std::collections::HashMap;

use lb_blend_message::crypto::proofs::RealProofsVerifier;
use lb_core::{
    block::BlockNumber,
    events::{HeaderEvent, TxEvent},
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
    ) -> (Self, Vec<Utxo>, Vec<HeaderEvent>) {
        match self {
            Self::BlendNetwork(state) => {
                let (new_state, utxos, events) = state.try_apply_header(
                    last_epoch_state,
                    epoch_state,
                    locked_notes,
                    config,
                    &rewards_params.blend,
                );
                (Self::BlendNetwork(new_state), utxos, events)
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

    pub fn update_rewards(
        &mut self,
        provider_id: ProviderId,
        metadata: &ActivityMetadata,
        rewards_params: &ServiceRewardsParameters,
    ) -> Result<(), Error> {
        match self {
            Self::BlendNetwork(state) => {
                state.rewards =
                    state
                        .rewards
                        .update_active(provider_id, metadata, &rewards_params.blend)?;
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
    // Rewards calculation and tracking for this service
    pub rewards: R,
}

impl<R: Rewards> ServiceState<R> {
    fn try_apply_header(
        mut self,
        last_epoch_state: &EpochState,
        epoch_state: &EpochState,
        locked_notes: &mut LockedNotes,
        service_params: &ServiceParameters,
        rewards_params: &R::Params,
    ) -> (Self, Vec<Utxo>, Vec<HeaderEvent>) {
        let mut reward_utxos = Vec::new();
        let mut events = Vec::new();

        if last_epoch_state.epoch() < epoch_state.epoch() {
            // Unlock notes from withdrawn declarations if possible
            events.extend(
                self.unlock_notes_from_withdrawn_declarations(locked_notes, epoch_state.epoch()),
            );

            // Garbage collect declarations
            self.gc_declarations(epoch_state.epoch(), service_params);

            // Update and distribute rewards
            (self.rewards, reward_utxos) = self.rewards.update_epoch(
                last_epoch_state,
                epoch_state,
                service_params,
                rewards_params,
            );
        }

        (self, reward_utxos, events)
    }

    /// Unlock notes from withdrawn declarations whose withdrawn epoch has been
    /// reached.
    ///
    /// Returns one [`HeaderEvent::SdpNoteUnlocked`] event per unlocked note.
    fn unlock_notes_from_withdrawn_declarations(
        &self,
        locked_notes: &mut LockedNotes,
        epoch: Epoch,
    ) -> Vec<HeaderEvent> {
        let mut events = Vec::new();
        self.declarations.iter().for_each(|(declaration_id, declaration)| {
            if let Some(withdraw_at) = declaration.withdraw_at
                && epoch >= withdraw_at
                && locked_notes
                    .is_locked_for_service(&declaration.locked_note_id, &declaration.service_type)
            {
                locked_notes
                    .unlock(declaration.service_type, &declaration.locked_note_id)
                    .expect("unlocking note from withdrawn declaraion must be successful if it hasn't been unlocked yet");
                events.push(
                    HeaderEvent::SdpNoteUnlocked {
                        note_id: declaration.locked_note_id,
                        service_type: declaration.service_type,
                        declaration_id: *declaration_id,
                    }
                );
            }
        });
        events
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
        let withdrawn = declaration.withdraw_at.is_some_and(|withdraw_at| {
            withdraw_at.strict_add(config.retention_period) < current_epoch
        });
        let inactive = declaration
            .active
            .strict_add(config.inactivity_period.into_inner())
            .strict_add(config.retention_period)
            < current_epoch;
        withdrawn || inactive
    }

    fn add_income(&mut self, income: Value) {
        self.rewards = self.rewards.add_income(income);
    }

    fn contains(&self, declaration_id: &DeclarationId) -> bool {
        self.declarations.contains_key(declaration_id)
    }
}

/// Returns true if the declaration is active at `current_epoch`:
/// an activity message has been accepted within `inactivity_period` epochs,
/// and its withdrawal (if any) has not yet taken effect.
fn is_active(declaration: &Declaration, current_epoch: Epoch, config: &ServiceParameters) -> bool {
    declaration
        .active
        .strict_add(config.inactivity_period.into_inner())
        >= current_epoch
        && declaration
            .withdraw_at
            .is_none_or(|withdraw_at| withdraw_at > current_epoch)
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
    ) -> Result<(Self, Vec<TxEvent>), Error> {
        let mut sdp = Self::new(epoch_state.epoch())
            .with_blend_service(&config.service_rewards_params.blend, epoch_state);

        let mut all_events = Vec::new();
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
        rewards_settings: &blend::RewardsParameters,
        epoch_state: &EpochState,
    ) -> Self {
        assert_eq!(
            epoch_state.epoch, self.epoch,
            "TODO: refactor to remove this assertion"
        );
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
            rewards,
        }
    }

    pub fn try_apply_header(
        &self,
        config: &Config,
        last_epoch_state: &EpochState,
        epoch_state: &EpochState,
    ) -> Result<(Self, HeaderEffect), Error> {
        let mut all_reward_utxos = Vec::new();
        let mut all_events = Vec::new();
        let mut locked_notes = self.locked_notes().clone();

        let services = self
            .services
            .iter()
            .map(|(service, service_state)| {
                let service_params = config
                    .service_params
                    .get(service)
                    .ok_or(Error::EpochParamsNotFound(*service))?;
                let (new_state, reward_utxos, events) = service_state.clone().try_apply_header(
                    last_epoch_state,
                    epoch_state,
                    &mut locked_notes,
                    service_params,
                    &config.service_rewards_params,
                );
                all_reward_utxos.extend(reward_utxos);
                all_events.extend(events);
                Ok::<_, Error>((*service, new_state))
            })
            .collect::<Result<_, _>>()?;

        Ok((
            Self {
                epoch: epoch_state.epoch(),
                services,
                locked_notes,
            },
            HeaderEffect {
                reward_utxos: all_reward_utxos,
                events: all_events,
            },
        ))
    }

    pub fn try_apply_genesis_sdp_declaration(
        mut self,
        utxo_tree: &UtxoTree,
        op: &SDPDeclareOp,
        config: &Config,
    ) -> Result<(Self, Vec<TxEvent>), Error> {
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
    ) -> Result<(Self, Vec<TxEvent>), Error> {
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
    ) -> Result<(Self, Vec<TxEvent>), Error> {
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
    ) -> Result<(Self, Vec<TxEvent>), Error> {
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
    /// current block, regardless of whether they are active or not.
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

    /// Returns the declarations that are active at `epoch`, grouped by
    /// service type.
    ///
    /// Service entries with no active declarations are omitted.
    /// Services missing from `service_params` are skipped.
    #[must_use]
    pub fn active_declarations(
        &self,
        epoch: Epoch,
        service_params: &HashMap<ServiceType, ServiceParameters>,
    ) -> lb_core::sdp::Declarations {
        self.services
            .iter()
            .filter_map(|(service_type, service)| {
                let params = service_params.get(service_type)?;
                let entries: HashMap<DeclarationId, Declaration> = service
                    .declarations()
                    .iter()
                    .filter(|(_, declaration)| is_active(declaration, epoch, params))
                    .map(|(declaration_id, declaration)| (*declaration_id, declaration.clone()))
                    .collect();
                if entries.is_empty() {
                    None
                } else {
                    Some((*service_type, entries))
                }
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

pub struct HeaderEffect {
    pub reward_utxos: Vec<Utxo>,
    pub events: Vec<HeaderEvent>,
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU64, sync::Arc};

    use lb_core::{
        mantle::ledger::Utxos,
        sdp::{Locator, SNAPSHOT_FINALIZATION_DELAY},
    };
    use lb_groth16::{AdditiveGroup as _, Fr};
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};
    use lb_utils::math::NonNegativeF64;
    use num_bigint::BigUint;

    use super::*;
    use crate::{
        cryptarchia::tests::utxo_with_sk, mantle::sdp::test_utils::generate_activity_proof,
    };

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

    const NONCE: Fr = Fr::ZERO;
    const LOTTERY_0: Fr = Fr::ZERO;
    const LOTTERY_1: Fr = Fr::ZERO;

    fn dummy_epoch_state(epoch: Epoch) -> EpochState {
        EpochState {
            epoch,
            nonce: NONCE,
            utxos: UtxoTree::default(),
            total_stake: 100,
            lottery_0: LOTTERY_0,
            lottery_1: LOTTERY_1,
            active_declarations: Arc::new(lb_core::sdp::Declarations::default()),
        }
    }

    fn dummy_sdp_ledger(epoch: Epoch, config: &Config) -> SdpLedger {
        SdpLedger::new(epoch).with_blend_service(
            &config.service_rewards_params.blend,
            &dummy_epoch_state(epoch),
        )
    }

    /// Build the epoch state for `epoch`, snapshotting `active_declarations`
    /// from `ledger` the same way production does. Using a constant nonce and
    /// lottery values keeps the `LeaderInputs` used by
    /// [`generate_activity_proof`] aligned with what the rewards module
    /// computes on epoch transition.
    fn next_epoch_state(epoch: Epoch, ledger: &SdpLedger, config: &Config) -> EpochState {
        EpochState {
            epoch,
            nonce: NONCE,
            utxos: UtxoTree::default(),
            total_stake: 100,
            lottery_0: LOTTERY_0,
            lottery_1: LOTTERY_1,
            active_declarations: Arc::new(
                ledger.active_declarations(epoch, &config.service_params),
            ),
        }
    }

    /// `active_declarations` must drop entries that have gone inactive (i.e.,
    /// `active + inactivity_period < snapshot_epoch`) even if they have not
    /// been garbage-collected yet.
    #[test]
    fn active_declarations_filters_out_inactive() {
        // Long retention so GC never runs in the window we test, short
        // inactivity so the declaration goes inactive quickly.
        let config = setup(ServiceParameters {
            inactivity_period: 2.try_into().unwrap(),
            retention_period: 100.into(),
            epoch: 0.into(),
        });

        let epoch0 = dummy_epoch_state(0.into());
        let mut ledger = dummy_sdp_ledger(0.into(), &config);

        // Advance to epoch 1 and declare. The new declaration's `active`
        // initializes to created + 2 = 3.
        let mut last_epoch_state = epoch0;
        let new_epoch_state = next_epoch_state(1.into(), &ledger, &config);
        (ledger, _) = ledger
            .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
            .unwrap();
        last_epoch_state = new_epoch_state;

        let (_utxo_sk, utxo) = utxo_with_sk();
        let signing_key = create_signing_key();
        let zk_key = create_zk_key(1);
        let declare_op = &SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locked_note_id: utxo.id(),
            zk_id: zk_key.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: "/ip4/1.1.1.1/udp/0".parse::<Locator>().unwrap().into(),
        };
        let declaration_id = declare_op.id();
        let ledger = apply_declare_with_dummies(
            &utxo_tree(vec![utxo]),
            ledger,
            declare_op,
            &zk_key,
            &config,
        )
        .unwrap();

        // Advance to epoch 6 without an activity message; GC won't fire
        // (retention=100), but the declaration is inactive past epoch 5
        // (active=3, inactivity=2 -> 3+2 < 6).
        let mut ledger = ledger;
        for epoch in 2..=6 {
            let new_epoch_state = next_epoch_state(epoch.into(), &ledger, &config);
            (ledger, _) = ledger
                .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
                .unwrap();
            last_epoch_state = new_epoch_state;
        }

        // The declaration is still present in the live ledger (no GC)
        assert!(ledger.get_declaration(&declaration_id).is_some());
        // but active_declarations at epoch 6 must filter it out.
        assert!(
            ledger
                .active_declarations(6.into(), &config.service_params)
                .for_service(&ServiceType::BlendNetwork)
                .is_none_or(|m| !m.contains_key(&declaration_id)),
            "inactive declaration must be excluded from the active-declarations snapshot"
        );
    }

    /// Genesis declarations are initialized with `active = created + 2`, so a
    /// declaration created at epoch 0 must still appear in the active set when
    /// it's consumed at epochs 0 and 1.
    #[test]
    fn active_declarations_includes_genesis_at_epochs_0_and_1() {
        let config = setup(ServiceParameters {
            inactivity_period: 2.try_into().unwrap(),
            retention_period: 1.into(),
            epoch: 0.into(),
        });

        // Build an SDP ledger with an declaration at epoch 0.
        let ledger = dummy_sdp_ledger(0.into(), &config);
        let (_utxo_sk, utxo) = utxo_with_sk();
        let signing_key = create_signing_key();
        let zk_key = create_zk_key(1);
        let declare_op = &SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locked_note_id: utxo.id(),
            zk_id: zk_key.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: "/ip4/1.1.1.1/udp/0".parse::<Locator>().unwrap().into(),
        };
        let declaration_id = declare_op.id();
        let ledger = apply_declare_with_dummies(
            &utxo_tree(vec![utxo]),
            ledger,
            declare_op,
            &zk_key,
            &config,
        )
        .unwrap();

        // `active` is initialized to created + 2.
        let declaration = ledger.get_declaration(&declaration_id).unwrap();
        assert_eq!(declaration.active, SNAPSHOT_FINALIZATION_DELAY);

        // At epoch 0 and 1, the declaration must be included in the active set.
        for epoch in [0u32, 1] {
            assert!(
                ledger
                    .active_declarations(epoch.into(), &config.service_params)
                    .for_service(&ServiceType::BlendNetwork)
                    .is_some_and(|m| m.contains_key(&declaration_id)),
                "genesis declaration must be active at epoch {epoch}"
            );
        }
    }

    /// A withdrawn declaration must remain active until its `withdrawn` epoch
    /// is reached, and become inactive from that epoch onward — even while
    /// the declaration is still present in the live SDP ledger.
    #[test]
    fn active_declarations_filters_out_withdrawn_at_effective_epoch() {
        // Long inactivity/retention so the only filter that fires in this test
        // is the withdrawn-effective-epoch check.
        let config = setup(ServiceParameters {
            inactivity_period: 100.try_into().unwrap(),
            retention_period: 100.into(),
            epoch: 0.into(),
        });

        let epoch0 = dummy_epoch_state(0.into());
        let mut ledger = dummy_sdp_ledger(0.into(), &config);

        // Advance to epoch 1 and declare. The declaration's `active`
        // initializes to created + 2 = 3.
        let last_epoch_state = epoch0;
        let new_epoch_state = next_epoch_state(1.into(), &ledger, &config);
        (ledger, _) = ledger
            .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
            .unwrap();

        let (utxo_sk, utxo) = utxo_with_sk();
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
        let ledger = apply_declare_with_dummies(
            &utxo_tree(vec![utxo]),
            ledger,
            declare_op,
            &zk_key,
            &config,
        )
        .unwrap();

        // Withdraw at epoch 1: `withdrawn = 1 + SNAPSHOT_FINALIZATION_DELAY = 3`.
        let withdraw_op = &SDPWithdrawOp {
            declaration_id,
            nonce: 1,
            locked_note_id: note_id,
        };
        let ledger =
            apply_withdraw_with_dummies(ledger, withdraw_op, utxo_sk, zk_key, &config).unwrap();
        let withdraw_at = ledger
            .get_declaration(&declaration_id)
            .unwrap()
            .withdraw_at
            .expect("withdraw must set the withdraw_at");
        assert_eq!(withdraw_at, Epoch::new(3));

        // The declaration is still in the live SDP ledger — cleanup runs only
        // when the ledger advances past `withdrawn_epoch`.
        assert!(ledger.get_declaration(&declaration_id).is_some());

        // Snapshot at any epoch strictly less than `withdrawn_epoch` must
        // include the declaration.
        for epoch in 0..withdraw_at.into_inner() {
            assert!(
                ledger
                    .active_declarations(epoch.into(), &config.service_params)
                    .for_service(&ServiceType::BlendNetwork)
                    .is_some_and(|m| m.contains_key(&declaration_id)),
                "withdrawn-but-not-yet-effective declaration must be active at epoch {epoch}"
            );
        }

        // Snapshot at `withdrawn_epoch` (and beyond) must exclude it.
        for epoch in withdraw_at.into_inner()..=withdraw_at.into_inner() + 2 {
            assert!(
                ledger
                    .active_declarations(epoch.into(), &config.service_params)
                    .for_service(&ServiceType::BlendNetwork)
                    .is_none_or(|m| !m.contains_key(&declaration_id)),
                "withdrawn declaration must be excluded from the snapshot at epoch {epoch}"
            );
        }
    }

    /// A provider that hasn't submit a new active message during
    /// `inactivity_period + retention_period` epochs must be removed.
    #[test]
    fn gc_inactive_declaration() {
        let config = setup(ServiceParameters {
            // Set inactivity/retention periods very short to check that
            // declaration is NOT removed before an activity message is submitted.
            inactivity_period: 2.try_into().unwrap(),
            retention_period: 1.into(),
            epoch: 0.into(),
        });

        // Init ledger with no declaration
        let epoch0 = dummy_epoch_state(0.into());
        let mut ledger = dummy_sdp_ledger(0.into(), &config);

        // Move forward to the epoch 1
        let epoch1 = next_epoch_state(1.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch0, &epoch1).unwrap();

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
        let epoch2 = next_epoch_state(2.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch1, &epoch2).unwrap();
        let epoch3 = next_epoch_state(3.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch2, &epoch3).unwrap();
        let epoch4 = next_epoch_state(4.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch3, &epoch4).unwrap();
        // Check that the declaration is still present.
        let declarations = ledger.get_declarations(ServiceType::BlendNetwork).unwrap();
        assert!(declarations.contains_key(&declaration_id));

        // Submit an activity message at epoch 4
        let active_op = SDPActiveOp {
            declaration_id,
            nonce: 1,
            metadata: ActivityMetadata::Blend(Box::new(generate_activity_proof(
                &zk_key,
                &epoch3, // proving activity from epoch 3
                &epoch4,
                &config.service_rewards_params.blend,
            ))),
        };
        let mut ledger = apply_active_with_dummies(ledger, &active_op, zk_key, &config).unwrap();
        let declaration = ledger.get_declarations(ServiceType::BlendNetwork).unwrap();
        assert_eq!(
            declaration.get(&declaration_id).unwrap().active,
            Epoch::new(4) // epoch when the activity message is submitted/accepted
        );

        // Move forward to the epoch 7. The declaration should be still present
        // because the activity message was accepted at epoch 4.
        let epoch5 = next_epoch_state(5.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch4, &epoch5).unwrap();
        let epoch6 = next_epoch_state(6.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch5, &epoch6).unwrap();
        let epoch7 = next_epoch_state(7.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch6, &epoch7).unwrap();
        let declarations = ledger.get_declarations(ServiceType::BlendNetwork).unwrap();
        assert!(declarations.contains_key(&declaration_id));

        // Before moving to epoch 8 where declaration will be removed,
        // applying another header within the same epoch 7 must be a no-op
        // (GC and unlock are gated to epoch transitions only).
        let ledger_before = ledger.clone();
        (ledger, _) = ledger.try_apply_header(&config, &epoch7, &epoch7).unwrap();
        assert_eq!(
            ledger, ledger_before,
            "within-epoch try_apply_header must not change ledger state"
        );

        // Move forward to epoch 8 where declaration should be removed
        // because no activity message has been submitted since epoch 4
        let epoch8 = next_epoch_state(8.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch7, &epoch8).unwrap();
        let declarations = ledger.get_declarations(ServiceType::BlendNetwork).unwrap();
        assert!(!declarations.contains_key(&declaration_id));
    }

    #[test]
    fn rewards_distributed_to_active_provider() {
        let config = setup(ServiceParameters {
            inactivity_period: 2.try_into().unwrap(),
            retention_period: 100.into(),
            epoch: 0.into(),
        });

        // Init ledger with no declaration.
        let epoch0 = dummy_epoch_state(0.into());
        let mut ledger = dummy_sdp_ledger(0.into(), &config);

        // Move forward to epoch 1 and declare.
        let epoch1 = next_epoch_state(1.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch0, &epoch1).unwrap();

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

        // Advance to epoch 3.
        let epoch2 = next_epoch_state(2.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch1, &epoch2).unwrap();
        let epoch3 = next_epoch_state(3.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch2, &epoch3).unwrap();

        // Simulate block-reward income accrued during epoch 3.
        let income: Value = 1000;
        ledger.add_blend_income(income);

        // Advance to epoch 4: The declaration becomes active.
        let epoch4 = next_epoch_state(4.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch3, &epoch4).unwrap();

        // Submit an activity proof at epoch 4
        let active_op = SDPActiveOp {
            declaration_id,
            nonce: 1,
            metadata: ActivityMetadata::Blend(Box::new(generate_activity_proof(
                &zk_key,
                &epoch3,
                &epoch4,
                &config.service_rewards_params.blend,
            ))),
        };
        let ledger =
            apply_active_with_dummies(ledger, &active_op, zk_key.clone(), &config).unwrap();

        // Advance to epoch 5: SDP rewards are distributed
        let epoch5 = next_epoch_state(5.into(), &ledger, &config);
        let (_, effect) = ledger.try_apply_header(&config, &epoch4, &epoch5).unwrap();

        // The single provider is both the only submitter and the premium
        // provider (min hamming distance), so they collect the full `income`.
        let provider_zk_id = zk_key.to_public_key();
        let received: Vec<&Utxo> = effect
            .reward_utxos
            .iter()
            .filter(|u| u.note.pk == provider_zk_id)
            .collect();
        assert_eq!(
            received.len(),
            1,
            "the active provider must receive exactly one reward UTXO",
        );
        assert_eq!(
            received[0].note.value, income,
            "single-provider reward must equal the full accrued income",
        );
    }

    /// Regression test: the per-epoch membership build must not panic on an
    /// SDP snapshot that contains two declarations with the same `zk_id`.
    ///
    /// `DeclarationId = Hash(service || provider_id || zk_id || locators)`
    /// does not bind `zk_id` uniqueness (the SDP spec explicitly permits
    /// duplicates), so two declarations with the *same* `zk_id` but
    /// *different* locators (hence different `DeclarationId`s) can both be on
    /// chain. `membership_info_from_epoch_state` (and, once rewards are
    /// re-enabled, `providers_and_zk_root` inside `try_apply_header`) feed
    /// their `zk_id`s into `sort_nodes_and_build_merkle_tree(..).expect(..)`,
    /// which used to return `Err(DuplicateKey)` on the collision and so
    /// panicked every Blend node at the epoch boundary. The builder now
    /// reduces duplicate keys to a single leaf, so the build succeeds.
    #[test]
    fn membership_merkle_build_tolerates_duplicate_zk_ids() {
        use lb_blend_crypto::merkle::sort_nodes_and_build_merkle_tree;

        let signing_key = create_signing_key();
        let zk_key = create_zk_key(1);
        let (_sk_a, utxo_a) = utxo_with_sk();
        let (_sk_b, utxo_b) = utxo_with_sk();

        // Two declarations sharing the SAME zk_id, differing only in locators
        // (and locked note) -> distinct DeclarationIds, identical zk_id.
        let declare_a = SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locked_note_id: utxo_a.id(),
            zk_id: zk_key.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: "/ip4/1.1.1.1/udp/0".parse::<Locator>().unwrap().into(),
        };
        let declare_b = SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locked_note_id: utxo_b.id(),
            zk_id: zk_key.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: "/ip4/2.2.2.2/udp/0".parse::<Locator>().unwrap().into(),
        };
        assert_ne!(declare_a.id(), declare_b.id());
        assert_eq!(declare_a.zk_id, declare_b.zk_id);

        // Exactly what `membership_info_from_epoch_state` does with the
        // snapshot: build the core-membership Merkle tree keyed by each
        // declaration's zk_id. Production `.expect()`s this result.
        let mut zk_ids = vec![declare_a.zk_id.into_inner(), declare_b.zk_id.into_inner()];
        let result = sort_nodes_and_build_merkle_tree(&mut zk_ids, |zk_id| *zk_id);

        assert!(
            result.is_ok(),
            "membership Merkle build must not error (and thus `.expect()`-panic) \
             on duplicate zk_ids: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_withdraw_provider() {
        let config = setup(ServiceParameters {
            // inactivity/retention periods should be long enough
            // for this test to avoid the declaration being removed due to
            // inacitivity before we can test the withdraw logic.
            inactivity_period: 20.try_into().unwrap(),
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
        let epoch0 = dummy_epoch_state(0.into());
        let sdp_ledger = dummy_sdp_ledger(0.into(), &config);

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
            .withdraw_at
            .expect("withdraw_at must be set after withdraw tx is accepted");

        // Move forward epochs until withdrawn_epoch is reached,
        // and check that the note has been unlocked. The unlock event
        // must fire exactly once — on the epoch reaching `withdrawn_epoch`,
        // and never earlier.
        let mut sdp_ledger = sdp_ledger;
        let mut last_epoch_state = epoch0;
        for epoch in 1..=withdrawn_epoch.into_inner() {
            let new_epoch_state = next_epoch_state(epoch.into(), &sdp_ledger, &config);
            let events;
            (sdp_ledger, HeaderEffect { events, .. }) = sdp_ledger
                .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
                .unwrap();
            let unlock_events = events.into_iter().filter_map(|event| {
                let HeaderEvent::SdpNoteUnlocked {
                    note_id: unlocked_note,
                    service_type,
                    declaration_id: id,
                } = &event;
                (*unlocked_note == note_id && *service_type == service_a && *id == declaration_id)
                    .then_some(event)
            });
            if epoch == withdrawn_epoch.into_inner() {
                assert_eq!(unlock_events.count(), 1);
            } else {
                assert_eq!(unlock_events.count(), 0);
            }
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
        let target_epoch = withdrawn_epoch
            .strict_add(retention_period)
            .strict_add(Epoch::new(1));
        for epoch in (withdrawn_epoch.into_inner() + 1)..target_epoch.into_inner() {
            let new_epoch_state = next_epoch_state(epoch.into(), &sdp_ledger, &config);
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
        let new_epoch_state = next_epoch_state(target_epoch, &sdp_ledger, &config);
        (sdp_ledger, _) = sdp_ledger
            .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
            .unwrap();
        assert!(
            sdp_ledger.get_declaration(&declaration_id).is_none(),
            "declaration should have been removed"
        );
    }
}
