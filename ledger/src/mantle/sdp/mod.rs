pub mod rewards;
#[cfg(test)]
pub(crate) mod test_utils;

use std::collections::HashMap;

use lb_blend_message::crypto::proofs::RealProofsVerifier;
use lb_core::{
    block::BlockNumber,
    events::{HeaderEvent, TxEvent},
    mantle::{
        Value,
        ledger::{
            DeclarationKeys,
            verification_mode::{GenesisMode, StandardMode},
        },
        note_set::NoteSets,
        notes::{Note, NoteCommitment, OutputCommitments},
        ops::{
            SignedOperation,
            sdp::{
                SDPActiveExecutionContext, SDPActiveOp, SDPDeclareExecutionContext, SDPDeclareOp,
                SDPWithdrawExecutionContext, SDPWithdrawOp,
                declare::SDPDeclareGenesisValidationContext,
            },
        },
        transactions::{
            hash::TxHashView,
            states::{Preverified, Verified},
        },
    },
    sdp::{
        ActivityMetadata, Declaration, DeclarationId, MinStake, Nonce, ProviderId,
        ServiceParameters, ServiceType,
    },
};
use lb_cryptarchia_engine::Epoch;
use lb_log_targets::{diagnostic::BLEND_REACHABILITY, ledger};
use rewards::{Error as RewardsError, Rewards};
use tracing::debug;

use crate::{EpochState, mantle::sdp::rewards::blend};

const LOG_TARGET: &str = ledger::mantle::SDP;

type Declarations = rpds::RedBlackTreeMapSync<DeclarationId, Declaration>;
type Released = Vec<(DeclarationId, OutputCommitments)>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
enum Service {
    BlendNetwork(ServiceState<blend::Rewards<RealProofsVerifier>>),
}

impl Service {
    fn try_apply_header(
        self,
        last_epoch_state: &EpochState,
        epoch_state: &EpochState,
        config: ServiceParameters,
        rewards_params: &ServiceRewardsParameters,
    ) -> (Self, Vec<Note>, Released, Vec<HeaderEvent>) {
        match self {
            Self::BlendNetwork(state) => {
                let (new_state, reward_notes, released, events) = state.try_apply_header(
                    last_epoch_state,
                    epoch_state,
                    config,
                    &rewards_params.blend,
                );
                (
                    Self::BlendNetwork(new_state),
                    reward_notes,
                    released,
                    events,
                )
            }
        }
    }

    fn pending_income(&self) -> Value {
        match self {
            Self::BlendNetwork(state) => state.rewards.pending_income(),
        }
    }

    fn contains(&self, declaration_id: &DeclarationId) -> bool {
        match self {
            Self::BlendNetwork(state) => state.contains(declaration_id),
        }
    }

    const fn declarations(&self) -> &Declarations {
        match self {
            Self::BlendNetwork(state) => &state.declarations.declarations,
        }
    }

    const fn declaration_keys(&self) -> &DeclarationKeys {
        match self {
            Self::BlendNetwork(state) => &state.declarations.keys,
        }
    }

    pub fn declarations_clone(&self) -> Declarations {
        match self {
            Self::BlendNetwork(state) => state.declarations.declarations.clone(),
        }
    }

    // After an Operation that adds and removes no declaration and changes no
    // `provider_id` or `zk_id`, so the keys stay those of the declarations.
    pub fn update_declarations(&mut self, declarations: Declarations) {
        match self {
            Self::BlendNetwork(state) => state.declarations.declarations = declarations,
        }
    }

    // After the declaration `declaration_id` was added to the declarations.
    pub fn add_declaration(&mut self, declarations: Declarations, declaration_id: DeclarationId) {
        match self {
            Self::BlendNetwork(state) => state.declarations.add(declarations, declaration_id),
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

    fn with_genesis_rewards(
        self,
        rewards_params: &ServiceRewardsParameters,
        epoch_state: &EpochState,
    ) -> Self {
        match self {
            Self::BlendNetwork(mut state) => {
                state.rewards = blend::Rewards::new(&rewards_params.blend, epoch_state);
                Self::BlendNetwork(state)
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
    #[error("Sdp declaration id not found: {0:?}")]
    DeclarationNotFound(DeclarationId),
    #[error("Note is still in use by the service")]
    WithdrawalWhileUsedInService,
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
    #[error("Invalid signature")]
    InvalidSignature,
    #[error("Invalid proof")]
    InvalidProof,
    #[error("Error while computing rewards: {0:?}")]
    RewardsError(#[from] RewardsError),
    #[error(transparent)]
    SdpOp(#[from] lb_core::mantle::ops::sdp::SdpError),
    #[error("Invalid deferral")]
    InvalidDeferral,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ServiceState<R: Rewards> {
    service_type: ServiceType,
    /// Declarations accumulated until the current block.
    declarations: ServiceDeclarations,
    // Rewards calculation and tracking for this service
    pub rewards: R,
}

/// The declarations of a service with their keys, which a declaration checks
/// its `provider_id` and `zk_id` against. Only the declarations are encoded and
/// compared, the keys are rebuilt from them when decoding.
#[derive(Debug, Clone, Default)]
struct ServiceDeclarations {
    declarations: Declarations,
    keys: DeclarationKeys,
}

impl ServiceDeclarations {
    fn add(&mut self, declarations: Declarations, declaration_id: DeclarationId) {
        let declaration = declarations
            .get(&declaration_id)
            .expect("the added declaration is in the declarations");
        self.keys.insert_mut(declaration_id, declaration);
        self.declarations = declarations;
    }

    fn remove_mut(&mut self, declaration_id: &DeclarationId) {
        if let Some(declaration) = self.declarations.get(declaration_id) {
            self.keys.remove_mut(declaration);
            self.declarations.remove_mut(declaration_id);
        }
    }
}

impl PartialEq for ServiceDeclarations {
    fn eq(&self, other: &Self) -> bool {
        self.declarations == other.declarations
    }
}

impl Eq for ServiceDeclarations {}

impl serde::Serialize for ServiceDeclarations {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.declarations.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for ServiceDeclarations {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let declarations = Declarations::deserialize(deserializer)?;
        Ok(Self {
            keys: DeclarationKeys::from_declarations(&declarations),
            declarations,
        })
    }
}

impl<R: Rewards> ServiceState<R> {
    fn try_apply_header(
        mut self,
        last_epoch_state: &EpochState,
        epoch_state: &EpochState,
        service_params: ServiceParameters,
        rewards_params: &R::Params,
    ) -> (Self, Vec<Note>, Released, Vec<HeaderEvent>) {
        let mut reward_notes = Vec::new();
        let mut released = Vec::new();
        let mut events = Vec::new();

        if last_epoch_state.epoch() < epoch_state.epoch() {
            // Update and distribute rewards
            (self.rewards, reward_notes) = self.rewards.update_epoch(
                last_epoch_state,
                epoch_state,
                &service_params,
                rewards_params,
            );
            events.extend(reward_notes.iter().map(|note| {
                debug!(
                    target: LOG_TARGET,
                    service_type = ?self.service_type,
                    ?note,
                    old_epoch = %last_epoch_state.epoch,
                    new_epoch = %epoch_state.epoch,
                    "SDP reward distributed",
                );
                HeaderEvent::SdpRewardDistributed {
                    service_type: self.service_type,
                    note: *note,
                }
            }));

            // Remove withdrawn declarations and release their withdrawal outputs.
            let release_events;
            (released, release_events) =
                self.release_and_remove_withdrawn_declarations(epoch_state.epoch());
            events.extend(release_events);
        }

        (self, reward_notes, released, events)
    }

    /// For every withdrawn declaration whose `withdraw_at + 1` epoch has been
    /// reached, release the outputs of its withdrawal and remove it.
    fn release_and_remove_withdrawn_declarations(
        &mut self,
        epoch: Epoch,
    ) -> (Released, Vec<HeaderEvent>) {
        let released: Released = self
            .declarations
            .declarations
            .iter()
            .filter(|(_, declaration)| {
                declaration
                    .withdraw_at
                    .is_some_and(|withdraw_at| withdraw_at < epoch)
            })
            .map(|(id, declaration)| (*id, declaration.withdraw_outputs.clone()))
            .collect();
        let events = released
            .iter()
            .map(
                |(declaration_id, outputs)| HeaderEvent::SdpWithdrawalReleased {
                    service_type: self.service_type,
                    declaration_id: *declaration_id,
                    outputs: outputs.clone(),
                },
            )
            .collect();
        for (id, _) in &released {
            self.declarations.remove_mut(id);
        }
        (released, events)
    }

    fn add_income(&mut self, income: Value) {
        self.rewards = self.rewards.add_income(income);
    }

    fn contains(&self, declaration_id: &DeclarationId) -> bool {
        self.declarations.declarations.contains_key(declaration_id)
    }
}

/// Returns true if the declaration is active at `current_epoch`:
/// an activity message has been accepted within `inactivity_period` epochs,
/// and its withdrawal (if any) has not yet taken effect.
fn is_active(declaration: &Declaration, current_epoch: Epoch, config: ServiceParameters) -> bool {
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
    // The epoch when this ledger was created
    epoch: Epoch,
}

impl SdpLedger {
    #[must_use]
    pub fn new(epoch: Epoch) -> Self {
        Self {
            services: rpds::HashTrieMapSync::new_sync(),
            epoch,
        }
    }

    pub fn from_genesis(
        config: &Config,
        mut note_sets: NoteSets,
        epoch_state: &EpochState,
        tx_hash_view: &TxHashView,
        declarations: impl IntoIterator<Item = SignedOperation<SDPDeclareOp, Preverified, GenesisMode>>,
    ) -> Result<(Self, NoteSets, Vec<TxEvent>), Error> {
        let mut sdp = Self::new(epoch_state.epoch())
            .with_blend_service(&config.service_rewards_params.blend, epoch_state);

        let mut all_events = Vec::new();
        for declaration in declarations {
            let service_state = {
                let operation = declaration.operation();
                sdp.services
                    .get(&operation.service_type)
                    .ok_or(Error::ServiceNotFound(operation.service_type))?
            };

            let verified_declaration = declaration
                .into_verified(&SDPDeclareGenesisValidationContext {
                    note_sets: &note_sets,
                    tx_hash_view,
                    declarations: service_state.declarations(),
                    declaration_keys: service_state.declaration_keys(),
                    min_stake: &config.min_stake,
                })
                .map_err(|(_signed_operation, error)| error)?;

            let (result, result_note_sets, events) =
                sdp.try_apply_genesis_sdp_declaration(note_sets, verified_declaration, config)?;
            sdp = result;
            note_sets = result_note_sets;
            all_events.extend(events);
        }

        Ok((sdp, note_sets, all_events))
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
        let service = Service::BlendNetwork(Self::new_service_state(
            ServiceType::BlendNetwork,
            blend::Rewards::new(rewards_settings, epoch_state),
        ));
        self.services = self.services.insert(ServiceType::BlendNetwork, service);
        self
    }

    #[must_use]
    pub fn with_genesis_epoch_state(
        mut self,
        rewards_params: &ServiceRewardsParameters,
        epoch_state: &EpochState,
    ) -> Self {
        // The declarations run before the genesis epoch state is final; no income
        // exists at genesis, so the reward state is rebuilt from the final one.
        self.services = self
            .services
            .iter()
            .map(|(service_type, service)| {
                (
                    *service_type,
                    service
                        .clone()
                        .with_genesis_rewards(rewards_params, epoch_state),
                )
            })
            .collect();
        self
    }

    #[must_use]
    fn new_service_state<R: Rewards>(service_type: ServiceType, rewards: R) -> ServiceState<R> {
        ServiceState {
            service_type,
            declarations: ServiceDeclarations::default(),
            rewards,
        }
    }

    pub fn try_apply_header(
        &self,
        config: &Config,
        last_epoch_state: &EpochState,
        epoch_state: &EpochState,
    ) -> Result<(Self, HeaderEffect), Error> {
        let income_before = self.pending_rewards_income();
        let mut all_reward_notes = Vec::new();
        let mut all_released = Vec::new();
        let mut all_events = Vec::new();

        let services = self
            .services
            .iter()
            .map(|(service, service_state)| {
                let service_params = config
                    .service_params
                    .get(service)
                    .ok_or(Error::EpochParamsNotFound(*service))?;
                let (new_state, reward_notes, released, events) =
                    service_state.clone().try_apply_header(
                        last_epoch_state,
                        epoch_state,
                        *service_params,
                        &config.service_rewards_params,
                    );
                all_reward_notes.extend(reward_notes);
                all_released.extend(released);
                all_events.extend(events);
                Ok::<_, Error>((*service, new_state))
            })
            .collect::<Result<_, _>>()?;
        // SDP Epoch Finalization appends the released notes in ascending order of the
        // declaration id bytes, across services.
        all_released.sort_by_key(|(declaration_id, _)| *declaration_id);

        let ledger = Self {
            epoch: epoch_state.epoch(),
            services,
        };
        // Service Reward Distribution debits the whole income of the settled epochs
        // from blend_rewards_pending, paid or not.
        let settled_rewards = income_before
            .checked_sub(ledger.pending_rewards_income())
            .expect("an epoch transition settles income and never adds any");
        Ok((
            ledger,
            HeaderEffect {
                reward_notes: all_reward_notes,
                settled_rewards,
                released_outputs: all_released
                    .iter()
                    .flat_map(|(_, outputs)| outputs.iter().copied())
                    .collect(),
                events: all_events,
            },
        ))
    }

    pub fn try_apply_genesis_sdp_declaration(
        mut self,
        note_sets: NoteSets,
        declaration: SignedOperation<SDPDeclareOp, Verified, GenesisMode>,
        config: &Config,
    ) -> Result<(Self, NoteSets, Vec<TxEvent>), Error> {
        let operation = declaration.operation();
        let operation_id = operation.id();

        let Some(service_state) = self.services.get_mut(&operation.service_type) else {
            return Err(Error::ServiceNotFound(operation.service_type));
        };

        // Execute SDP Declare
        let (result, events) = declaration
            .execute(SDPDeclareExecutionContext {
                note_sets,
                epoch: self.epoch,
                declarations: service_state.declarations_clone(),
            })
            .map_err(|(_signed_operation, error)| error)?;

        if let Some(declaration) = result.declarations.get(&operation_id) {
            let inactivity_period = config
                .service_params
                .get(&declaration.service_type)
                .map_or(0, |parameters| {
                    parameters.inactivity_period.into_inner().into_inner()
                });
            tracing::info!(
                target: LOG_TARGET,
                diagnostic = BLEND_REACHABILITY,
                event = "sdp_genesis_declaration_applied",
                canonical = true,
                provider_id = ?declaration.provider_id,
                declaration_id = %operation_id,
                ledger_epoch = u32::from(self.epoch),
                ledger_slot = 0u64,
                initial_active_epoch = u32::from(declaration.active),
                inactivity_period,
                "Applied genesis SDP declaration"
            );
        }

        service_state.add_declaration(result.declarations, operation_id);
        Ok((self, result.note_sets, events))
    }

    pub fn try_apply_sdp_declaration(
        mut self,
        note_sets: NoteSets,
        signed_operation: SignedOperation<SDPDeclareOp, Verified, StandardMode>,
        _config: &Config,
    ) -> Result<(Self, NoteSets, Vec<TxEvent>), Error> {
        let operation = signed_operation.operation();
        let operation_service_type = operation.service_type;
        let operation_id = operation.id();

        let Some(service_state) = self.services.get(&operation.service_type) else {
            return Err(Error::ServiceNotFound(operation.service_type));
        };

        let (result, events) = signed_operation
            .execute(SDPDeclareExecutionContext {
                note_sets,
                epoch: self.epoch,
                declarations: service_state.declarations_clone(),
            })
            .map_err(|(_signed_operation, error)| error)?;

        self.services
            .get_mut(&operation_service_type)
            .expect("service was checked before execution")
            .add_declaration(result.declarations, operation_id);
        Ok((self, result.note_sets, events))
    }

    pub fn apply_active_msg(
        mut self,
        signed_operation: SignedOperation<SDPActiveOp, Verified, StandardMode>,
        config: &Config,
    ) -> Result<(Self, Vec<TxEvent>), Error> {
        let operation = signed_operation.operation();
        let operation_declaration_id = operation.declaration_id;
        let operation_metadata = operation.metadata.clone();

        let (service, _) = self.get_service(&operation_declaration_id, config)?;
        let Some(service_state) = self.services.get(&service) else {
            return Err(Error::ServiceNotFound(service));
        };

        let (result, events) = signed_operation
            .execute(SDPActiveExecutionContext {
                epoch: self.epoch,
                declarations: service_state.declarations_clone(),
            })
            .map_err(|(_signed_operation, error)| error)?;

        let provider_id = result
            .declarations
            .get(&operation_declaration_id)
            .expect("the declaration should be in the list after execution")
            .provider_id;

        let service_state = self
            .services
            .get_mut(&service)
            .expect("service was checked before execution");
        service_state.update_declarations(result.declarations);
        service_state.update_rewards(
            provider_id,
            &operation_metadata,
            &config.service_rewards_params,
        )?;

        Ok((self, events))
    }

    pub fn apply_withdrawn_msg(
        mut self,
        note_sets: NoteSets,
        signed_operation: SignedOperation<SDPWithdrawOp, Verified, StandardMode>,
        config: &Config,
    ) -> Result<(Self, NoteSets, Vec<TxEvent>), Error> {
        let operation = signed_operation.operation();

        let (service, _) = self.get_service(&operation.declaration_id, config)?;
        let Some(service_state) = self.services.get_mut(&service) else {
            return Err(Error::ServiceNotFound(service));
        };

        let (result, events) = signed_operation
            .execute(SDPWithdrawExecutionContext {
                declarations: service_state.declarations_clone(),
                note_sets,
                epoch: self.epoch,
            })
            .map_err(|(_signed_operation, error)| error)?;

        service_state.update_declarations(result.declarations);

        Ok((self, result.note_sets, events))
    }

    pub fn add_blend_income(&mut self, income: Value) {
        if let Some(Service::BlendNetwork(state)) =
            self.services.get_mut(&ServiceType::BlendNetwork)
        {
            state.add_income(income);
        }
    }

    #[must_use]
    pub fn pending_rewards_income(&self) -> Value {
        self.services
            .values()
            .map(Service::pending_income)
            .fold(0, |total, income| {
                total
                    .checked_add(income)
                    .expect("the pending service income fits in a Value")
            })
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
                    .filter(|(_, declaration)| is_active(declaration, epoch, *params))
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
        self.services
            .iter()
            .find_map(|(_, service)| service.declarations().get(declaration_id))
    }

    /// Get the declarations for a given service type.
    #[must_use]
    pub fn get_declarations_by_service(&self, service_type: ServiceType) -> Option<&Declarations> {
        self.services.get(&service_type).map(Service::declarations)
    }

    /// Get the keys of the declarations of a given service type.
    #[must_use]
    pub fn get_declaration_keys_by_service(
        &self,
        service_type: ServiceType,
    ) -> Option<&DeclarationKeys> {
        self.services
            .get(&service_type)
            .map(Service::declaration_keys)
    }

    /// Get the service type and declarations for a given declaration ID.
    #[must_use]
    pub fn get_declarations_by_id(&self, declaration_id: &DeclarationId) -> Option<&Declarations> {
        self.services.iter().find_map(|(_, service)| {
            let declarations = service.declarations();
            declarations
                .contains_key(declaration_id)
                .then_some(declarations)
        })
    }

    /// Get the service type and parameters for a given declaration ID.
    fn get_service<'a>(
        &self,
        declaration_id: &DeclarationId,
        config: &'a Config,
    ) -> Result<(ServiceType, &'a ServiceParameters), Error> {
        let service = self
            .services
            .iter()
            .find_map(|(service_type, service)| {
                service.contains(declaration_id).then_some(*service_type)
            })
            .ok_or(Error::DeclarationNotFound(*declaration_id))?;

        config
            .service_params
            .get(&service)
            .map(|params| (service, params))
            .ok_or(Error::ServiceParamsNotFound(service))
    }
}

pub struct HeaderEffect {
    // per service, ascending zk_id
    pub reward_notes: Vec<Note>,
    // income the epoch transition settled
    pub settled_rewards: Value,
    // ascending declaration id order
    pub released_outputs: Vec<NoteCommitment>,
    pub events: Vec<HeaderEvent>,
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU64, sync::Arc};

    use lb_core::{
        mantle::{
            note_set::NoteSet,
            notes::{NoteNullifier, derive_note_nullifier},
            ops::{ZkAndEd25519Proof, ZkTransferProof, op_proof::samples::SampleProof as _},
        },
        sdp::{Locator, SNAPSHOT_FINALIZATION_DELAY},
    };
    use lb_groth16::{AdditiveGroup as _, CompressedGroth16Proof, Fr, fr_from_bytes_unchecked};
    use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519Signature, ZkKey};
    use lb_utils::math::PositiveF64;
    use num_bigint::BigUint;

    use super::*;
    use crate::{
        cryptarchia::tests::note_with_sk, mantle::sdp::test_utils::generate_activity_proof,
    };

    fn setup(service_params: ServiceParameters) -> Config {
        let mut params = HashMap::new();
        params.insert(ServiceType::BlendNetwork, service_params);
        Config {
            service_params: Arc::new(params),
            service_rewards_params: ServiceRewardsParameters {
                blend: blend::RewardsParameters {
                    rounds_per_epoch: NonZeroU64::new(10).unwrap(),
                    message_frequency_per_round: PositiveF64::try_from(1.0).unwrap(),
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

    fn declare_op(
        zk_key: &ZkKey,
        signing_key: &Ed25519Key,
        (input_sk, input): &(Fr, Note),
    ) -> SDPDeclareOp {
        SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locators: "/ip4/1.1.1.1/udp/3000".parse::<Locator>().unwrap().into(),
            provider_id: ProviderId(signing_key.public_key()),
            zk_id: zk_key.to_public_key(),
            inputs: [derive_note_nullifier(&input.commitment(), input_sk)].into(),
            commitment_merkle_root: NoteSet::empty().commitment_root(),
            amount: input.value,
        }
    }

    fn withdraw_op(declaration_id: DeclarationId, nonce: Nonce, output: u64) -> SDPWithdrawOp {
        SDPWithdrawOp {
            declaration_id,
            nonce,
            service_note_nullifier: NoteNullifier(fr_from_bytes_unchecked(&declaration_id.0)),
            outputs: [NoteCommitment(Fr::from(output))].into(),
            excess_value: 0,
        }
    }

    const NONCE: Fr = Fr::ZERO;
    const LOTTERY_0: Fr = Fr::ZERO;
    const LOTTERY_1: Fr = Fr::ZERO;

    fn dummy_epoch_state(epoch: Epoch) -> EpochState {
        EpochState {
            epoch,
            nonce: NONCE,
            blend_pow_difficulty: Fr::ZERO,
            note_sets: NoteSets::new(),
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
            blend_pow_difficulty: Fr::ZERO,
            note_sets: NoteSets::new(),
            total_stake: 100,
            lottery_0: LOTTERY_0,
            lottery_1: LOTTERY_1,
            active_declarations: Arc::new(
                ledger.active_declarations(epoch, &config.service_params),
            ),
        }
    }

    fn epoch_snapshot_contains(
        decl_id: &DeclarationId,
        epoch: Epoch,
        ledger: &SdpLedger,
        config: &Config,
    ) -> bool {
        ledger
            .active_declarations(epoch, &config.service_params)
            .for_service(&ServiceType::BlendNetwork)
            .is_some_and(|m| m.contains_key(decl_id))
    }

    #[test]
    fn failed_activity_does_not_return_partially_updated_ledger() {
        let config = setup(ServiceParameters {
            inactivity_period: 2.try_into().unwrap(),
            epoch: 0.into(),
        });
        let epoch0 = dummy_epoch_state(0.into());
        let ledger = dummy_sdp_ledger(0.into(), &config);
        let utxo = note_with_sk();
        let signing_key = create_signing_key();
        let zk_key = create_zk_key(1);
        let declare_op = declare_op(&zk_key, &signing_key, &utxo);
        let sample_proof = ZkAndEd25519Proof::sample();
        let declare_signed_operation =
            SignedOperation::new(declare_op, sample_proof).into_state_trusted();
        let operation_id = declare_signed_operation.operation().id();

        let (ledger, _, _) = ledger
            .try_apply_sdp_declaration(NoteSets::new(), declare_signed_operation, &config)
            .unwrap();
        let original = ledger.clone();
        let active_op = SDPActiveOp {
            declaration_id: operation_id,
            nonce: 1,
            metadata: ActivityMetadata::Blend(Box::new(generate_activity_proof(
                &zk_key,
                &epoch0,
                &epoch0,
                &config.service_rewards_params.blend,
            ))),
        };
        let placeholder_proof = Ed25519Signature::zero();
        let active_signed_operation =
            SignedOperation::new(active_op, placeholder_proof).into_state_trusted();

        // The freshly-created rewards state has no target epoch yet, so reward
        // calculation fails after operation execution has produced its updated
        // declaration context. The consuming API must not return that partial
        // state to its caller.
        assert_eq!(
            ledger
                .clone()
                .apply_active_msg(active_signed_operation, &config)
                .unwrap_err(),
            Error::RewardsError(RewardsError::TargetEpochNotSet)
        );
        assert_eq!(ledger, original);
    }

    /// `active_declarations` must drop entries that have gone inactive (i.e.,
    /// `active + inactivity_period < snapshot_epoch`).
    #[test]
    fn active_declarations_filters_out_inactive() {
        let config = setup(ServiceParameters {
            inactivity_period: 2.try_into().unwrap(),
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

        let utxo = note_with_sk();
        let signing_key = create_signing_key();
        let zk_key = create_zk_key(1);
        let declare_op = declare_op(&zk_key, &signing_key, &utxo);
        let declaration_id = declare_op.id();
        let proof = ZkAndEd25519Proof {
            zk_proof: ZkTransferProof::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
            ed25519_sig: Ed25519Signature::zero(),
        };
        let signed_operation = SignedOperation::new(declare_op, proof).into_state_trusted();

        let ledger = ledger
            .try_apply_sdp_declaration(NoteSets::new(), signed_operation, &config)
            .map(|(sdp_ledger, _, _)| sdp_ledger)
            .unwrap();

        // Advance to epoch 6 without an activity message. The declaration is
        // inactive past epoch 5 (active=3, inactivity=2 -> 3+2 < 6).
        let mut ledger = ledger;
        for epoch in 2..=6 {
            let new_epoch_state = next_epoch_state(epoch.into(), &ledger, &config);
            (ledger, _) = ledger
                .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
                .unwrap();
            last_epoch_state = new_epoch_state;
        }

        // The declaration is still present in the live ledger ...
        assert!(ledger.get_declaration(&declaration_id).is_some());
        // ... but active_declarations at epoch 6 must filter it out.
        assert!(
            !epoch_snapshot_contains(&declaration_id, 6.into(), &ledger, &config),
            "inactive declaration must be excluded from the epoch-6 active snapshot"
        );
        // whereas active_declarations at epoch 5 must include it.
        assert!(
            epoch_snapshot_contains(&declaration_id, 5.into(), &ledger, &config),
            "declaration must be included in the epoch-5 active snapshot"
        );
    }

    /// Genesis declarations are initialized with `active = created + 2`, so a
    /// declaration created at epoch 0 must still appear in the active set when
    /// it's consumed at epochs 0 and 1.
    #[test]
    fn active_declarations_includes_genesis_at_epochs_0_and_1() {
        let config = setup(ServiceParameters {
            inactivity_period: 2.try_into().unwrap(),
            epoch: 0.into(),
        });

        // Build an SDP ledger with a declaration at epoch 0.
        let ledger = dummy_sdp_ledger(0.into(), &config);
        let utxo = note_with_sk();
        let signing_key = create_signing_key();
        let zk_key = create_zk_key(1);
        let declare_op = declare_op(&zk_key, &signing_key, &utxo);
        let declaration_id = declare_op.id();
        let proof = ZkAndEd25519Proof {
            zk_proof: ZkTransferProof::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
            ed25519_sig: Ed25519Signature::zero(),
        };
        let signed_operation = SignedOperation::new(declare_op, proof).into_state_trusted();

        let ledger = ledger
            .try_apply_sdp_declaration(NoteSets::new(), signed_operation, &config)
            .map(|(sdp_ledger, _, _)| sdp_ledger)
            .unwrap();

        // `active` is initialized to created + 2.
        let declaration = ledger.get_declaration(&declaration_id).unwrap();
        assert_eq!(declaration.active, SNAPSHOT_FINALIZATION_DELAY);

        // At epoch 0 and 1, the declaration must be included in the active set.
        for epoch in [0u32, 1] {
            assert!(
                epoch_snapshot_contains(&declaration_id, epoch.into(), &ledger, &config),
                "genesis declaration must be active at epoch {epoch}"
            );
        }
    }

    /// A withdrawn declaration must remain active until its `withdrawn` epoch
    /// is reached, and become inactive from that epoch onward — even while
    /// the declaration is still present in the live SDP ledger.
    #[test]
    fn active_declarations_filters_out_withdrawn_at_effective_epoch() {
        // Long inactivity epoch so the only filter that fires in this test
        // is the withdrawn-effective-epoch check.
        let config = setup(ServiceParameters {
            inactivity_period: 100.try_into().unwrap(),
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

        let utxo = note_with_sk();
        let signing_key = create_signing_key();
        let zk_key = create_zk_key(1);
        let declare_op = declare_op(&zk_key, &signing_key, &utxo);
        let declaration_id = declare_op.id();
        let proof = ZkAndEd25519Proof {
            zk_proof: ZkTransferProof::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
            ed25519_sig: Ed25519Signature::zero(),
        };
        let signed_operation_declare = SignedOperation::new(declare_op, proof).into_state_trusted();

        let ledger = ledger
            .try_apply_sdp_declaration(NoteSets::new(), signed_operation_declare, &config)
            .map(|(sdp_ledger, _, _)| sdp_ledger)
            .unwrap();

        // Withdraw at epoch 1: `withdrawn = 1 + SNAPSHOT_FINALIZATION_DELAY = 3`.
        let withdraw_op = withdraw_op(declaration_id, 1, 100);
        let proof = ZkAndEd25519Proof::sample();
        let signed_operation_withdraw =
            SignedOperation::new(withdraw_op, proof).into_state_trusted();

        let ledger = ledger
            .apply_withdrawn_msg(NoteSets::new(), signed_operation_withdraw, &config)
            .map(|(sdp_ledger, _, _)| sdp_ledger)
            .unwrap();
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
                epoch_snapshot_contains(&declaration_id, epoch.into(), &ledger, &config),
                "withdrawn-but-not-yet-effective declaration must be active at epoch {epoch}"
            );
        }

        // Snapshot at `withdrawn_epoch` (and beyond) must exclude it.
        for epoch in withdraw_at.into_inner()..=withdraw_at.into_inner() + 2 {
            assert!(
                !epoch_snapshot_contains(&declaration_id, epoch.into(), &ledger, &config),
                "withdrawn declaration must be excluded from the snapshot at epoch {epoch}"
            );
        }
    }

    /// A provider's `active` field is refreshed when it submits an activity
    /// message, and the declaration persists across epochs regardless of how
    /// long it has been inactive.
    #[test]
    fn active_message_refreshes_declaration() {
        let config = setup(ServiceParameters {
            inactivity_period: 2.try_into().unwrap(),
            epoch: 0.into(),
        });

        // Init ledger with no declaration
        let epoch0 = dummy_epoch_state(0.into());
        let mut ledger = dummy_sdp_ledger(0.into(), &config);

        // Move forward to the epoch 1
        let epoch1 = next_epoch_state(1.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch0, &epoch1).unwrap();

        // Add a declaration at epoch 1
        let utxo = note_with_sk();
        let signing_key = create_signing_key();
        let zk_key = create_zk_key(1);
        let declare_op = &declare_op(&zk_key, &signing_key, &utxo);
        let declaration_id = declare_op.id();
        let proof = ZkAndEd25519Proof {
            zk_proof: ZkTransferProof::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
            ed25519_sig: Ed25519Signature::zero(),
        };
        let signed_operation_declare =
            SignedOperation::new(declare_op.clone(), proof).into_state_trusted();

        ledger = ledger
            .try_apply_sdp_declaration(NoteSets::new(), signed_operation_declare, &config)
            .map(|(sdp_ledger, _, _)| sdp_ledger)
            .unwrap();
        let declarations = ledger
            .get_declarations_by_service(ServiceType::BlendNetwork)
            .unwrap();
        assert!(declarations.contains_key(&declaration_id));

        // Move forward to epoch 4 where the provider can submit an activity message.
        // (The provider is expected to provide the service from epoch 3)
        let epoch2 = next_epoch_state(2.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch1, &epoch2).unwrap();
        let epoch3 = next_epoch_state(3.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch2, &epoch3).unwrap();
        let epoch4 = next_epoch_state(4.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch3, &epoch4).unwrap();
        // Check that the declaration is still present.
        let declarations = ledger
            .get_declarations_by_service(ServiceType::BlendNetwork)
            .unwrap();
        assert_eq!(
            declarations.get(&declaration_id).unwrap().active,
            Epoch::new(3)
        );

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
        let proof = Ed25519Signature::zero();
        let signed_operation_active = SignedOperation::new(active_op, proof).into_state_trusted();

        ledger = ledger
            .apply_active_msg(signed_operation_active, &config)
            .map(|(sdp_ledger, _)| sdp_ledger)
            .unwrap();
        let declarations = ledger
            .get_declarations_by_service(ServiceType::BlendNetwork)
            .unwrap();
        assert_eq!(
            declarations.get(&declaration_id).unwrap().active,
            Epoch::new(4) // epoch when the activity message is submitted/accepted
        );

        // Move forward to epoch 7 where declaration will become inactive
        // (active=4, inactivity=2 -> 4+2 < 7).
        let epoch5 = next_epoch_state(5.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch4, &epoch5).unwrap();
        let epoch6 = next_epoch_state(6.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch5, &epoch6).unwrap();
        let epoch7 = next_epoch_state(7.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch6, &epoch7).unwrap();
        // Nevertheless, the declaration should be still present because no withdrawal
        // message was submitted.
        let declarations = ledger
            .get_declarations_by_service(ServiceType::BlendNetwork)
            .unwrap();
        assert_eq!(
            declarations.get(&declaration_id).unwrap().active,
            Epoch::new(4) // not changed
        );
        // but active_declarations at epoch 7 must filter it out.
        assert!(
            !epoch_snapshot_contains(&declaration_id, 7.into(), &ledger, &config),
            "inactive declaration must be excluded from the epoch-7 active snapshot"
        );
        // whereas active_declarations at epoch 6 must include it.
        assert!(
            epoch_snapshot_contains(&declaration_id, 6.into(), &ledger, &config),
            "declaration must be included in the epoch-6 active snapshot"
        );
    }

    #[test]
    fn rewards_distributed_to_active_provider() {
        let config = setup(ServiceParameters {
            inactivity_period: 2.try_into().unwrap(),
            epoch: 0.into(),
        });

        // Init ledger with no declaration.
        let epoch0 = dummy_epoch_state(0.into());
        let mut ledger = dummy_sdp_ledger(0.into(), &config);

        // Move forward to epoch 1 and declare.
        let epoch1 = next_epoch_state(1.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch0, &epoch1).unwrap();

        let utxo = note_with_sk();
        let signing_key = create_signing_key();
        let zk_key = create_zk_key(1);
        let declare_op = declare_op(&zk_key, &signing_key, &utxo);
        let declaration_id = declare_op.id();
        let proof = ZkAndEd25519Proof {
            zk_proof: ZkTransferProof::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
            ed25519_sig: Ed25519Signature::zero(),
        };
        let signed_operation_declare = SignedOperation::new(declare_op, proof).into_state_trusted();

        ledger = ledger
            .try_apply_sdp_declaration(NoteSets::new(), signed_operation_declare, &config)
            .map(|(sdp_ledger, _, _)| sdp_ledger)
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
        let proof = Ed25519Signature::zero();
        let signed_operation_active = SignedOperation::new(active_op, proof).into_state_trusted();

        let ledger = ledger
            .apply_active_msg(signed_operation_active, &config)
            .map(|(sdp_ledger, _)| sdp_ledger)
            .unwrap();

        // Advance to epoch 5: SDP rewards are distributed
        assert_eq!(ledger.pending_rewards_income(), income);
        let epoch5 = next_epoch_state(5.into(), &ledger, &config);
        let (_, effect) = ledger.try_apply_header(&config, &epoch4, &epoch5).unwrap();

        // The single provider is both the only submitter and the premium
        // provider (min hamming distance), so they collect the full `income`.
        let provider_zk_id = zk_key.to_public_key();
        let received: Vec<&Note> = effect
            .reward_notes
            .iter()
            .filter(|note| note.public_key == provider_zk_id)
            .collect();
        assert_eq!(
            received.len(),
            1,
            "the active provider must receive exactly one reward note",
        );
        assert_eq!(
            received[0].value, income,
            "single-provider reward must equal the full accrued income",
        );
        assert_eq!(effect.settled_rewards, income);

        // `SdpRewardDistributed` events must mirror the reward notes one-to-one
        // so wallets can credit provider keys off the header events alone.
        let reward_events: Vec<(ServiceType, Note)> = effect
            .events
            .iter()
            .filter_map(|event| match event {
                HeaderEvent::SdpRewardDistributed { service_type, note } => {
                    Some((*service_type, *note))
                }
                HeaderEvent::SdpWithdrawalReleased { .. }
                | HeaderEvent::ChannelWithdrawalReleased { .. } => None,
            })
            .collect();
        assert_eq!(reward_events.len(), effect.reward_notes.len());
        for (service_type, note) in &reward_events {
            assert_eq!(*service_type, ServiceType::BlendNetwork);
            assert!(effect.reward_notes.contains(note));
        }
    }

    /// Once a Blend declaration is removed, the epoch after its `withdraw_at`,
    /// its `provider_id` and `zk_id` become reusable
    /// — a fresh declaration reusing both must be accepted.
    #[test]
    fn accepts_reused_ids_after_withdrawn_epoch() {
        let config = setup(ServiceParameters {
            inactivity_period: 20.try_into().unwrap(),
            epoch: 0.into(),
        });

        let signing_key = create_signing_key();
        let zk_key = create_zk_key(1);
        let utxo_a = note_with_sk();
        let utxo_b = note_with_sk();

        let declare_a = declare_op(&zk_key, &signing_key, &utxo_a);
        let declaration_id_a = declare_a.id();
        let proof_a = ZkAndEd25519Proof {
            zk_proof: ZkTransferProof::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
            ed25519_sig: Ed25519Signature::zero(),
        };
        let signed_operation_declare_a =
            SignedOperation::new(declare_a, proof_a).into_state_trusted();

        let epoch0 = dummy_epoch_state(0.into());
        let sdp_ledger = dummy_sdp_ledger(0.into(), &config);

        let sdp_ledger = sdp_ledger
            .try_apply_sdp_declaration(NoteSets::new(), signed_operation_declare_a, &config)
            .map(|(sdp_ledger, _, _)| sdp_ledger)
            .unwrap();

        // Withdraw A.
        let withdraw_op = withdraw_op(declaration_id_a, 1, 100);
        let proof_withdraw = ZkAndEd25519Proof::sample();
        let signed_operation_withdraw =
            SignedOperation::new(withdraw_op, proof_withdraw).into_state_trusted();

        let (sdp_ledger, _note_sets, _events) = sdp_ledger
            .apply_withdrawn_msg(NoteSets::new(), signed_operation_withdraw, &config)
            .unwrap();

        let withdraw_epoch = sdp_ledger
            .get_declaration(&declaration_id_a)
            .expect("declaration must still exist until the withdrawn epoch is reached")
            .withdraw_at
            .expect("withdraw_at must be set after withdraw tx is accepted");

        // Advance epochs until A is removed at `withdraw_epoch + 1`.
        let mut sdp_ledger = sdp_ledger;
        let mut last_epoch_state = epoch0;
        for epoch in 1..=withdraw_epoch.into_inner() + 1 {
            let new_epoch_state = next_epoch_state(epoch.into(), &sdp_ledger, &config);
            (sdp_ledger, _) = sdp_ledger
                .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
                .unwrap();
            last_epoch_state = new_epoch_state;
        }
        assert!(
            sdp_ledger.get_declaration(&declaration_id_a).is_none(),
            "declaration A must be removed at the `withdraw_at + 1` epoch"
        );

        // Re-declare reusing A's `provider_id` and `zk_id` (fresh service note
        // and locators, so the `declaration_id` differs). Must be accepted.
        let declare_b = SDPDeclareOp {
            locators: "/ip4/2.2.2.2/udp/3000".parse::<Locator>().unwrap().into(),
            ..declare_op(&zk_key, &signing_key, &utxo_b)
        };
        let proof_b = ZkAndEd25519Proof {
            zk_proof: ZkTransferProof::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
            ed25519_sig: Ed25519Signature::zero(),
        };
        let signed_operation_declare_b =
            SignedOperation::new(declare_b, proof_b).into_state_trusted();

        sdp_ledger
            .try_apply_sdp_declaration(NoteSets::new(), signed_operation_declare_b, &config)
            .expect(
                "Declaration reusing A's provider_id and zk_id must be accepted after A is removed",
            );
    }

    // The verification of `operation` against the declarations of its service,
    // whose keys must be those of the declarations.
    fn verify_declaration(
        ledger: &SdpLedger,
        operation: SDPDeclareOp,
        config: &Config,
    ) -> Result<(), lb_core::mantle::ops::sdp::SdpError> {
        use lb_core::mantle::{
            TxHash, ledger::VerifiableOperation as _, ops::sdp::SDPDeclareVerificationContext,
        };

        let declarations = ledger
            .get_declarations_by_service(operation.service_type)
            .unwrap();
        let declaration_keys = ledger
            .get_declaration_keys_by_service(operation.service_type)
            .unwrap();
        assert_eq!(
            *declaration_keys,
            DeclarationKeys::from_declarations(declarations)
        );
        let tx_hash_view = TxHashView::from(TxHash::from([0; 32]));
        SignedOperation::<_, _, StandardMode>::new(operation, ZkAndEd25519Proof::sample())
            .into_state_trusted::<Preverified>()
            .verify(&SDPDeclareVerificationContext {
                note_sets: &NoteSets::new(),
                block_number: 0,
                tx_hash_view: &tx_hash_view,
                declarations,
                declaration_keys,
                min_stake: &config.min_stake,
            })
            .map(|_| ())
    }

    /// A withdrawn declaration keeps its `provider_id` and `zk_id` until it is
    /// removed, the epoch after its `withdraw_at`.
    #[test]
    fn declared_ids_stay_taken_until_the_withdrawn_declaration_is_removed() {
        use lb_core::mantle::ops::sdp::SdpError;

        let config = setup(ServiceParameters {
            inactivity_period: 20.try_into().unwrap(),
            epoch: 0.into(),
        });
        let signing_key = create_signing_key();
        let other_signing_key = Ed25519Key::from_bytes(&[1; 32]);
        let utxo_a = note_with_sk();
        let utxo_b = note_with_sk();
        let declare_a = declare_op(&create_zk_key(1), &signing_key, &utxo_a);
        let declaration_id = declare_a.id();
        let same_provider_id = declare_op(&create_zk_key(2), &signing_key, &utxo_b);
        let same_zk_id = declare_op(&create_zk_key(1), &other_signing_key, &utxo_b);
        let fresh = declare_op(&create_zk_key(2), &other_signing_key, &utxo_b);
        let duplicate_provider_id = Err(SdpError::DuplicateProviderId {
            service_type: ServiceType::BlendNetwork,
            provider_id: Box::new(same_provider_id.provider_id),
        });
        let duplicate_zk_id = Err(SdpError::DuplicateZkId {
            service_type: ServiceType::BlendNetwork,
            zk_id: same_zk_id.zk_id,
        });

        let ledger = dummy_sdp_ledger(0.into(), &config);
        assert_eq!(
            verify_declaration(&ledger, same_provider_id.clone(), &config),
            Ok(())
        );
        let (ledger, _, _) = ledger
            .try_apply_sdp_declaration(
                NoteSets::new(),
                SignedOperation::new(declare_a, ZkAndEd25519Proof::sample()).into_state_trusted(),
                &config,
            )
            .unwrap();
        let (mut ledger, _, _) = ledger
            .apply_withdrawn_msg(
                NoteSets::new(),
                SignedOperation::new(
                    withdraw_op(declaration_id, 1, 100),
                    ZkAndEd25519Proof::sample(),
                )
                .into_state_trusted(),
                &config,
            )
            .unwrap();
        let withdraw_at = ledger
            .get_declaration(&declaration_id)
            .unwrap()
            .withdraw_at
            .unwrap();

        let mut last_epoch_state = dummy_epoch_state(0.into());
        for epoch in 0..=withdraw_at.into_inner() + 1 {
            if epoch > 0 {
                let epoch_state = next_epoch_state(epoch.into(), &ledger, &config);
                (ledger, _) = ledger
                    .try_apply_header(&config, &last_epoch_state, &epoch_state)
                    .unwrap();
                last_epoch_state = epoch_state;
            }
            assert_eq!(verify_declaration(&ledger, fresh.clone(), &config), Ok(()));
            if ledger.get_declaration(&declaration_id).is_some() {
                assert_eq!(
                    verify_declaration(&ledger, same_provider_id.clone(), &config),
                    duplicate_provider_id
                );
                assert_eq!(
                    verify_declaration(&ledger, same_zk_id.clone(), &config),
                    duplicate_zk_id
                );
            } else {
                assert_eq!(epoch, withdraw_at.into_inner() + 1);
                assert_eq!(
                    verify_declaration(&ledger, same_provider_id.clone(), &config),
                    Ok(())
                );
                assert_eq!(
                    verify_declaration(&ledger, same_zk_id.clone(), &config),
                    Ok(())
                );
            }
        }

        // The keys are rebuilt when decoding.
        let decoded: SdpLedger =
            serde_json::from_str(&serde_json::to_string(&ledger).unwrap()).unwrap();
        assert_eq!(decoded, ledger);
        assert_eq!(
            decoded.get_declaration_keys_by_service(ServiceType::BlendNetwork),
            ledger.get_declaration_keys_by_service(ServiceType::BlendNetwork)
        );
    }

    #[test]
    fn test_withdraw_provider() {
        let config = setup(ServiceParameters {
            inactivity_period: 20.try_into().unwrap(),
            epoch: 0.into(),
        });

        let utxo = note_with_sk();
        let signing_key = create_signing_key();
        let zk_key = create_zk_key(1);

        let declare_op = declare_op(&zk_key, &signing_key, &utxo);
        let declaration_id = declare_op.id();
        let proof = ZkAndEd25519Proof {
            zk_proof: ZkTransferProof::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
            ed25519_sig: Ed25519Signature::zero(),
        };
        let signed_operation_declare = SignedOperation::new(declare_op, proof).into_state_trusted();

        // Initialize ledger with service config and declare
        let epoch0 = dummy_epoch_state(0.into());
        let sdp_ledger = dummy_sdp_ledger(0.into(), &config);

        let sdp_ledger = sdp_ledger
            .try_apply_sdp_declaration(NoteSets::new(), signed_operation_declare, &config)
            .map(|(sdp_ledger, _, _)| sdp_ledger)
            .unwrap();

        // Verify declaration is present
        assert!(sdp_ledger.get_declaration(&declaration_id).is_some());

        // Withdraw the declaration
        let withdraw_op = withdraw_op(declaration_id, 1, 100);
        let proof_withdraw = ZkAndEd25519Proof::sample();
        let signed_operation_withdraw =
            SignedOperation::new(withdraw_op, proof_withdraw).into_state_trusted();

        let sdp_ledger = sdp_ledger
            .apply_withdrawn_msg(NoteSets::new(), signed_operation_withdraw, &config)
            .map(|(sdp_ledger, _, _)| sdp_ledger)
            .unwrap();

        let withdraw_epoch = sdp_ledger
            .get_declaration(&declaration_id)
            .expect("declaration must still exist until the withdrawn epoch is reached")
            .withdraw_at
            .expect("withdraw_at must be set after withdraw tx is accepted");

        // Move forward up to and including `withdraw_at`.
        // The declaration must still be present and the note still in service.
        let mut sdp_ledger = sdp_ledger;
        let mut last_epoch_state = epoch0;
        for epoch in 1..=withdraw_epoch.into_inner() {
            let new_epoch_state = next_epoch_state(epoch.into(), &sdp_ledger, &config);
            let events;
            let released_outputs;
            (
                sdp_ledger,
                HeaderEffect {
                    events,
                    released_outputs,
                    ..
                },
            ) = sdp_ledger
                .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
                .unwrap();
            assert_eq!(count_release_events(events, declaration_id), 0);
            assert_eq!(released_outputs, []);
            last_epoch_state = new_epoch_state;
        }
        assert!(
            sdp_ledger.get_declaration(&declaration_id).is_some(),
            "declaration must still exist at `withdraw_at`"
        );

        // Move forward to the `withdraw_at + 1` epoch. The declaration must
        // be removed and the note must be unlocked.
        let new_epoch_state = next_epoch_state(
            withdraw_epoch.strict_add(Epoch::new(1)),
            &sdp_ledger,
            &config,
        );
        let events;
        let released_outputs;
        (
            sdp_ledger,
            HeaderEffect {
                events,
                released_outputs,
                ..
            },
        ) = sdp_ledger
            .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
            .unwrap();
        assert_eq!(count_release_events(events, declaration_id), 1);
        assert_eq!(released_outputs, vec![NoteCommitment(Fr::from(100u64))]);
        assert!(
            sdp_ledger.get_declaration(&declaration_id).is_none(),
            "declaration must be removed the epoch after `withdraw_at`"
        );
    }

    /// A withdrawal included in epoch `e` sets `withdraw_at = e + 2`. The node
    /// still serves epoch `e + 1`, its report attesting `e + 1` is accepted
    /// during `e + 2`, and the epoch-`e + 1` reward is distributed in the first
    /// block of `e + 3`, right before the declaration is removed.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "This test walks the full withdraw timeline epoch by epoch, and splitting it would not improve readability"
    )]
    fn last_served_epoch_is_rewarded_before_removal() {
        let config = setup(ServiceParameters {
            inactivity_period: 2.try_into().unwrap(),
            epoch: 0.into(),
        });

        let epoch0 = dummy_epoch_state(0.into());
        let mut ledger = dummy_sdp_ledger(0.into(), &config);

        // Declare at epoch 1.
        let epoch1 = next_epoch_state(1.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch0, &epoch1).unwrap();

        let utxo = note_with_sk();
        let signing_key = create_signing_key();
        let zk_key = create_zk_key(1);
        let declare_op = declare_op(&zk_key, &signing_key, &utxo);
        let declaration_id = declare_op.id();
        let proof = ZkAndEd25519Proof {
            zk_proof: ZkTransferProof::new(CompressedGroth16Proof::from_bytes(&[0u8; 128])),
            ed25519_sig: Ed25519Signature::zero(),
        };
        let signed_operation_declare = SignedOperation::new(declare_op, proof).into_state_trusted();
        ledger = ledger
            .try_apply_sdp_declaration(NoteSets::new(), signed_operation_declare, &config)
            .map(|(sdp_ledger, _, _)| sdp_ledger)
            .unwrap();

        // Withdraw at epoch 2 (`e`): `withdraw_at = 4`.
        let epoch2 = next_epoch_state(2.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch1, &epoch2).unwrap();
        let withdraw_op = withdraw_op(declaration_id, 1, 100);
        let proof_withdraw = ZkAndEd25519Proof::sample();
        let signed_operation_withdraw =
            SignedOperation::new(withdraw_op, proof_withdraw).into_state_trusted();
        ledger = ledger
            .apply_withdrawn_msg(NoteSets::new(), signed_operation_withdraw, &config)
            .map(|(sdp_ledger, _, _)| sdp_ledger)
            .unwrap();
        let withdraw_at = ledger
            .get_declaration(&declaration_id)
            .unwrap()
            .withdraw_at
            .expect("withdraw must set the withdraw_at");
        assert_eq!(withdraw_at, Epoch::new(4));

        // Epoch 3 (`e + 1`) is the last served epoch: the node is still in the
        // snapshot and block-reward income accrues.
        let epoch3 = next_epoch_state(3.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch2, &epoch3).unwrap();
        assert!(
            epoch_snapshot_contains(&declaration_id, 3.into(), &ledger, &config),
            "the node must still serve the epoch before `withdraw_at`"
        );
        let income: Value = 1000;
        ledger.add_blend_income(income);

        // Epoch 4 (`withdraw_at`): the node leaves the snapshot but is not
        // removed yet, and its report attesting epoch 3 is accepted.
        let epoch4 = next_epoch_state(4.into(), &ledger, &config);
        let events;
        let released_outputs;
        (
            ledger,
            HeaderEffect {
                events,
                released_outputs,
                ..
            },
        ) = ledger.try_apply_header(&config, &epoch3, &epoch4).unwrap();
        assert!(!epoch_snapshot_contains(
            &declaration_id,
            4.into(),
            &ledger,
            &config
        ));
        assert_eq!(
            count_release_events(events, declaration_id),
            0,
            "the outputs must not be released at `withdraw_at`"
        );
        assert_eq!(released_outputs, []);
        assert!(ledger.get_declaration(&declaration_id).is_some());

        let active_op = SDPActiveOp {
            declaration_id,
            nonce: 2,
            metadata: ActivityMetadata::Blend(Box::new(generate_activity_proof(
                &zk_key,
                &epoch3,
                &epoch4,
                &config.service_rewards_params.blend,
            ))),
        };
        let proof_active = Ed25519Signature::zero();
        let signed_operation_active =
            SignedOperation::new(active_op, proof_active).into_state_trusted();
        ledger = ledger
            .apply_active_msg(signed_operation_active, &config)
            .map(|(sdp_ledger, _)| sdp_ledger)
            .expect("the report attesting `withdraw_at - 1` must be accepted at `withdraw_at`");

        // Epoch 5 (`withdraw_at + 1`): the epoch-3 reward is distributed and
        // the declaration removed in the same header, in that order.
        let epoch5 = next_epoch_state(5.into(), &ledger, &config);
        let (ledger, effect) = ledger.try_apply_header(&config, &epoch4, &epoch5).unwrap();
        let received: Vec<&Note> = effect
            .reward_notes
            .iter()
            .filter(|note| note.public_key == zk_key.to_public_key())
            .collect();
        assert_eq!(
            received.len(),
            1,
            "the withdrawing provider must be paid for its last served epoch"
        );
        assert_eq!(received[0].value, income);
        assert!(matches!(
            effect.events.first(),
            Some(HeaderEvent::SdpRewardDistributed { .. })
        ));
        assert!(matches!(
            effect.events.last(),
            Some(HeaderEvent::SdpWithdrawalReleased { .. })
        ));
        assert_eq!(count_release_events(effect.events, declaration_id), 1);
        assert_eq!(
            effect.released_outputs,
            vec![NoteCommitment(Fr::from(100u64))]
        );
        assert!(
            ledger.get_declaration(&declaration_id).is_none(),
            "declaration must be removed the epoch after `withdraw_at`"
        );
    }

    #[test]
    fn withdrawal_outputs_are_released_in_ascending_declaration_id_order() {
        let config = setup(ServiceParameters {
            inactivity_period: 2.try_into().unwrap(),
            epoch: 0.into(),
        });
        let mut last_epoch_state = dummy_epoch_state(0.into());
        let mut ledger = dummy_sdp_ledger(0.into(), &config);
        let mut note_sets = NoteSets::new();
        let mut declarations: Vec<SDPDeclareOp> = (1..=2u8)
            .map(|seed| {
                declare_op(
                    &create_zk_key(u64::from(seed)),
                    &Ed25519Key::from_bytes(&[seed; 32]),
                    &note_with_sk(),
                )
            })
            .collect();
        // Withdraw the larger declaration id first, so that insertion order is not id
        // order.
        declarations.sort_by_key(|declaration| std::cmp::Reverse(declaration.id()));
        for (index, declaration) in declarations.iter().enumerate() {
            let events;
            (ledger, note_sets, events) = ledger
                .try_apply_sdp_declaration(
                    note_sets,
                    SignedOperation::new(declaration.clone(), ZkAndEd25519Proof::sample())
                        .into_state_trusted(),
                    &config,
                )
                .unwrap();
            assert_eq!(events, []);
            (ledger, note_sets, _) = ledger
                .apply_withdrawn_msg(
                    note_sets,
                    SignedOperation::new(
                        withdraw_op(declaration.id(), 1, 100 + index as u64),
                        ZkAndEd25519Proof::sample(),
                    )
                    .into_state_trusted(),
                    &config,
                )
                .unwrap();
        }

        // Withdrawn at epoch 0: `withdraw_at = 2`, released in the first block of epoch
        // 3.
        let mut released = Vec::new();
        for epoch in 1..=3u32 {
            let epoch_state = next_epoch_state(epoch.into(), &ledger, &config);
            let effect;
            (ledger, effect) = ledger
                .try_apply_header(&config, &last_epoch_state, &epoch_state)
                .unwrap();
            released.push(effect.released_outputs);
            last_epoch_state = epoch_state;
        }
        assert_eq!(
            released,
            vec![
                vec![],
                vec![],
                vec![
                    NoteCommitment(Fr::from(101u64)),
                    NoteCommitment(Fr::from(100u64))
                ]
            ]
        );
        assert!(
            declarations
                .iter()
                .all(|declaration| ledger.get_declaration(&declaration.id()).is_none())
        );
    }

    fn count_release_events(events: Vec<HeaderEvent>, declaration_id: DeclarationId) -> usize {
        events
            .into_iter()
            .filter(|event| {
                matches!(
                    event,
                    HeaderEvent::SdpWithdrawalReleased { declaration_id: d, .. } if *d == declaration_id
                )
            })
            .count()
    }
}
