use lb_binary_codec::canonical::BinaryEncode as _;
use lb_cryptarchia_engine::Epoch;
use lb_groth16::Fr;
use lb_zktransfer::ZkTransferVerifierInput;

use super::{SDPDeclareOp, SdpError};
use crate::{
    block::BlockNumber,
    events::TxEvent,
    mantle::{
        batch::DeferredZkTransfer,
        gas::{
            EXECUTION_SIGNATURE_GAS, Gas, GasOverflow, MainnetGasProfile, NoteSetKey,
            OpGasCalculator, OpGasContext, OperationGas, spend_gas,
        },
        ledger::{
            DeclarationKeys, Declarations, ExecutableOperation, PreverifiableOperation,
            ProvableOperation, VerifiableOperation,
            verification_mode::{GenesisMode, StandardMode, VerificationMode},
        },
        note_set::{LEDGER_SET, NoteSets, SDP_SET},
        ops::{OpId, SignedOperation, ZkAndEd25519Proof},
        transactions::{
            hash::TxHashView,
            states::{Preverified, Unverified, Verified},
        },
    },
    sdp::{Declaration, MinStake},
};

trait SDPDeclareValidationExt {
    fn validate(
        &self,
        declarations: &Declarations,
        declaration_keys: &DeclarationKeys,
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
        declarations: &Declarations,
        declaration_keys: &DeclarationKeys,
        min_stake: &MinStake,
    ) -> Result<(), SdpError> {
        // Check that the declaration doesn't already exist
        if declarations.contains_key(&self.id()) {
            return Err(SdpError::DuplicateDeclaration(self.id()));
        }
        validate_service_scoped_uniqueness(self, declaration_keys)?;

        // Ensure the stake is sufficient for joining the service.
        if self.amount < min_stake.threshold {
            return Err(SdpError::AmountBelowMinStake {
                amount: self.amount,
                threshold: min_stake.threshold,
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

        // Spend the inputs from the ledger set and add the service note to the SDP set
        let ledger = context
            .note_sets
            .get(LEDGER_SET)
            .expect("the ledger note set always exists")
            .clone()
            .execute_spending(self.inputs.as_slice())?;
        let sdp = context
            .note_sets
            .get(SDP_SET)
            .expect("the SDP note set always exists")
            .clone()
            .execute_adding(&[self.service_note().commitment()])?;
        context.note_sets = context
            .note_sets
            .update(LEDGER_SET, ledger)
            .update(SDP_SET, sdp);

        Ok((context, Vec::new()))
    }
}

/// `provider_id` and `zk_id` must each be unique within the same service.
///
/// When both are taken the error is that of the first declaration in id order
/// holding either, its `provider_id` compared first.
fn validate_service_scoped_uniqueness(
    op: &SDPDeclareOp,
    declaration_keys: &DeclarationKeys,
) -> Result<(), SdpError> {
    let duplicate_provider_id = || SdpError::DuplicateProviderId {
        service_type: op.service_type,
        provider_id: Box::new(op.provider_id),
    };
    let duplicate_zk_id = || SdpError::DuplicateZkId {
        service_type: op.service_type,
        zk_id: op.zk_id,
    };
    match (
        declaration_keys.provider_id(op.service_type, op.provider_id),
        declaration_keys.zk_id(op.service_type, op.zk_id),
    ) {
        (Some(provider_holder), Some(zk_holder)) if zk_holder < provider_holder => {
            Err(duplicate_zk_id())
        }
        (Some(_), _) => Err(duplicate_provider_id()),
        (None, Some(_)) => Err(duplicate_zk_id()),
        (None, None) => Ok(()),
    }
}

pub struct SDPDeclarePreverificationContext<'a> {
    pub tx_hash_view: &'a TxHashView,
}

pub struct SDPDeclareVerificationContext<'a> {
    pub note_sets: &'a NoteSets,
    pub block_number: BlockNumber,
    pub tx_hash_view: &'a TxHashView,
    pub declarations: &'a Declarations,
    pub declaration_keys: &'a DeclarationKeys,
    pub min_stake: &'a MinStake,
}

pub struct SDPDeclareGenesisValidationContext<'a> {
    pub note_sets: &'a NoteSets,
    pub tx_hash_view: &'a TxHashView,
    pub declarations: &'a Declarations,
    pub declaration_keys: &'a DeclarationKeys,
    pub min_stake: &'a MinStake,
}

pub struct SDPDeclareExecutionContext {
    pub note_sets: NoteSets,
    pub epoch: Epoch,
    pub declarations: Declarations,
}

impl OpId for SDPDeclareOp {
    fn op_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }
}

impl ProvableOperation for SDPDeclareOp {
    type Proof = ZkAndEd25519Proof;
    const CODE: u8 = 0x20;
}

impl OperationGas<MainnetGasProfile> for SDPDeclareOp {
    const GAS_COST: Gas = Gas::new(669);
}

impl OpGasCalculator<MainnetGasProfile> for SDPDeclareOp {
    // The Operation, plus its signature and the spend of its notes into the
    // service note, over the commitments the transaction buffered in the ledger
    // note set before it.
    fn execution_gas(&self, context: &impl OpGasContext) -> Result<Gas, GasOverflow> {
        Self::GAS_COST
            .checked_add(EXECUTION_SIGNATURE_GAS)?
            .checked_add(spend_gas(
                self.inputs.len(),
                1,
                context.buffered(&NoteSetKey::Ledger),
            )?)
    }
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
    type DeferredProof = DeferredZkTransfer;
    type Context<'a> = SDPDeclareVerificationContext<'a>;
    type Error = SdpError;

    fn verify(&self, context: &Self::Context<'_>) -> Result<Self::DeferredProof, Self::Error> {
        let operation = self.operation();

        SDPDeclareValidationExt::validate(
            operation,
            context.declarations,
            context.declaration_keys,
            context.min_stake,
        )?;

        // Validate Inputs
        let ledger = context
            .note_sets
            .get(LEDGER_SET)
            .expect("the ledger note set always exists");
        ledger.assert_spendable(
            operation.inputs.as_slice(),
            &operation.commitment_merkle_root,
            context.block_number,
        )?;
        let tx_commitment_root = ledger.cached_tx_commitment_root(
            context.note_sets.tx_roots(),
            &operation.commitment_merkle_root,
            context.block_number,
        )?;

        // Defer the proof verification so that the caller can batch it.
        // Ed25519 verification is done by `preverify`.
        let nullifiers: Vec<Fr> = operation
            .inputs
            .iter()
            .map(|nullifier| nullifier.0)
            .collect();
        // The declaration creates no output in the ledger set: its excess is the stake.
        let public = ZkTransferVerifierInput::new(
            &nullifiers,
            &[],
            operation.amount,
            tx_commitment_root,
            *context.tx_hash_view.as_fr(),
        )?;
        Ok(DeferredZkTransfer::new(
            *self.proof().zk_proof.as_proof(),
            public,
        ))
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
    type DeferredProof = ();
    type Context<'a> = SDPDeclareGenesisValidationContext<'a>;
    type Error = SdpError;

    fn verify(&self, context: &Self::Context<'_>) -> Result<Self::DeferredProof, Self::Error> {
        let operation = self.operation();

        SDPDeclareValidationExt::validate(
            operation,
            context.declarations,
            context.declaration_keys,
            context.min_stake,
        )?;

        let ledger = context
            .note_sets
            .get(LEDGER_SET)
            .expect("the ledger note set always exists");
        ledger.assert_spendable(
            operation.inputs.as_slice(),
            &operation.commitment_merkle_root,
            0,
        )?;

        // Genesis Validation Exemptions 1: the ZkTransfer of a declaration is verified
        // as at any other time, against the empty root extended with the
        // outputs of the genesis Transfer.
        let tx_commitment_root = ledger.tx_commitment_root(&operation.commitment_merkle_root, 0)?;
        let nullifiers: Vec<Fr> = operation
            .inputs
            .iter()
            .map(|nullifier| nullifier.0)
            .collect();
        let public = ZkTransferVerifierInput::new(
            &nullifiers,
            &[],
            operation.amount,
            tx_commitment_root,
            *context.tx_hash_view.as_fr(),
        )?;
        if !lb_zktransfer::verify(self.proof().zk_proof.as_proof(), &public).unwrap_or(false) {
            return Err(SdpError::InvalidZkTransfer);
        }

        Ok(())
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

#[cfg(test)]
mod tests {
    use lb_cryptarchia_engine::Epoch;
    use lb_groth16::{AdditiveGroup as _, Field as _, Fr, Groth16Input};
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey, ZkPublicKey};
    use num_bigint::BigUint;

    use super::{
        GenesisMode, MinStake, PreverifiableOperation as _, Preverified, ProvableOperation,
        SDPDeclareExecutionContext, SDPDeclareGenesisValidationContext, SDPDeclareOp,
        SDPDeclarePreverificationContext, SDPDeclareVerificationContext, SdpError, SignedOperation,
        StandardMode, TxHashView, Unverified, VerifiableOperation as _, Verified,
        ZkAndEd25519Proof, validate_service_scoped_uniqueness,
    };
    use crate::{
        mantle::{
            TxHash,
            batch::DeferredZkTransfer,
            gas::{Gas, OpGasCalculator as _, test_utils::FixedThresholds},
            ledger::{DeclarationKeys, Declarations},
            note_set::{CommitmentMmr, LEDGER_SET, NoteSet, NoteSetError, NoteSets, SDP_SET},
            notes::{
                self, NoteCommitment, NoteNullifier, OutputCommitments, derive_note_nonce,
                derive_note_nullifier,
            },
            ops::{ZkTransferProof, op_proof::samples::SampleProof as _},
        },
        sdp::{Declaration, ServiceType, StakeThreshold},
    };

    fn nullifier(n: u64) -> NoteNullifier {
        NoteNullifier(Fr::from(n))
    }

    // The ledger set holds the commitment 1, recorded at the end of block 0.
    fn note_sets() -> NoteSets {
        let ledger = NoteSet::empty()
            .execute_adding(&[NoteCommitment(Fr::from(1u64))])
            .unwrap();
        NoteSets::new().update(LEDGER_SET, ledger).end_block(0)
    }

    fn root() -> Fr {
        note_sets().get(LEDGER_SET).unwrap().commitment_root()
    }

    fn declare_op(provider_sk: u8, zk_sk: u64, locator: &str) -> SDPDeclareOp {
        SDPDeclareOp {
            service_type: ServiceType::BlendNetwork,
            locators: vec![locator.parse().unwrap()].try_into().unwrap(),
            provider_id: Ed25519Key::from_bytes(&[provider_sk; 32])
                .public_key()
                .into(),
            zk_id: ZkKey::from(BigUint::from(zk_sk)).to_public_key(),
            inputs: [nullifier(2)].into(),
            commitment_merkle_root: root(),
            amount: 10_000,
        }
    }

    /// Two declarations in the same service sharing the same `provider_id`
    /// (different `zk_id` and locators) must be rejected by the SDP
    /// per-service uniqueness check.
    #[test]
    fn rejects_duplicate_provider_id_within_service() {
        let declare_a = declare_op(1, 1, "/ip4/1.1.1.1/udp/3000");
        let declare_b = declare_op(1, 2, "/ip4/2.2.2.2/udp/3000");

        let declarations = Declarations::new_sync()
            .insert(declare_a.id(), Declaration::new(Epoch::new(0), &declare_a));

        assert!(matches!(
            validate_service_scoped_uniqueness(
                &declare_b,
                &DeclarationKeys::from_declarations(&declarations)
            ),
            Err(SdpError::DuplicateProviderId { .. })
        ));
    }

    /// Two declarations in the same service sharing the same `zk_id`
    /// (different `provider_id` and locators) must be rejected by the SDP
    /// per-service uniqueness check.
    #[test]
    fn rejects_duplicate_zk_id_within_service() {
        let declare_a = declare_op(1, 1, "/ip4/1.1.1.1/udp/3000");
        let declare_b = declare_op(2, 1, "/ip4/2.2.2.2/udp/3000");

        let declarations = Declarations::new_sync()
            .insert(declare_a.id(), Declaration::new(Epoch::new(0), &declare_a));

        assert!(matches!(
            validate_service_scoped_uniqueness(
                &declare_b,
                &DeclarationKeys::from_declarations(&declarations)
            ),
            Err(SdpError::DuplicateZkId { .. })
        ));
    }

    // The check scanning every declaration of the service in id order.
    fn scanned_uniqueness(op: &SDPDeclareOp, declarations: &Declarations) -> Result<(), SdpError> {
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

    /// The lookups give the error of the first declaration in id order holding
    /// either id, as a scan of the service, and accept fresh ids.
    #[test]
    fn uniqueness_lookups_match_scanning_the_service() {
        let declared: Vec<SDPDeclareOp> = (1..=4u8)
            .map(|n| declare_op(n, u64::from(n), &format!("/ip4/{n}.{n}.{n}.{n}/udp/3000")))
            .collect();
        let declarations = declared.iter().fold(Declarations::new_sync(), |map, op| {
            map.insert(op.id(), Declaration::new(Epoch::new(0), op))
        });
        let keys = DeclarationKeys::from_declarations(&declarations);
        for provider_sk in 1..=6u8 {
            for zk_sk in 1..=6u64 {
                let op = declare_op(provider_sk, zk_sk, "/ip4/9.9.9.9/udp/3000");
                assert_eq!(
                    validate_service_scoped_uniqueness(&op, &keys),
                    scanned_uniqueness(&op, &declarations),
                    "provider {provider_sk}, zk {zk_sk}"
                );
            }
        }
        assert_eq!(
            validate_service_scoped_uniqueness(&declare_op(5, 5, "/ip4/9.9.9.9/udp/3000"), &keys),
            Ok(())
        );

        // A removed declaration frees its ids.
        let mut removed = keys;
        removed.remove_mut(&Declaration::new(Epoch::new(0), &declared[0]));
        assert_eq!(
            removed,
            DeclarationKeys::from_declarations(&declarations.remove(&declared[0].id()))
        );
        assert_eq!(
            validate_service_scoped_uniqueness(&declared[0], &removed),
            Ok(())
        );
    }

    mod standard_mode {
        use lb_zktransfer::ZkTransferInputsError;

        use super::*;

        fn tx_hash_view() -> TxHashView {
            TxHashView::from(TxHash::from([11u8; 32]))
        }

        fn verify_against(
            operation: SDPDeclareOp,
            zk_proof: ZkTransferProof,
            declarations: &Declarations,
            threshold: StakeThreshold,
            note_sets: &NoteSets,
        ) -> Result<DeferredZkTransfer, SdpError> {
            preverified(operation, zk_proof, &tx_hash_view()).verify(
                &SDPDeclareVerificationContext {
                    note_sets,
                    block_number: 1,
                    tx_hash_view: &tx_hash_view(),
                    declarations,
                    declaration_keys: &DeclarationKeys::from_declarations(declarations),
                    min_stake: &MinStake {
                        threshold,
                        timestamp: 0,
                    },
                },
            )
        }

        fn verify_declaration(operation: SDPDeclareOp) -> Result<DeferredZkTransfer, SdpError> {
            verify_against(
                operation,
                ZkTransferProof::sample(),
                &Declarations::new_sync(),
                0,
                &note_sets(),
            )
        }

        fn preverified(
            operation: SDPDeclareOp,
            zk_proof: ZkTransferProof,
            tx_hash_view: &TxHashView,
        ) -> SignedOperation<SDPDeclareOp, Preverified, StandardMode> {
            let ed25519_sig =
                Ed25519Key::from_bytes(&[1; 32]).sign_payload(tx_hash_view.as_bytes().as_ref());

            SignedOperation::<_, Unverified, StandardMode>::new(
                operation,
                ZkAndEd25519Proof::new(zk_proof, ed25519_sig),
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
                declare_op(1, 1, "/ip4/1.1.1.1/udp/3000"),
                ZkAndEd25519Proof::new(
                    ZkTransferProof::sample(),
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
        fn verify_rejects_a_declaration_that_is_already_registered() {
            let operation = declare_op(1, 1, "/ip4/1.1.1.1/udp/3000");
            let declaration_id = operation.id();
            let declarations = Declarations::new_sync()
                .insert(declaration_id, Declaration::new(Epoch::new(0), &operation));

            assert_eq!(
                verify_against(
                    operation,
                    ZkTransferProof::sample(),
                    &declarations,
                    0,
                    &note_sets()
                )
                .unwrap_err(),
                SdpError::DuplicateDeclaration(declaration_id)
            );
        }

        #[test]
        fn verify_accepts_a_well_formed_declaration() {
            verify_against(
                declare_op(1, 1, "/ip4/1.1.1.1/udp/3000"),
                ZkTransferProof::sample(),
                &Declarations::new_sync(),
                0,
                &note_sets(),
            )
            .expect("a well-formed declaration is accepted");
        }

        #[test]
        fn verify_defers_the_zk_transfer_public_inputs() {
            let deferred = verify_declaration(declare_op(1, 1, "/ip4/1.1.1.1/udp/3000")).unwrap();
            let public = deferred.inputs();
            assert_eq!(
                public.inputs.map(Groth16Input::into_inner),
                [Fr::from(2u64), Fr::ZERO, Fr::ZERO, Fr::ZERO]
            );
            assert_eq!(
                public.inputs_active.map(Groth16Input::into_inner),
                [Fr::ONE, Fr::ZERO, Fr::ZERO, Fr::ZERO]
            );
            assert_eq!(
                public.outputs_active.map(Groth16Input::into_inner),
                [Fr::ZERO; 8]
            );
            assert_eq!(public.excess_value.into_inner(), Fr::from(10_000u64));
            assert_eq!(public.tx_commitment_root.into_inner(), root());
            assert_eq!(public.msg.into_inner(), *tx_hash_view().as_fr());
        }

        #[test]
        fn verify_extends_the_referenced_root_with_the_buffer() {
            let sets = note_sets();
            let buffered = sets
                .get(LEDGER_SET)
                .unwrap()
                .clone()
                .buffer_outputs(&[NoteCommitment(Fr::from(4u64))]);
            let sets = sets.update(LEDGER_SET, buffered);
            let deferred = verify_against(
                declare_op(1, 1, "/ip4/1.1.1.1/udp/3000"),
                ZkTransferProof::sample(),
                &Declarations::new_sync(),
                0,
                &sets,
            )
            .unwrap();
            let expected = CommitmentMmr::new()
                .push(NoteCommitment(Fr::from(1u64)))
                .unwrap()
                .push(NoteCommitment(Fr::from(4u64)))
                .unwrap()
                .frontier_root();
            assert_eq!(deferred.inputs().tx_commitment_root.into_inner(), expected);
        }

        #[test]
        fn verify_rejects_an_unknown_root() {
            let operation = SDPDeclareOp {
                commitment_merkle_root: Fr::from(5u64),
                ..declare_op(1, 1, "/ip4/1.1.1.1/udp/3000")
            };
            assert_eq!(
                verify_declaration(operation).unwrap_err(),
                SdpError::NoteSet(NoteSetError::UnknownRoot(Fr::from(5u64)))
            );
        }

        #[test]
        fn verify_rejects_a_spent_input() {
            let sets = note_sets();
            let spent = sets
                .get(LEDGER_SET)
                .unwrap()
                .clone()
                .execute_spending(&[nullifier(2)])
                .unwrap();
            let sets = sets.update(LEDGER_SET, spent);
            assert_eq!(
                verify_against(
                    declare_op(1, 1, "/ip4/1.1.1.1/udp/3000"),
                    ZkTransferProof::sample(),
                    &Declarations::new_sync(),
                    0,
                    &sets
                )
                .unwrap_err(),
                SdpError::NoteSet(NoteSetError::AlreadySpent(nullifier(2)))
            );
        }

        #[test]
        fn verify_rejects_more_than_four_inputs() {
            let operation = SDPDeclareOp {
                inputs: [
                    nullifier(2),
                    nullifier(3),
                    nullifier(4),
                    nullifier(5),
                    nullifier(6),
                ]
                .into(),
                ..declare_op(1, 1, "/ip4/1.1.1.1/udp/3000")
            };
            assert_eq!(
                verify_declaration(operation).unwrap_err(),
                SdpError::ZkTransferInputs(ZkTransferInputsError::TooManyInputs(5))
            );
        }

        #[test]
        fn verify_rejects_an_amount_below_the_minimum_stake() {
            assert_eq!(
                verify_against(
                    declare_op(1, 1, "/ip4/1.1.1.1/udp/3000"),
                    ZkTransferProof::sample(),
                    &Declarations::new_sync(),
                    10_001,
                    &note_sets()
                )
                .unwrap_err(),
                SdpError::AmountBelowMinStake {
                    amount: 10_000,
                    threshold: 10_001
                }
            );
        }

        #[test]
        fn verify_accepts_an_amount_exactly_meeting_the_minimum_stake() {
            verify_against(
                declare_op(1, 1, "/ip4/1.1.1.1/udp/3000"),
                ZkTransferProof::sample(),
                &Declarations::new_sync(),
                10_000,
                &note_sets(),
            )
            .expect("an amount exactly meeting the minimum stake is accepted");
        }

        #[test]
        fn verify_builds_the_public_inputs_a_real_proof_satisfies() {
            let secret_key = Fr::from(1u64);
            let input = notes::Note::new(
                1_000,
                Fr::from(11u64),
                ZkKey::new(secret_key).to_public_key(),
            );
            let (mmr, path) = CommitmentMmr::new()
                .push_with_paths(input.commitment(), &mut [])
                .unwrap();
            let ledger = NoteSet::empty()
                .execute_adding(&[input.commitment()])
                .unwrap();
            let sets = NoteSets::new().update(LEDGER_SET, ledger).end_block(0);
            let root = mmr.frontier_root();
            let nullifier = derive_note_nullifier(&input.commitment(), &secret_key);
            let proof = crate::mantle::ops::transfer::test_utils::prove_transfer(
                &[(input, secret_key, path)],
                &[],
                root,
                *tx_hash_view().as_fr(),
            );
            let staking = |amount| SDPDeclareOp {
                inputs: [nullifier].into(),
                commitment_merkle_root: root,
                amount,
                ..declare_op(1, 1, "/ip4/1.1.1.1/udp/3000")
            };

            let deferred = verify_against(
                staking(1_000),
                ZkTransferProof::new(proof),
                &Declarations::new_sync(),
                0,
                &sets,
            )
            .unwrap();
            assert!(lb_zktransfer::verify(deferred.proof(), deferred.inputs()).unwrap());

            let deferred = verify_against(
                staking(999),
                ZkTransferProof::new(proof),
                &Declarations::new_sync(),
                0,
                &sets,
            )
            .unwrap();
            assert!(!lb_zktransfer::verify(deferred.proof(), deferred.inputs()).unwrap());
        }

        fn verified(
            operation: SDPDeclareOp,
        ) -> SignedOperation<SDPDeclareOp, Verified, StandardMode> {
            SignedOperation::<_, Unverified, StandardMode>::new(
                operation,
                <SDPDeclareOp as ProvableOperation>::Proof::sample(),
            )
            .into_state_trusted()
        }

        #[test]
        fn execute_spends_the_inputs_and_adds_the_service_note() {
            let operation = SDPDeclareOp::sample();
            let op_id: [u8; 32] =
                hex::decode("eee94894496fe11261d5f649f414c586c852436ba4c6c3fdb60294894a3dc19c")
                    .unwrap()
                    .try_into()
                    .unwrap();
            let zk_id = ZkPublicKey::from(Fr::from(25u64));
            let expected = notes::Note::new(45, derive_note_nonce(&op_id, 0, 45, &zk_id), zk_id);
            assert_eq!(operation.service_note(), expected);

            let (context, events) = verified(operation.clone())
                .execute(SDPDeclareExecutionContext {
                    note_sets: NoteSets::new(),
                    epoch: Epoch::new(4),
                    declarations: Declarations::new_sync(),
                })
                .expect("the input is unspent");

            let empty_root = NoteSet::empty().commitment_root();
            let ledger = context.note_sets.get(LEDGER_SET).unwrap();
            assert!(ledger.contains_nullifier(&nullifier(26)));
            assert_eq!(ledger.commitment_root(), empty_root);
            assert_eq!(ledger.tx_commitment_root(&empty_root, 0), Ok(empty_root)); // nothing buffered
            assert_eq!(
                context.note_sets.get(SDP_SET).unwrap().commitment_root(),
                CommitmentMmr::new()
                    .push(expected.commitment())
                    .unwrap()
                    .frontier_root()
            );
            let declaration = context.declarations.get(&operation.id()).unwrap();
            assert_eq!(declaration.service_note, expected.commitment());
            assert_eq!(declaration.withdraw_outputs, OutputCommitments::default());
            assert_eq!(declaration.created, Epoch::new(4));
            assert_eq!(events, []);
        }

        #[test]
        fn sdp_declare_op_execution_gas_does_not_scale_with_the_threshold() {
            // 1 input, the service note, nothing buffered:
            // 669 + 167 + 1946 + 838 + 25
            for threshold in [0, 1, 3] {
                assert_eq!(
                    SDPDeclareOp::sample().execution_gas(&FixedThresholds(threshold)),
                    Ok(Gas::new(3_645))
                );
            }
        }
    }

    mod genesis_mode {
        use lb_key_management_system_keys::keys::Ed25519Signature;

        use super::*;
        use crate::mantle::ops::transfer::prove_transfer;

        fn tx_hash_view() -> TxHashView {
            TxHashView::from(TxHash::from([9u8; 32]))
        }

        fn genesis_preverified(
            operation: SDPDeclareOp,
            tx_hash_view: &TxHashView,
        ) -> SignedOperation<SDPDeclareOp, Preverified, GenesisMode> {
            let ed25519_sig =
                Ed25519Key::from_bytes(&[1; 32]).sign_payload(tx_hash_view.as_bytes().as_ref());

            SignedOperation::<_, Unverified, GenesisMode>::new(
                operation,
                ZkAndEd25519Proof::new(ZkTransferProof::sample(), ed25519_sig),
            )
            .into_preverified(&SDPDeclarePreverificationContext { tx_hash_view })
            .expect("the provider key signed this transaction hash")
        }

        #[test]
        fn genesis_verify_rejects_an_unknown_root() {
            let operation = SDPDeclareOp {
                commitment_merkle_root: Fr::from(5u64),
                ..declare_op(1, 1, "/ip4/1.1.1.1/udp/3000")
            };

            let tx_hash_view = TxHashView::from(TxHash::from([11u8; 32]));
            let signed_operation = genesis_preverified(operation, &tx_hash_view);

            assert_eq!(
                signed_operation
                    .verify(&SDPDeclareGenesisValidationContext {
                        note_sets: &note_sets(),
                        tx_hash_view: &tx_hash_view,
                        declarations: &Declarations::new_sync(),
                        declaration_keys: &DeclarationKeys::default(),
                        min_stake: &MinStake {
                            threshold: 0,
                            timestamp: 0,
                        },
                    })
                    .unwrap_err(),
                SdpError::NoteSet(NoteSetError::UnknownRoot(Fr::from(5u64)))
            );
        }

        #[test]
        fn genesis_preverify_rejects_a_signature_over_another_transaction() {
            let signing_key = Ed25519Key::from_bytes(&[1; 32]);
            let signed_view = TxHashView::from(TxHash::from([11u8; 32]));
            let other_view = TxHashView::from(TxHash::from([12u8; 32]));

            let signed_operation = SignedOperation::<_, Unverified, GenesisMode>::new(
                declare_op(1, 1, "/ip4/1.1.1.1/udp/3000"),
                ZkAndEd25519Proof::new(
                    ZkTransferProof::sample(),
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
        fn genesis_verify_rejects_a_placeholder_proof() {
            let operation = declare_op(1, 1, "/ip4/1.1.1.1/udp/3000");

            let tx_hash_view = TxHashView::from(TxHash::from([11u8; 32]));
            let signed_operation = genesis_preverified(operation, &tx_hash_view);

            assert_eq!(
                signed_operation.verify(&SDPDeclareGenesisValidationContext {
                    note_sets: &note_sets(),
                    tx_hash_view: &tx_hash_view,
                    declarations: &Declarations::new_sync(),
                    declaration_keys: &DeclarationKeys::default(),
                    min_stake: &MinStake {
                        threshold: 0,
                        timestamp: 0,
                    },
                }),
                Err(SdpError::InvalidZkTransfer)
            );
        }

        #[test]
        fn genesis_verify_checks_the_zk_transfer_against_the_empty_root_extended_with_the_buffer() {
            let secret_key = Fr::from(1u64);
            // Outputs 0 and 1 of the genesis Transfer.
            let staked = notes::Note::new(1_000, Fr::ZERO, ZkKey::new(secret_key).to_public_key());
            let other = notes::Note::new(500, Fr::ONE, ZkPublicKey::new(Fr::from(9u64)));
            let outputs = [staked.commitment(), other.commitment()];
            let (mmr, staked_path) = CommitmentMmr::new()
                .push_with_paths(outputs[0], &mut [])
                .unwrap();
            let mut paths = vec![staked_path];
            let (mmr, _) = mmr.push_with_paths(outputs[1], &mut paths).unwrap();

            let ledger = NoteSet::empty()
                .execute_adding(&outputs)
                .unwrap()
                .buffer_outputs(&outputs);
            let buffered = NoteSets::new().update(LEDGER_SET, ledger);
            let unbuffered = NoteSets::new().update(
                LEDGER_SET,
                NoteSet::empty().execute_adding(&outputs).unwrap(),
            );

            let nullifier = derive_note_nullifier(&staked.commitment(), &secret_key);
            let proof = prove_transfer(
                &[(staked, secret_key, paths[0].clone())],
                &[],
                mmr.frontier_root(),
                *tx_hash_view().as_fr(),
            );
            let declaration = SDPDeclareOp {
                inputs: [nullifier].into(),
                commitment_merkle_root: CommitmentMmr::new().frontier_root(),
                amount: 1_000,
                ..SDPDeclareOp::sample()
            };
            let verify = |operation: SDPDeclareOp, note_sets: &NoteSets| {
                SignedOperation::<_, Unverified, GenesisMode>::new(
                    operation,
                    ZkAndEd25519Proof {
                        zk_proof: ZkTransferProof::new(proof),
                        ed25519_sig: Ed25519Signature::zero(),
                    },
                )
                .into_state_trusted::<Preverified>()
                .verify(&SDPDeclareGenesisValidationContext {
                    note_sets,
                    tx_hash_view: &tx_hash_view(),
                    declarations: &Declarations::new_sync(),
                    declaration_keys: &DeclarationKeys::default(),
                    min_stake: &MinStake {
                        threshold: 0,
                        timestamp: 0,
                    },
                })
            };

            assert_eq!(verify(declaration.clone(), &buffered), Ok(()));
            assert_eq!(
                verify(declaration.clone(), &unbuffered),
                Err(SdpError::InvalidZkTransfer)
            );
            assert_eq!(
                verify(
                    SDPDeclareOp {
                        amount: 999,
                        ..declaration
                    },
                    &buffered
                ),
                Err(SdpError::InvalidZkTransfer)
            );
        }
    }
}
