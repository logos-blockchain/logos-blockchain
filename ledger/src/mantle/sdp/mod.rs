pub mod rewards;
#[cfg(test)]
pub(crate) mod test_utils;

use std::collections::HashMap;

use lb_blend_message::crypto::proofs::RealProofsVerifier;
use lb_core::{
    block::BlockNumber,
    crypto::Hash,
    events::{HeaderEvent, TxEvent},
    mantle::{
        NoteId, OpProof, Utxo, Value,
        channel::Channels,
        ledger::{Declarations, Operation},
        ops::sdp::{
            SDPActiveExecutionContext, SDPActiveOp, SDPDeclareExecutionContext, SDPDeclareOp,
            SDPDeclareValidationContext, SDPWithdrawExecutionContext, SDPWithdrawOp,
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
use rewards::{Error as RewardsError, Rewards};
use tracing::debug;

use crate::{EpochState, UtxoTree, mantle::sdp::rewards::blend};

const LOG_TARGET: &str = "ledger::mantle::sdp";

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
    ) -> (Self, Vec<Utxo>, Vec<HeaderEvent>) {
        match self {
            Self::BlendNetwork(state) => {
                let (new_state, utxos, events) = state.try_apply_header(
                    last_epoch_state,
                    epoch_state,
                    config,
                    &rewards_params.blend,
                );
                (Self::BlendNetwork(new_state), utxos, events)
            }
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
    service_type: ServiceType,
    // Rewards calculation and tracking for this service
    pub rewards: R,
}

impl<R: Rewards> ServiceState<R> {
    fn try_apply_header(
        mut self,
        last_epoch_state: &EpochState,
        epoch_state: &EpochState,
        service_params: ServiceParameters,
        rewards_params: &R::Params,
    ) -> (Self, Vec<Utxo>, Vec<HeaderEvent>) {
        let mut reward_utxos = Vec::new();
        let mut events = Vec::new();

        if last_epoch_state.epoch() < epoch_state.epoch() {
            // Update and distribute rewards
            (self.rewards, reward_utxos) = self.rewards.update_epoch(
                last_epoch_state,
                epoch_state,
                &service_params,
                rewards_params,
            );
            events.extend(reward_utxos.iter().map(|utxo| {
                debug!(
                    target: LOG_TARGET,
                    service_type = ?self.service_type,
                    ?utxo,
                    old_epoch = %last_epoch_state.epoch,
                    new_epoch = %epoch_state.epoch,
                    "SDP reward distributed",
                );
                HeaderEvent::SdpRewardDistributed {
                    service_type: self.service_type,
                    utxo: *utxo,
                }
            }));
        }

        (self, reward_utxos, events)
    }

    fn add_income(&mut self, income: Value) {
        self.rewards = self.rewards.add_income(income);
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
    declarations: Declarations,
    services: rpds::HashTrieMapSync<ServiceType, Service>,
    locked_notes: LockedNotes,
    // The epoch when this ledger was created
    epoch: Epoch,
}

impl SdpLedger {
    #[must_use]
    pub fn new(epoch: Epoch) -> Self {
        Self {
            declarations: Declarations::new(),
            services: rpds::HashTrieMapSync::new_sync(),
            locked_notes: LockedNotes::new(),
            epoch,
        }
    }

    pub fn from_genesis<'a>(
        config: &Config,
        utxo_tree: &UtxoTree,
        channels: &Channels,
        epoch_state: &EpochState,
        ops: impl Iterator<Item = (&'a SDPDeclareOp, &'a OpProof)> + 'a,
    ) -> Result<(Self, Vec<TxEvent>), Error> {
        let mut sdp = Self::new(epoch_state.epoch())
            .with_blend_service(&config.service_rewards_params.blend, epoch_state);

        let mut all_events = Vec::new();
        for (op, _) in ops {
            let (result, events) =
                sdp.try_apply_genesis_sdp_declaration(utxo_tree, channels, op, config)?;
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
        let service = Service::BlendNetwork(Self::new_service_state(
            ServiceType::BlendNetwork,
            blend::Rewards::new(rewards_settings, epoch_state),
        ));
        self.services = self.services.insert(ServiceType::BlendNetwork, service);
        self
    }

    #[must_use]
    const fn new_service_state<R: Rewards>(
        service_type: ServiceType,
        rewards: R,
    ) -> ServiceState<R> {
        ServiceState {
            service_type,
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
        let mut new_ledger = self.clone();

        if last_epoch_state.epoch() < epoch_state.epoch() {
            all_events
                .extend(new_ledger.unlock_and_remove_withdrawn_declarations(epoch_state.epoch()));
        }

        new_ledger.services = self
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
                    *service_params,
                    &config.service_rewards_params,
                );
                all_reward_utxos.extend(reward_utxos);
                all_events.extend(events);
                Ok::<_, Error>((*service, new_state))
            })
            .collect::<Result<_, _>>()?;
        new_ledger.epoch = epoch_state.epoch();

        Ok((
            new_ledger,
            HeaderEffect {
                reward_utxos: all_reward_utxos,
                events: all_events,
            },
        ))
    }

    /// For every withdrawn declaration whose `withdrawn` epoch has been
    /// reached, unlock the locked note and remove the declaration from the
    /// registry.
    ///
    /// Returns one [`HeaderEvent::SdpNoteUnlocked`] event per unlocked note.
    fn unlock_and_remove_withdrawn_declarations(&mut self, epoch: Epoch) -> Vec<HeaderEvent> {
        let mut events = Vec::new();

        // The removals go to a clone.
        let mut declarations = self.declarations.clone();
        for (id, declaration) in &self.declarations {
            let Some(withdraw_at) = declaration.withdraw_at else {
                continue;
            };
            if epoch < withdraw_at {
                continue;
            }
            if self
                .locked_notes
                .is_locked_for_service(&declaration.locked_note_id, &declaration.service_type)
            {
                self.locked_notes
                    .unlock(declaration.service_type, &declaration.locked_note_id)
                    .expect("unlocking note from withdrawn declaration must be successful if it hasn't been unlocked yet");
                events.push(HeaderEvent::SdpNoteUnlocked {
                    note_id: declaration.locked_note_id,
                    service_type: declaration.service_type,
                    declaration_id: *id,
                });
            }
            (declarations, _) = declarations
                .remove(id)
                .expect("the declaration is in the tree");
        }
        self.declarations = declarations;

        events
    }

    pub fn try_apply_genesis_sdp_declaration(
        mut self,
        utxo_tree: &UtxoTree,
        channels: &Channels,
        op: &SDPDeclareOp,
        config: &Config,
    ) -> Result<(Self, Vec<TxEvent>), Error> {
        if !self.services.contains_key(&op.service_type) {
            return Err(Error::ServiceNotFound(op.service_type));
        }

        // Validate SDP Declare
        // TODO: Genesis has a different verification flow than `SignedMantleTx`.
        // Refactor into a   type state.
        op.validate(&SDPDeclareGenesisValidationContext {
            utxo_tree,
            channels,
            locked_notes: &self.locked_notes,
            declarations: &self.declarations,
            min_stake: &config.min_stake,
        })?;

        // Execute SDP Declare
        let (result, events) =
            <SDPDeclareOp as Operation<SDPDeclareGenesisValidationContext>>::execute(
                op,
                SDPDeclareExecutionContext {
                    utxo_tree: utxo_tree.clone(),
                    epoch: self.epoch,
                    declarations: self.declarations.clone(),
                    locked_notes: self.locked_notes.clone(),
                    min_stake: config.min_stake,
                },
            )?;

        self.locked_notes = result.locked_notes;
        self.declarations = result.declarations;
        Ok((self, events))
    }

    pub fn try_apply_sdp_declaration(
        mut self,
        utxo_tree: &UtxoTree,
        op: &SDPDeclareOp,
        config: &Config,
    ) -> Result<(Self, Vec<TxEvent>), Error> {
        if !self.services.contains_key(&op.service_type) {
            return Err(Error::ServiceNotFound(op.service_type));
        }

        let (result, events) = <SDPDeclareOp as Operation<SDPDeclareValidationContext>>::execute(
            op,
            SDPDeclareExecutionContext {
                utxo_tree: utxo_tree.clone(),
                epoch: self.epoch,
                declarations: self.declarations.clone(),
                locked_notes: self.locked_notes.clone(),
                min_stake: config.min_stake,
            },
        )?;

        self.locked_notes = result.locked_notes;
        self.declarations = result.declarations;
        Ok((self, events))
    }

    pub fn apply_active_msg(
        mut self,
        op: &SDPActiveOp,
        config: &Config,
    ) -> Result<(Self, Vec<TxEvent>), Error> {
        let (service, _) = self.get_service(&op.declaration_id, config)?;
        let Some(service_state) = self.services.get_mut(&service) else {
            return Err(Error::ServiceNotFound(service));
        };

        let (result, events) = op.execute(SDPActiveExecutionContext {
            epoch: self.epoch,
            declarations: self.declarations.clone(),
        })?;

        let provider_id = result
            .declarations
            .get(&op.declaration_id)
            .expect("the declaration should be in the list after execution")
            .provider_id;

        self.declarations = result.declarations;
        service_state.update_rewards(provider_id, &op.metadata, &config.service_rewards_params)?;

        Ok((self, events))
    }

    pub fn apply_withdrawn_msg(
        mut self,
        op: &SDPWithdrawOp,
        config: &Config,
    ) -> Result<(Self, Vec<TxEvent>), Error> {
        let (service, _) = self.get_service(&op.declaration_id, config)?;
        if !self.services.contains_key(&service) {
            return Err(Error::ServiceNotFound(service));
        }

        let (result, events) = op.execute(SDPWithdrawExecutionContext {
            declarations: self.declarations.clone(),
            locked_notes: self.locked_notes.clone(),
            epoch: self.epoch,
        })?;

        self.locked_notes = result.locked_notes;
        self.declarations = result.declarations;

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
    pub const fn declarations(&self) -> &Declarations {
        &self.declarations
    }

    /// Commitment to the declarations of all services.
    #[must_use]
    pub fn declarations_root(&self) -> Hash {
        self.declarations.root()
    }

    /// Returns the declarations that are active at `epoch`.
    ///
    /// Declarations of services missing from `service_params` are dropped.
    #[must_use]
    pub fn active_declarations(
        &self,
        epoch: Epoch,
        service_params: &HashMap<ServiceType, ServiceParameters>,
    ) -> Declarations {
        // The snapshot drops the inactive declarations from a clone of the
        // live tree, so the surviving ones keep the position they occupy
        // there and the root doesn't depend on the iteration order.
        let mut snapshot = self.declarations.clone();
        for (declaration_id, declaration) in &self.declarations {
            let active = service_params
                .get(&declaration.service_type)
                .is_some_and(|params| is_active(declaration, epoch, *params));
            if !active {
                (snapshot, _) = snapshot
                    .remove(declaration_id)
                    .expect("the declaration is in the tree");
            }
        }
        snapshot
    }

    #[must_use]
    pub fn get_declaration(&self, declaration_id: &DeclarationId) -> Option<&Declaration> {
        self.declarations.get_ref(declaration_id)
    }

    /// Get the service type and parameters for a given declaration ID.
    fn get_service<'a>(
        &self,
        declaration_id: &DeclarationId,
        config: &'a Config,
    ) -> Result<(ServiceType, &'a ServiceParameters), Error> {
        let service = self
            .declarations
            .get_ref(declaration_id)
            .ok_or(Error::DeclarationNotFound(*declaration_id))?
            .service_type;

        config
            .service_params
            .get(&service)
            .map(|params| (service, params))
            .ok_or(Error::ServiceParamsNotFound(service))
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
            active_declarations: Arc::new(Declarations::default()),
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

    fn epoch_snapshot_contains(
        decl_id: &DeclarationId,
        epoch: Epoch,
        ledger: &SdpLedger,
        config: &Config,
    ) -> bool {
        ledger
            .active_declarations(epoch, &config.service_params)
            .contains(decl_id)
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
        let ledger = ledger
            .try_apply_sdp_declaration(&utxo_tree(vec![utxo]), declare_op, &config)
            .map(|(sdp_ledger, _)| sdp_ledger)
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
        let ledger = ledger
            .try_apply_sdp_declaration(&utxo_tree(vec![utxo]), declare_op, &config)
            .map(|(sdp_ledger, _)| sdp_ledger)
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
        let ledger = ledger
            .try_apply_sdp_declaration(&utxo_tree(vec![utxo]), declare_op, &config)
            .map(|(sdp_ledger, _)| sdp_ledger)
            .unwrap();

        // Withdraw at epoch 1: `withdrawn = 1 + SNAPSHOT_FINALIZATION_DELAY = 3`.
        let withdraw_op = &SDPWithdrawOp {
            declaration_id,
            nonce: 1,
            locked_note_id: note_id,
        };
        let ledger = ledger
            .apply_withdrawn_msg(withdraw_op, &config)
            .map(|(sdp_ledger, _)| sdp_ledger)
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
        ledger = ledger
            .try_apply_sdp_declaration(&utxo_tree(vec![utxo]), declare_op, &config)
            .map(|(sdp_ledger, _)| sdp_ledger)
            .unwrap();
        let declarations = ledger.declarations();
        assert!(declarations.contains(&declaration_id));

        // Move forward to epoch 4 where the provider can submit an activity message.
        // (The provider is expected to provide the service from epoch 3)
        let epoch2 = next_epoch_state(2.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch1, &epoch2).unwrap();
        let epoch3 = next_epoch_state(3.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch2, &epoch3).unwrap();
        let epoch4 = next_epoch_state(4.into(), &ledger, &config);
        (ledger, _) = ledger.try_apply_header(&config, &epoch3, &epoch4).unwrap();
        // Check that the declaration is still present.
        let declarations = ledger.declarations();
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
        ledger = ledger
            .apply_active_msg(&active_op, &config)
            .map(|(sdp_ledger, _)| sdp_ledger)
            .unwrap();
        let declarations = ledger.declarations();
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
        let declarations = ledger.declarations();
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
        ledger = ledger
            .try_apply_sdp_declaration(&utxo_tree(vec![utxo]), declare_op, &config)
            .map(|(sdp_ledger, _)| sdp_ledger)
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
        let ledger = ledger
            .apply_active_msg(&active_op, &config)
            .map(|(sdp_ledger, _)| sdp_ledger)
            .unwrap();

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

        // `SdpRewardDistributed` events must mirror the reward UTXOs one-to-one
        // so wallets can credit provider keys off the header events alone.
        let reward_events: Vec<(ServiceType, Utxo)> = effect
            .events
            .iter()
            .filter_map(|event| match event {
                HeaderEvent::SdpRewardDistributed { service_type, utxo } => {
                    Some((*service_type, *utxo))
                }
                HeaderEvent::SdpNoteUnlocked { .. } => None,
            })
            .collect();
        assert_eq!(reward_events.len(), effect.reward_utxos.len());
        for (service_type, utxo) in &reward_events {
            assert_eq!(*service_type, ServiceType::BlendNetwork);
            assert!(effect.reward_utxos.contains(utxo));
        }
    }

    /// Once a Blend declaration is withdrawn/removed at its `withdraw_at`
    /// epoch, its `provider_id` and `zk_id` become reusable
    /// — a fresh declaration reusing both must be accepted.
    #[test]
    fn accepts_reused_ids_after_withdrawn_epoch() {
        let config = setup(ServiceParameters {
            inactivity_period: 20.try_into().unwrap(),
            epoch: 0.into(),
        });

        let signing_key = create_signing_key();
        let zk_key = create_zk_key(1);
        let (_utxo_sk_a, utxo_a) = utxo_with_sk();
        let (_utxo_sk_b, utxo_b) = utxo_with_sk();

        let declare_a = SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locked_note_id: utxo_a.id(),
            zk_id: zk_key.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: "/ip4/1.1.1.1/udp/0".parse::<Locator>().unwrap().into(),
        };
        let declaration_id_a = declare_a.id();

        let epoch0 = dummy_epoch_state(0.into());
        let sdp_ledger = dummy_sdp_ledger(0.into(), &config);
        let utxos = utxo_tree(vec![utxo_a, utxo_b]);

        let sdp_ledger = sdp_ledger
            .try_apply_sdp_declaration(&utxos, &declare_a, &config)
            .map(|(sdp_ledger, _)| sdp_ledger)
            .unwrap();

        // Withdraw A.
        let withdraw_op = SDPWithdrawOp {
            declaration_id: declaration_id_a,
            nonce: 1,
            locked_note_id: utxo_a.id(),
        };
        let (sdp_ledger, _events) = sdp_ledger
            .apply_withdrawn_msg(&withdraw_op, &config)
            .unwrap();

        let withdraw_epoch = sdp_ledger
            .get_declaration(&declaration_id_a)
            .expect("declaration must still exist until the withdrawn epoch is reached")
            .withdraw_at
            .expect("withdraw_at must be set after withdraw tx is accepted");

        // Advance epochs until A is removed at `withdraw_epoch`.
        let mut sdp_ledger = sdp_ledger;
        let mut last_epoch_state = epoch0;
        for epoch in 1..=withdraw_epoch.into_inner() {
            let new_epoch_state = next_epoch_state(epoch.into(), &sdp_ledger, &config);
            (sdp_ledger, _) = sdp_ledger
                .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
                .unwrap();
            last_epoch_state = new_epoch_state;
        }
        assert!(
            sdp_ledger.get_declaration(&declaration_id_a).is_none(),
            "declaration A must be removed at the withdrawn epoch"
        );

        // Re-declare reusing A's `provider_id` and `zk_id` (fresh locked note
        // and locators, so the `declaration_id` differs). Must be accepted.
        let declare_b = SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locked_note_id: utxo_b.id(),
            zk_id: zk_key.to_public_key(),
            provider_id: ProviderId(signing_key.public_key()),
            locators: "/ip4/2.2.2.2/udp/0".parse::<Locator>().unwrap().into(),
        };
        sdp_ledger
            .try_apply_sdp_declaration(&utxos, &declare_b, &config)
            .expect(
                "Declaration reusing A's provider_id and zk_id must be accepted after A is removed",
            );
    }

    #[test]
    fn test_withdraw_provider() {
        let config = setup(ServiceParameters {
            inactivity_period: 20.try_into().unwrap(),
            epoch: 0.into(),
        });

        let service_a = ServiceType::BlendNetwork;
        let (_utxo_sk, utxo) = utxo_with_sk();
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
        let sdp_ledger = sdp_ledger
            .try_apply_sdp_declaration(&utxo_tree, declare_op, &config)
            .map(|(sdp_ledger, _)| sdp_ledger)
            .unwrap();

        // Verify declaration is present
        assert!(sdp_ledger.get_declaration(&declaration_id).is_some());

        // Withdraw the declaration
        let withdraw_op = &SDPWithdrawOp {
            declaration_id,
            nonce: 1,
            locked_note_id: note_id,
        };
        let sdp_ledger = sdp_ledger
            .apply_withdrawn_msg(withdraw_op, &config)
            .map(|(sdp_ledger, _)| sdp_ledger)
            .unwrap();

        let withdraw_epoch = sdp_ledger
            .get_declaration(&declaration_id)
            .expect("declaration must still exist until the withdrawn epoch is reached")
            .withdraw_at
            .expect("withdraw_at must be set after withdraw tx is accepted");

        // Move forward to the epoch just before the withdrawn epoch.
        // The declaration must still be present and the note still locked.
        let mut sdp_ledger = sdp_ledger;
        let mut last_epoch_state = epoch0;
        for epoch in 1..withdraw_epoch.into_inner() {
            let new_epoch_state = next_epoch_state(epoch.into(), &sdp_ledger, &config);
            let events;
            (sdp_ledger, HeaderEffect { events, .. }) = sdp_ledger
                .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
                .unwrap();
            assert_eq!(
                count_unlock_events(events, note_id, service_a, declaration_id),
                0
            );
            last_epoch_state = new_epoch_state;
        }
        assert!(
            sdp_ledger.get_declaration(&declaration_id).is_some(),
            "declaration must still exist before the withdrawn epoch is reached"
        );
        assert!(
            sdp_ledger
                .locked_notes()
                .is_locked_for_service(&declare_op.locked_note_id, &ServiceType::BlendNetwork),
            "the provider's note must still be locked before the withdrawn epoch is reached"
        );

        // Move forward to the withdrawn epoch. The declaration must be removed
        // and the note must be unlocked.
        let new_epoch_state = next_epoch_state(withdraw_epoch, &sdp_ledger, &config);
        let events;
        (sdp_ledger, HeaderEffect { events, .. }) = sdp_ledger
            .try_apply_header(&config, &last_epoch_state, &new_epoch_state)
            .unwrap();
        assert_eq!(
            count_unlock_events(events, note_id, service_a, declaration_id),
            1
        );
        assert!(
            sdp_ledger.get_declaration(&declaration_id).is_none(),
            "declaration must be removed at the withdrawn epoch"
        );
        assert!(
            !sdp_ledger
                .locked_notes()
                .is_locked_for_service(&declare_op.locked_note_id, &ServiceType::BlendNetwork),
            "the provider's note must be unlocked at the withdrawn epoch"
        );
    }

    fn count_unlock_events(
        events: Vec<HeaderEvent>,
        note_id: NoteId,
        service_type: ServiceType,
        declaration_id: DeclarationId,
    ) -> usize {
        events
            .into_iter()
            .filter(|event| {
                matches!(
                    event,
                    HeaderEvent::SdpNoteUnlocked {
                        note_id: n,
                        service_type: s,
                        declaration_id: d,
                    } if *n == note_id && *s == service_type && *d == declaration_id
                )
            })
            .count()
    }
}
