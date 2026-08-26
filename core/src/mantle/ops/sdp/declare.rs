use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::ZkPublicKey;

use super::{SDPDeclareOp, SdpError};
use crate::{
    events::TxEvent,
    mantle::{
        Note, Value,
        channel::Channels,
        gas::{Gas, MainnetGasProfile, OperationGas, SignedOperationExecutionGas},
        ledger::{
            Declarations, ExecutableOperation, PreverifiableOperation, ProvableOperation, Utxos,
            VerifiableOperation,
            verification_mode::{GenesisMode, StandardMode, VerificationMode},
        },
        ops::{SignedOperation, ZkAndEd25519Proof},
        transactions::{
            hash::TxHashView,
            states::{Preverified, Unverified, VerificationState, Verified},
        },
    },
    sdp::{Declaration, MinStake, locked_notes::LockedNotes},
};

trait SDPDeclareValidationExt {
    fn validate(
        &self,
        note: Note,
        channels: &Channels,
        declarations: &Declarations,
        locked_notes: &LockedNotes,
        min_stake: &MinStake,
    ) -> Result<(), SdpError>;

    fn execute(
        &self,
        context: SDPDeclareExecutionContext,
    ) -> Result<(SDPDeclareExecutionContext, Vec<TxEvent>), SdpError>;
}

impl SDPDeclareValidationExt for SDPDeclareOp {
    fn validate(
        &self,
        note: Note,
        channels: &Channels,
        declarations: &Declarations,
        locked_notes: &LockedNotes,
        min_stake: &MinStake,
    ) -> Result<(), SdpError> {
        // Check that the declaration doesn't already exist
        if declarations.contains_key(&self.id()) {
            return Err(SdpError::DuplicateDeclaration(self.id()));
        }
        validate_service_scoped_uniqueness(self, declarations)?;

        // A channel note cannot be used as collateral for a service declaration.
        if channels.is_channel_note(&self.locked_note_id) {
            return Err(SdpError::ChannelNote(self.locked_note_id));
        }

        // Ensure value of locked note is sufficient for joining the service.
        if note.value < min_stake.threshold {
            return Err(SdpError::NoteInsufficientValue {
                note_id: self.locked_note_id,
                value: note.value,
            });
        }

        // Ensure the note has not already been locked for this service.
        if locked_notes.is_locked_for_service(&self.locked_note_id, &self.service_type) {
            return Err(SdpError::NoteAlreadyUsedForService {
                note_id: self.locked_note_id,
                service_type: self.service_type,
            });
        }

        Ok(())
    }

    fn execute(
        &self,
        mut context: SDPDeclareExecutionContext,
    ) -> Result<(SDPDeclareExecutionContext, Vec<TxEvent>), SdpError> {
        let declaration_id = self.id();
        let declaration = Declaration::new(context.epoch, self);
        context.declarations = context.declarations.insert(declaration_id, declaration);
        let utxo = context
            .utxo_tree
            .utxos()
            .get(&self.locked_note_id)
            .expect("The operation should have been checked")
            .0;

        context.locked_notes = context
            .locked_notes
            .lock(
                &context.min_stake,
                self.service_type,
                utxo.note,
                &self.locked_note_id,
            )
            .map_err(|_| SdpError::UnexpectedError)?;

        Ok((context, Vec::new()))
    }
}

/// `provider_id` and `zk_id` must each be unique within the same service.
fn validate_service_scoped_uniqueness(
    op: &SDPDeclareOp,
    declarations: &Declarations,
) -> Result<(), SdpError> {
    declarations
        .values()
        .filter(|d| d.service_type == op.service_type)
        .try_for_each(|existing| {
            if existing.provider_id == op.provider_id {
                Err(SdpError::DuplicateProviderId {
                    service_type: op.service_type,
                    provider_id: Box::new(op.provider_id),
                })
            } else if existing.zk_id == op.zk_id {
                Err(SdpError::DuplicateZkId {
                    service_type: op.service_type,
                    zk_id: op.zk_id,
                })
            } else {
                Ok(())
            }
        })
}

pub struct SDPDeclarePreverificationContext<'a> {
    pub tx_hash_view: &'a TxHashView,
}

pub struct SDPDeclareVerificationContext<'a> {
    pub utxo_tree: &'a Utxos,
    pub channels: &'a Channels,
    pub locked_notes: &'a LockedNotes,
    pub tx_hash_view: &'a TxHashView,
    pub declarations: &'a Declarations,
    pub min_stake: &'a MinStake,
}

pub struct SDPDeclareGenesisValidationContext<'a> {
    pub utxo_tree: &'a Utxos,
    pub channels: &'a Channels,
    pub locked_notes: &'a LockedNotes,
    pub declarations: &'a Declarations,
    pub min_stake: &'a MinStake,
}

pub struct SDPDeclareExecutionContext {
    pub utxo_tree: Utxos,
    pub epoch: Epoch,
    pub declarations: Declarations,
    pub locked_notes: LockedNotes,
    pub min_stake: MinStake,
}

impl ProvableOperation for SDPDeclareOp {
    type Proof = ZkAndEd25519Proof;
    const CODE: u8 = 0x20;
}

impl OperationGas<MainnetGasProfile> for SDPDeclareOp {
    const GAS_COST: Gas = Gas::new(646);
}

impl PreverifiableOperation<StandardMode>
    for SignedOperation<SDPDeclareOp, Unverified, StandardMode>
{
    type Context<'a> = SDPDeclarePreverificationContext<'a>;
    type Error = SdpError;

    fn preverify(&self, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        self.operation()
            .preverify(context.tx_hash_view, &self.proof().ed25519_sig)
    }
}

impl VerifiableOperation<StandardMode>
    for SignedOperation<SDPDeclareOp, Preverified, StandardMode>
{
    type Context<'a> = SDPDeclareVerificationContext<'a>;
    type Error = SdpError;

    fn verify(&self, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();

        // Check that the note exist
        let Some((utxo, _)) = context.utxo_tree.utxos().get(&operation.locked_note_id) else {
            return Err(SdpError::InexistingNote(operation.locked_note_id));
        };

        // Ensure locked note exists and ownership over the locked note and `zk_id`
        let note = utxo.note;
        if !ZkPublicKey::verify_multi(
            &[note.pk, operation.zk_id],
            context.tx_hash_view.as_fr(),
            &self.proof().zk_sig,
        ) {
            return Err(SdpError::InvalidZkSignature);
        }

        SDPDeclareValidationExt::validate(
            operation,
            note,
            context.channels,
            context.declarations,
            context.locked_notes,
            context.min_stake,
        )
    }
}

// TODO: Collapse into generic over Mode
impl PreverifiableOperation<GenesisMode>
    for SignedOperation<SDPDeclareOp, Unverified, GenesisMode>
{
    type Context<'a> = SDPDeclarePreverificationContext<'a>;
    type Error = SdpError;

    fn preverify(&self, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        self.operation()
            .preverify(context.tx_hash_view, &self.proof().ed25519_sig)
    }
}

impl VerifiableOperation<GenesisMode> for SignedOperation<SDPDeclareOp, Preverified, GenesisMode> {
    type Context<'a> = SDPDeclareGenesisValidationContext<'a>;
    type Error = SdpError;

    fn verify(&self, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();

        // Check that the note exist
        let Some((utxo, _)) = context.utxo_tree.utxos().get(&operation.locked_note_id) else {
            return Err(SdpError::InexistingNote(operation.locked_note_id));
        };
        let note = utxo.note;

        SDPDeclareValidationExt::validate(
            operation,
            note,
            context.channels,
            context.declarations,
            context.locked_notes,
            context.min_stake,
        )
    }
}

impl<Mode: VerificationMode> ExecutableOperation for SignedOperation<SDPDeclareOp, Verified, Mode> {
    type Context<'a> = SDPDeclareExecutionContext;
    type Error = SdpError;

    fn execute<'a>(
        &self,
        context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        SDPDeclareValidationExt::execute(self.operation(), context)
    }
}

impl<State: VerificationState, Mode: VerificationMode> SignedOperationExecutionGas
    for SignedOperation<SDPDeclareOp, State, Mode>
{
    fn gas_multiplier(&self) -> Value {
        1
    }
}

#[cfg(test)]
mod tests {
    use lb_cryptarchia_engine::Epoch;
    use lb_groth16::{AdditiveGroup as _, Fr};
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey, ZkSignature};
    use num_bigint::BigUint;

    use super::{
        Channels, GenesisMode, LockedNotes, MinStake, Note, PreverifiableOperation as _,
        Preverified, ProvableOperation, SDPDeclareGenesisValidationContext, SDPDeclareOp,
        SDPDeclarePreverificationContext, SDPDeclareVerificationContext, SdpError, SignedOperation,
        StandardMode, TxHashView, Unverified, Utxos, VerifiableOperation as _, ZkAndEd25519Proof,
        validate_service_scoped_uniqueness,
    };
    use crate::{
        mantle::{
            TxHash, Utxo,
            gas::{Gas, MainnetGasProfile, SignedOperationExecutionGas as _},
            ledger::Declarations,
            ops::{channel::ChannelId, op_proof::samples::SampleProof as _},
        },
        sdp::{Declaration, ServiceType},
    };

    fn locked_utxo(zk_key: &ZkKey) -> Utxo {
        Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: Note::new(10_000, zk_key.to_public_key()),
        }
    }

    fn declare_op(provider_sk: u8, zk_sk: u64, locator: &str) -> SDPDeclareOp {
        SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locators: vec![locator.parse().unwrap()].try_into().unwrap(),
            provider_id: Ed25519Key::from_bytes(&[provider_sk; 32])
                .public_key()
                .into(),
            zk_id: ZkKey::from(BigUint::from(zk_sk)).to_public_key(),
            locked_note_id: Fr::ZERO.into(),
        }
    }

    /// Two declarations in the same service sharing the same `provider_id`
    /// (different `zk_id` and locators) must be rejected by the SDP
    /// per-service uniqueness check.
    #[test]
    fn rejects_duplicate_provider_id_within_service() {
        let declare_a = declare_op(1, 1, "/ip4/1.1.1.1/udp/0");
        let declare_b = declare_op(1, 2, "/ip4/2.2.2.2/udp/0");

        let declarations = Declarations::new_sync()
            .insert(declare_a.id(), Declaration::new(Epoch::new(0), &declare_a));

        assert!(matches!(
            validate_service_scoped_uniqueness(&declare_b, &declarations),
            Err(SdpError::DuplicateProviderId { .. })
        ));
    }

    /// Two declarations in the same service sharing the same `zk_id`
    /// (different `provider_id` and locators) must be rejected by the SDP
    /// per-service uniqueness check.
    #[test]
    fn rejects_duplicate_zk_id_within_service() {
        let declare_a = declare_op(1, 1, "/ip4/1.1.1.1/udp/0");
        let declare_b = declare_op(2, 1, "/ip4/2.2.2.2/udp/0");

        let declarations = Declarations::new_sync()
            .insert(declare_a.id(), Declaration::new(Epoch::new(0), &declare_a));

        assert!(matches!(
            validate_service_scoped_uniqueness(&declare_b, &declarations),
            Err(SdpError::DuplicateZkId { .. })
        ));
    }

    mod standard_mode {
        use super::*;

        fn preverified(
            operation: SDPDeclareOp,
            zk_sig: ZkSignature,
            tx_hash_view: &TxHashView,
        ) -> SignedOperation<SDPDeclareOp, Preverified, StandardMode> {
            let ed25519_sig =
                Ed25519Key::from_bytes(&[1; 32]).sign_payload(tx_hash_view.as_bytes().as_ref());

            SignedOperation::<_, Unverified, StandardMode>::new(
                operation,
                ZkAndEd25519Proof::new(zk_sig, ed25519_sig),
            )
            .into_preverified(&SDPDeclarePreverificationContext { tx_hash_view })
            .expect("the provider key signed this transaction hash")
        }

        #[test]
        fn preverify_rejects_a_signature_over_another_transaction() {
            let signing_key = Ed25519Key::from_bytes(&[1; 32]);
            let signed_view = TxHashView::from(TxHash::from([11u8; 32]));
            let other_view = TxHashView::from(TxHash::from([12u8; 32]));

            let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
                declare_op(1, 1, "/ip4/1.1.1.1/udp/0"),
                ZkAndEd25519Proof::new(
                    ZkSignature::sample(),
                    signing_key.sign_payload(signed_view.as_bytes().as_ref()),
                ),
            );

            assert_eq!(
                signed_operation.preverify(&SDPDeclarePreverificationContext {
                    tx_hash_view: &signed_view
                }),
                Ok(())
            );
            assert_eq!(
                signed_operation.preverify(&SDPDeclarePreverificationContext {
                    tx_hash_view: &other_view
                }),
                Err(SdpError::InvalidEddsaSignature)
            );
        }

        #[test]
        fn verify_rejects_a_locked_note_that_is_not_in_the_ledger() {
            let operation = declare_op(1, 1, "/ip4/1.1.1.1/udp/0");
            let locked_note_id = operation.locked_note_id;

            let tx_hash_view = TxHashView::from(TxHash::from([11u8; 32]));
            let signed_operation = preverified(operation, ZkSignature::sample(), &tx_hash_view);

            assert_eq!(
                signed_operation.verify(&SDPDeclareVerificationContext {
                    utxo_tree: &Utxos::new(),
                    channels: &Channels::new(),
                    locked_notes: &LockedNotes::new(),
                    tx_hash_view: &tx_hash_view,
                    declarations: &Declarations::new_sync(),
                    min_stake: &MinStake {
                        threshold: 0,
                        timestamp: 0,
                    },
                }),
                Err(SdpError::InexistingNote(locked_note_id))
            );
        }

        #[test]
        fn verify_rejects_a_zk_signature_over_another_transaction() {
            let note_key = ZkKey::from(BigUint::from(3u64));
            let utxo = locked_utxo(&note_key);
            let (utxos, _) = Utxos::new().insert(utxo.id(), utxo);

            let operation = SDPDeclareOp {
                locked_note_id: utxo.id(),
                ..declare_op(1, 1, "/ip4/1.1.1.1/udp/0")
            };
            let declaration_key = ZkKey::from(BigUint::from(1u64));

            let tx_hash_view = TxHashView::from(TxHash::from([11u8; 32]));
            let other_view = TxHashView::from(TxHash::from([12u8; 32]));
            let zk_sig = ZkKey::multi_sign(&[note_key, declaration_key], other_view.as_fr())
                .expect("signing should succeed");

            let signed_operation = preverified(operation, zk_sig, &tx_hash_view);

            assert_eq!(
                signed_operation.verify(&SDPDeclareVerificationContext {
                    utxo_tree: &utxos,
                    channels: &Channels::new(),
                    locked_notes: &LockedNotes::new(),
                    tx_hash_view: &tx_hash_view,
                    declarations: &Declarations::new_sync(),
                    min_stake: &MinStake {
                        threshold: 0,
                        timestamp: 0,
                    },
                }),
                Err(SdpError::InvalidZkSignature)
            );
        }

        #[test]
        fn verify_rejects_a_zk_signature_missing_the_declaration_key() {
            let note_key = ZkKey::from(BigUint::from(3u64));
            let utxo = locked_utxo(&note_key);
            let (utxos, _) = Utxos::new().insert(utxo.id(), utxo);

            let operation = SDPDeclareOp {
                locked_note_id: utxo.id(),
                ..declare_op(1, 1, "/ip4/1.1.1.1/udp/0")
            };
            let _declaration_key = ZkKey::from(BigUint::from(1u64));

            let tx_hash_view = TxHashView::from(TxHash::from([11u8; 32]));
            let zk_sig = ZkKey::multi_sign(&[note_key], tx_hash_view.as_fr())
                .expect("signing should succeed");

            let signed_operation = preverified(operation, zk_sig, &tx_hash_view);

            assert_eq!(
                signed_operation.verify(&SDPDeclareVerificationContext {
                    utxo_tree: &utxos,
                    channels: &Channels::new(),
                    locked_notes: &LockedNotes::new(),
                    tx_hash_view: &tx_hash_view,
                    declarations: &Declarations::new_sync(),
                    min_stake: &MinStake {
                        threshold: 0,
                        timestamp: 0,
                    },
                }),
                Err(SdpError::InvalidZkSignature)
            );
        }

        #[test]
        fn verify_rejects_a_zk_signature_missing_the_locked_note_key() {
            let note_key = ZkKey::from(BigUint::from(3u64));
            let utxo = locked_utxo(&note_key);
            let (utxos, _) = Utxos::new().insert(utxo.id(), utxo);

            let operation = SDPDeclareOp {
                locked_note_id: utxo.id(),
                ..declare_op(1, 1, "/ip4/1.1.1.1/udp/0")
            };
            let declaration_key = ZkKey::from(BigUint::from(1u64));

            let tx_hash_view = TxHashView::from(TxHash::from([11u8; 32]));
            let zk_sig = ZkKey::multi_sign(&[declaration_key], tx_hash_view.as_fr())
                .expect("signing should succeed");

            let signed_operation = preverified(operation, zk_sig, &tx_hash_view);

            assert_eq!(
                signed_operation.verify(&SDPDeclareVerificationContext {
                    utxo_tree: &utxos,
                    channels: &Channels::new(),
                    locked_notes: &LockedNotes::new(),
                    tx_hash_view: &tx_hash_view,
                    declarations: &Declarations::new_sync(),
                    min_stake: &MinStake {
                        threshold: 0,
                        timestamp: 0,
                    },
                }),
                Err(SdpError::InvalidZkSignature)
            );
        }

        #[test]
        fn verify_rejects_a_declaration_that_is_already_registered() {
            let note_key = ZkKey::from(BigUint::from(3u64));
            let utxo = locked_utxo(&note_key);
            let (utxos, _) = Utxos::new().insert(utxo.id(), utxo);

            let operation = SDPDeclareOp {
                locked_note_id: utxo.id(),
                ..declare_op(1, 1, "/ip4/1.1.1.1/udp/0")
            };
            let declaration_id = operation.id();
            let declarations = Declarations::new_sync()
                .insert(declaration_id, Declaration::new(Epoch::new(0), &operation));
            let declaration_key = ZkKey::from(BigUint::from(1u64));

            let tx_hash_view = TxHashView::from(TxHash::from([11u8; 32]));
            let zk_sig = ZkKey::multi_sign(&[note_key, declaration_key], tx_hash_view.as_fr())
                .expect("signing should succeed");

            let signed_operation = preverified(operation, zk_sig, &tx_hash_view);

            assert_eq!(
                signed_operation.verify(&SDPDeclareVerificationContext {
                    utxo_tree: &utxos,
                    channels: &Channels::new(),
                    locked_notes: &LockedNotes::new(),
                    tx_hash_view: &tx_hash_view,
                    declarations: &declarations,
                    min_stake: &MinStake {
                        threshold: 0,
                        timestamp: 0,
                    },
                }),
                Err(SdpError::DuplicateDeclaration(declaration_id))
            );
        }

        #[test]
        fn verify_rejects_a_locked_note_owned_by_a_channel() {
            let note_key = ZkKey::from(BigUint::from(3u64));
            let utxo = locked_utxo(&note_key);
            let (utxos, _) = Utxos::new().insert(utxo.id(), utxo);

            let operation = SDPDeclareOp {
                locked_note_id: utxo.id(),
                ..declare_op(1, 1, "/ip4/1.1.1.1/udp/0")
            };
            let declaration_key = ZkKey::from(BigUint::from(1u64));

            let tx_hash_view = TxHashView::from(TxHash::from([11u8; 32]));
            let zk_sig = ZkKey::multi_sign(&[note_key, declaration_key], tx_hash_view.as_fr())
                .expect("signing should succeed");

            let channels = Channels::new()
                .register_channel_note(&utxo.id(), &ChannelId::from([21u8; 32]))
                .expect("the note is not owned by another channel");

            let signed_operation = preverified(operation, zk_sig, &tx_hash_view);

            assert_eq!(
                signed_operation.verify(&SDPDeclareVerificationContext {
                    utxo_tree: &utxos,
                    channels: &channels,
                    locked_notes: &LockedNotes::new(),
                    tx_hash_view: &tx_hash_view,
                    declarations: &Declarations::new_sync(),
                    min_stake: &MinStake {
                        threshold: 0,
                        timestamp: 0,
                    },
                }),
                Err(SdpError::ChannelNote(utxo.id()))
            );
        }

        #[test]
        fn verify_rejects_a_locked_note_below_the_minimum_stake() {
            let note_key = ZkKey::from(BigUint::from(3u64));
            let utxo = locked_utxo(&note_key);
            let (utxos, _) = Utxos::new().insert(utxo.id(), utxo);

            let operation = SDPDeclareOp {
                locked_note_id: utxo.id(),
                ..declare_op(1, 1, "/ip4/1.1.1.1/udp/0")
            };
            let declaration_key = ZkKey::from(BigUint::from(1u64));

            let tx_hash_view = TxHashView::from(TxHash::from([11u8; 32]));
            let zk_sig = ZkKey::multi_sign(&[note_key, declaration_key], tx_hash_view.as_fr())
                .expect("signing should succeed");

            let signed_operation = preverified(operation, zk_sig, &tx_hash_view);

            assert_eq!(
                signed_operation.verify(&SDPDeclareVerificationContext {
                    utxo_tree: &utxos,
                    channels: &Channels::new(),
                    locked_notes: &LockedNotes::new(),
                    tx_hash_view: &tx_hash_view,
                    declarations: &Declarations::new_sync(),
                    min_stake: &MinStake {
                        threshold: utxo.note.value + 1,
                        timestamp: 0,
                    },
                }),
                Err(SdpError::NoteInsufficientValue {
                    note_id: utxo.id(),
                    value: utxo.note.value,
                })
            );
        }

        #[test]
        fn verify_accepts_a_locked_note_exactly_meeting_the_minimum_stake() {
            let note_key = ZkKey::from(BigUint::from(3u64));
            let utxo = locked_utxo(&note_key);
            let (utxos, _) = Utxos::new().insert(utxo.id(), utxo);

            let operation = SDPDeclareOp {
                locked_note_id: utxo.id(),
                ..declare_op(1, 1, "/ip4/1.1.1.1/udp/0")
            };
            let declaration_key = ZkKey::from(BigUint::from(1u64));

            let tx_hash_view = TxHashView::from(TxHash::from([11u8; 32]));
            let zk_sig = ZkKey::multi_sign(&[note_key, declaration_key], tx_hash_view.as_fr())
                .expect("signing should succeed");

            let signed_operation = preverified(operation, zk_sig, &tx_hash_view);

            assert_eq!(
                signed_operation.verify(&SDPDeclareVerificationContext {
                    utxo_tree: &utxos,
                    channels: &Channels::new(),
                    locked_notes: &LockedNotes::new(),
                    tx_hash_view: &tx_hash_view,
                    declarations: &Declarations::new_sync(),
                    min_stake: &MinStake {
                        threshold: utxo.note.value,
                        timestamp: 0,
                    },
                }),
                Ok(())
            );
        }

        #[test]
        fn verify_rejects_a_locked_note_already_locked_for_the_service() {
            let note_key = ZkKey::from(BigUint::from(3u64));
            let utxo = locked_utxo(&note_key);
            let (utxos, _) = Utxos::new().insert(utxo.id(), utxo);

            let operation = SDPDeclareOp {
                locked_note_id: utxo.id(),
                ..declare_op(1, 1, "/ip4/1.1.1.1/udp/0")
            };
            let declaration_key = ZkKey::from(BigUint::from(1u64));

            let tx_hash_view = TxHashView::from(TxHash::from([11u8; 32]));
            let zk_sig = ZkKey::multi_sign(&[note_key, declaration_key], tx_hash_view.as_fr())
                .expect("signing should succeed");

            let min_stake = MinStake {
                threshold: 0,
                timestamp: 0,
            };
            let locked_notes = LockedNotes::new()
                .lock(&min_stake, ServiceType::BlendNetwork, utxo.note, &utxo.id())
                .expect("the note covers the minimum stake");

            let signed_operation = preverified(operation, zk_sig, &tx_hash_view);

            assert_eq!(
                signed_operation.verify(&SDPDeclareVerificationContext {
                    utxo_tree: &utxos,
                    channels: &Channels::new(),
                    locked_notes: &locked_notes,
                    tx_hash_view: &tx_hash_view,
                    declarations: &Declarations::new_sync(),
                    min_stake: &min_stake,
                }),
                Err(SdpError::NoteAlreadyUsedForService {
                    note_id: utxo.id(),
                    service_type: ServiceType::BlendNetwork,
                })
            );
        }

        #[test]
        fn verify_accepts_a_well_formed_declaration() {
            let note_key = ZkKey::from(BigUint::from(3u64));
            let utxo = locked_utxo(&note_key);
            let (utxos, _) = Utxos::new().insert(utxo.id(), utxo);

            let operation = SDPDeclareOp {
                locked_note_id: utxo.id(),
                ..declare_op(1, 1, "/ip4/1.1.1.1/udp/0")
            };
            let declaration_key = ZkKey::from(BigUint::from(1u64));

            let tx_hash_view = TxHashView::from(TxHash::from([11u8; 32]));
            let zk_sig = ZkKey::multi_sign(&[note_key, declaration_key], tx_hash_view.as_fr())
                .expect("signing should succeed");

            let signed_operation = preverified(operation, zk_sig, &tx_hash_view);

            assert_eq!(
                signed_operation.verify(&SDPDeclareVerificationContext {
                    utxo_tree: &utxos,
                    channels: &Channels::new(),
                    locked_notes: &LockedNotes::new(),
                    tx_hash_view: &tx_hash_view,
                    declarations: &Declarations::new_sync(),
                    min_stake: &MinStake {
                        threshold: 0,
                        timestamp: 0,
                    },
                }),
                Ok(())
            );
        }

        #[test]
        fn sdp_declare_op_execution_gas() {
            let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
                SDPDeclareOp::sample(),
                <SDPDeclareOp as ProvableOperation>::Proof::sample(),
            );

            assert_eq!(
                signed_operation.execution_gas::<MainnetGasProfile>(),
                Ok(Gas::new(646))
            );
        }
    }

    mod genesis_mode {
        use super::*;

        fn genesis_preverified(
            operation: SDPDeclareOp,
            tx_hash_view: &TxHashView,
        ) -> SignedOperation<SDPDeclareOp, Preverified, GenesisMode> {
            let ed25519_sig =
                Ed25519Key::from_bytes(&[1; 32]).sign_payload(tx_hash_view.as_bytes().as_ref());

            SignedOperation::<_, Unverified, GenesisMode>::new(
                operation,
                ZkAndEd25519Proof::new(ZkSignature::sample(), ed25519_sig),
            )
            .into_preverified(&SDPDeclarePreverificationContext { tx_hash_view })
            .expect("the provider key signed this transaction hash")
        }

        #[test]
        fn genesis_verify_rejects_a_locked_note_that_is_not_in_the_ledger() {
            let note_key = ZkKey::from(BigUint::from(3u64));
            let locked_note_id = locked_utxo(&note_key).id();

            let operation = SDPDeclareOp {
                locked_note_id,
                ..declare_op(1, 1, "/ip4/1.1.1.1/udp/0")
            };

            let tx_hash_view = TxHashView::from(TxHash::from([11u8; 32]));
            let signed_operation = genesis_preverified(operation, &tx_hash_view);

            assert_eq!(
                signed_operation.verify(&SDPDeclareGenesisValidationContext {
                    utxo_tree: &Utxos::new(),
                    channels: &Channels::new(),
                    locked_notes: &LockedNotes::new(),
                    declarations: &Declarations::new_sync(),
                    min_stake: &MinStake {
                        threshold: 0,
                        timestamp: 0,
                    },
                }),
                Err(SdpError::InexistingNote(locked_note_id))
            );
        }

        #[test]
        fn genesis_preverify_rejects_a_signature_over_another_transaction() {
            let signing_key = Ed25519Key::from_bytes(&[1; 32]);
            let signed_view = TxHashView::from(TxHash::from([11u8; 32]));
            let other_view = TxHashView::from(TxHash::from([12u8; 32]));

            let signed_operation = SignedOperation::<_, Unverified, GenesisMode>::new(
                declare_op(1, 1, "/ip4/1.1.1.1/udp/0"),
                ZkAndEd25519Proof::new(
                    ZkSignature::sample(),
                    signing_key.sign_payload(signed_view.as_bytes().as_ref()),
                ),
            );

            assert_eq!(
                signed_operation.preverify(&SDPDeclarePreverificationContext {
                    tx_hash_view: &signed_view
                }),
                Ok(())
            );
            assert_eq!(
                signed_operation.preverify(&SDPDeclarePreverificationContext {
                    tx_hash_view: &other_view
                }),
                Err(SdpError::InvalidEddsaSignature)
            );
        }

        #[test]
        fn genesis_verify_accepts_a_declaration_without_a_zk_signature() {
            let note_key = ZkKey::from(BigUint::from(3u64));
            let utxo = locked_utxo(&note_key);
            let (utxos, _) = Utxos::new().insert(utxo.id(), utxo);

            let operation = SDPDeclareOp {
                locked_note_id: utxo.id(),
                ..declare_op(1, 1, "/ip4/1.1.1.1/udp/0")
            };

            let tx_hash_view = TxHashView::from(TxHash::from([11u8; 32]));
            let signed_operation = genesis_preverified(operation, &tx_hash_view);

            assert_eq!(
                signed_operation.verify(&SDPDeclareGenesisValidationContext {
                    utxo_tree: &utxos,
                    channels: &Channels::new(),
                    locked_notes: &LockedNotes::new(),
                    declarations: &Declarations::new_sync(),
                    min_stake: &MinStake {
                        threshold: 0,
                        timestamp: 0,
                    },
                }),
                Ok(())
            );
        }
    }
}
