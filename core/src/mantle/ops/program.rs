use lb_binary_codec::canonical::{BinaryCodec, BinaryEncode as _};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    events::TxEvent,
    mantle::{
        gas::{Gas, MainnetGasProfile, OpGasCalculator, OperationGas},
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, VerifiableOperation,
            verification_mode::StandardMode,
        },
        ops::{NoOpProof, OpId, SignedOperation},
        programs::{ProgramKeyError, ProgramVerificationKey, Programs, RegisteredProgram},
        transactions::states::{Preverified, Unverified, Verified},
    },
};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct ProgramRegisterOp {
    pub verification_key: ProgramVerificationKey,
}

impl ProgramRegisterOp {
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample() -> Self {
        Self {
            verification_key: crate::mantle::fixtures::programs::sample_key(),
        }
    }
}

impl OpId for ProgramRegisterOp {
    fn op_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }
}

impl ProvableOperation for ProgramRegisterOp {
    type Proof = NoOpProof;
    const CODE: u8 = 0x01;
}

impl OperationGas<MainnetGasProfile> for ProgramRegisterOp {
    const GAS_COST: Gas = Gas::new(15114);
}

impl OpGasCalculator<MainnetGasProfile> for ProgramRegisterOp {}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ProgramRegisterError {
    #[error("Invalid program verification key: {0}")]
    InvalidKey(#[from] ProgramKeyError),
}

pub struct ProgramRegisterExecutionContext {
    pub programs: Programs,
}

impl PreverifiableOperation<StandardMode>
    for SignedOperation<ProgramRegisterOp, Unverified, StandardMode>
{
    type Context<'a> = ();
    type Error = ProgramRegisterError;

    fn preverify(&self, _context: &Self::Context<'_>) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl VerifiableOperation<StandardMode>
    for SignedOperation<ProgramRegisterOp, Preverified, StandardMode>
{
    type DeferredProof = ();
    type Context<'a> = ();
    type Error = ProgramRegisterError;

    fn verify(&self, _context: &Self::Context<'_>) -> Result<Self::DeferredProof, Self::Error> {
        // Every point on its curve, in its prime order subgroup and not the point
        // at infinity, since an invalid point would make the proofs of the program
        // unsound. The key keeps its decompression for the execution.
        self.operation()
            .verification_key
            .groth16()
            .map(drop)
            .map_err(Into::into)
    }
}

impl ExecutableOperation for SignedOperation<ProgramRegisterOp, Verified, StandardMode> {
    type Context<'a> = ProgramRegisterExecutionContext;
    type Error = ProgramRegisterError;

    fn execute<'a>(
        &self,
        context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        // Storing an already registered key has no effect, and e(α, β) is computed
        // once here, paid by the registration. The points are the ones the
        // verification decompressed and checked, kept by the key.
        let program = RegisteredProgram::new(self.operation().verification_key.clone())?;
        let (programs, _) = context.programs.register(program);
        Ok((ProgramRegisterExecutionContext { programs }, Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use lb_binary_codec::canonical::BinaryDecode as _;
    use lb_groth16::{
        Groth16VerificationKey,
        test_utils::{off_subgroup_g2, three_input_setup},
    };

    use super::*;
    use crate::mantle::{
        VerificationError, gas::test_utils::FixedThresholds, programs::derive_program_id,
    };

    fn valid_key() -> ProgramVerificationKey {
        ProgramVerificationKey::try_from(&three_input_setup(1).0).unwrap()
    }

    fn verify(verification_key: &ProgramVerificationKey) -> Result<(), ProgramRegisterError> {
        SignedOperation::<_, Unverified, StandardMode>::new(
            ProgramRegisterOp {
                verification_key: verification_key.clone(),
            },
            NoOpProof,
        )
        .into_preverified(&())
        .expect("preverify does not read the key")
        .verify(&())
    }

    #[test]
    fn verify_checks_every_point_of_the_key() {
        let invalid = ProgramRegisterError::InvalidKey(ProgramKeyError::InvalidPoint);
        let key = three_input_setup(1).0;
        let program_key =
            |key: &Groth16VerificationKey| ProgramVerificationKey::try_from(key).unwrap();
        let bytes = program_key(&key).to_bytes();

        let off_subgroup = program_key(&Groth16VerificationKey {
            gamma_2: off_subgroup_g2(),
            ..key.clone()
        });
        // No flag and a coordinate below q, yet no point has x = 0: 3 is not a
        // square modulo q.
        let mut off_curve = bytes;
        off_curve[..32].fill(0);
        for decodable in [off_subgroup.to_bytes(), off_curve] {
            let decoded = ProgramVerificationKey::decode_all(&decodable, &()).unwrap();
            assert_eq!(verify(&decoded), Err(invalid.clone()));
        }
        assert_eq!(
            verify(&program_key(&Groth16VerificationKey {
                // The point at infinity.
                delta_2: <_>::default(),
                ..key.clone()
            })),
            Err(invalid.clone())
        );
        assert_eq!(verify(&program_key(&key)), Ok(()));
        assert!(
            !VerificationError::from(invalid).is_operation_proof_failure(),
            "an invalid key condemns the block and not one copy of it"
        );

        let mut flagged = bytes;
        flagged[31] |= 0x40;
        assert!(ProgramVerificationKey::decode_all(&flagged, &()).is_err());
        assert!(
            serde_json::from_str::<ProgramVerificationKey>(&format!(
                "\"{}\"",
                hex::encode(flagged)
            ))
            .is_err()
        );
    }

    #[test]
    fn execute_registers_the_key_under_its_id_once() {
        let key = valid_key();
        let signed: SignedOperation<_, Verified, StandardMode> = SignedOperation::new(
            ProgramRegisterOp {
                verification_key: key.clone(),
            },
            NoOpProof,
        )
        .into_state_trusted();
        let (context, events) = signed
            .clone()
            .execute(ProgramRegisterExecutionContext {
                programs: Programs::new(),
            })
            .expect("a checked key registers");
        assert_eq!(events, []);
        assert_eq!(
            context
                .programs
                .get(&derive_program_id(&key))
                .map(RegisteredProgram::key),
            Some(&key)
        );
        let (again, _) = signed
            .execute(ProgramRegisterExecutionContext {
                programs: context.programs.clone(),
            })
            .expect("registering again has no effect");
        assert_eq!(again.programs, context.programs);
    }

    #[test]
    fn execute_reuses_the_key_decompressed_by_verify() {
        let key = valid_key();
        let verified = SignedOperation::<_, Unverified, StandardMode>::new(
            ProgramRegisterOp {
                verification_key: key.clone(),
            },
            NoOpProof,
        )
        .into_preverified(&())
        .expect("preverify does not read the key")
        .into_verified(&())
        .map_err(|(_, error)| error)
        .expect("a valid key verifies");
        assert!(!key.keeps_groth16());
        assert!(
            verified.operation().verification_key.keeps_groth16(),
            "the execution finds the points verify decompressed"
        );
        let (context, _) = verified
            .execute(ProgramRegisterExecutionContext {
                programs: Programs::new(),
            })
            .map_err(|(_, error)| error)
            .expect("a checked key registers");
        assert_eq!(
            context.programs.get(&derive_program_id(&key)),
            Some(&RegisteredProgram::new(key).unwrap())
        );
    }

    #[test]
    fn program_register_op_execution_gas_is_15114() {
        assert_eq!(
            ProgramRegisterOp::sample().execution_gas(&FixedThresholds(3)),
            Ok(Gas::new(15_114))
        );
    }
}
