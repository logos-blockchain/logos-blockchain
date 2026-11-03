use std::sync::LazyLock;

use ark_ff::{AdditiveGroup as _, PrimeField as _};
use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode, DecodeError};
use lb_groth16::{
    CompressedGroth16Proof, CompressedGroth16VerificationKey, Fr, Groth16AlphaBeta, Groth16Proof,
    Groth16VerificationKey, fr_from_bytes,
};
use lb_key_management_system_keys::keys::{ZkKey, ZkPublicKey};
use lb_poseidon2::Digest as _;
use lb_program_spend::{MAX_INPUTS, MAX_OUTPUTS};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

use crate::{
    crypto::{Digest as _, Hasher, ZkHasher},
    mantle::{
        ledger::Value,
        notes::{Note, ProgramId},
    },
};

pub const PROGRAM_PUBLIC_INPUTS: usize = 3;
pub const PROGRAM_KEY_IC_LEN: usize = PROGRAM_PUBLIC_INPUTS + 1;
pub const PROGRAM_KEY_SIZE: usize = 352;

static PROGRAM_ID_V1: LazyLock<Vec<u8>> = LazyLock::new(|| b"PROGRAM_ID_V1".to_vec());

static PROGRAM_SPEND_NOTES_V1: LazyLock<Fr> = LazyLock::new(|| {
    fr_from_bytes(b"PROGRAM_SPEND_NOTES_V1").expect("BigUint should load from constant string")
});

// Alpha G1, Beta Gamma Delta G2, 4 IC G1, compressed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProgramVerificationKey(CompressedGroth16VerificationKey);

impl ProgramVerificationKey {
    pub fn new(key: CompressedGroth16VerificationKey) -> Result<Self, ProgramKeyError> {
        if key.ic.len() != PROGRAM_KEY_IC_LEN {
            return Err(ProgramKeyError::IcCount(key.ic.len()));
        }
        Ok(Self(key))
    }

    #[must_use]
    pub const fn as_compressed(&self) -> &CompressedGroth16VerificationKey {
        &self.0
    }

    pub fn to_groth16(&self) -> Result<Groth16VerificationKey, ProgramKeyError> {
        Groth16VerificationKey::try_from(&self.0).map_err(|_| ProgramKeyError::InvalidPoint)
    }

    #[must_use]
    pub fn to_bytes(&self) -> [u8; PROGRAM_KEY_SIZE] {
        self.0
            .to_bytes()
            .try_into()
            .expect("a program key encodes to its size")
    }

    #[must_use]
    pub fn from_bytes(bytes: &[u8; PROGRAM_KEY_SIZE]) -> Self {
        Self(
            CompressedGroth16VerificationKey::from_bytes(bytes)
                .expect("a program key size holds 4 ic points"),
        )
    }
}

impl BinaryEncode for ProgramVerificationKey {
    fn encoded_length(&self) -> usize {
        PROGRAM_KEY_SIZE
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_bytes());
    }
}

impl BinaryDecode for ProgramVerificationKey {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, bytes) = <[u8; PROGRAM_KEY_SIZE]>::decode(input, &())?;
        Ok((rest, Self::from_bytes(&bytes)))
    }
}

impl Serialize for ProgramVerificationKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        lb_utils::serde::serialize_bytes_array::<PROGRAM_KEY_SIZE, _>(self.to_bytes(), serializer)
    }
}

impl<'de> Deserialize<'de> for ProgramVerificationKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes =
            lb_utils::serde::deserialize_bytes_array_or_seq::<PROGRAM_KEY_SIZE, _>(deserializer)?;
        Ok(Self::from_bytes(&bytes))
    }
}

impl TryFrom<&Groth16VerificationKey> for ProgramVerificationKey {
    type Error = ProgramKeyError;

    fn try_from(key: &Groth16VerificationKey) -> Result<Self, Self::Error> {
        Self::new(
            CompressedGroth16VerificationKey::try_from(key)
                .map_err(|_| ProgramKeyError::InvalidPoint)?,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProgramKeyError {
    #[error("a program key holds 4 ic points: got {0}")]
    IcCount(usize),
    #[error("a program key point is not in its prime order subgroup or is the point at infinity")]
    InvalidPoint,
}

#[must_use]
pub fn derive_program_id(key: &ProgramVerificationKey) -> ProgramId {
    let mut preimage = PROGRAM_ID_V1.clone();
    key.encode_into(&mut preimage);
    // A classic hash, reduced mod `p` as `op_id` is, since `ProgramId` is a field
    // of the note commitment.
    Fr::from_le_bytes_mod_order(&Hasher::digest(&preimage))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredProgram {
    key: ProgramVerificationKey,
    groth16_key: Groth16VerificationKey,
    alpha_beta: Groth16AlphaBeta,
}

impl RegisteredProgram {
    pub fn new(key: ProgramVerificationKey) -> Result<Self, ProgramKeyError> {
        let groth16_key = key.to_groth16()?;
        let alpha_beta = groth16_key.alpha_beta();
        Ok(Self {
            key,
            groth16_key,
            alpha_beta,
        })
    }

    #[must_use]
    pub const fn key(&self) -> &ProgramVerificationKey {
        &self.key
    }

    #[must_use]
    pub const fn alpha_beta(&self) -> &Groth16AlphaBeta {
        &self.alpha_beta
    }

    pub fn verify(
        &self,
        proof: &CompressedGroth16Proof,
        public: &[Fr; PROGRAM_PUBLIC_INPUTS],
    ) -> Result<bool, ProgramProofError> {
        let proof = Groth16Proof::try_from(proof).map_err(|_| ProgramProofError::Expansion)?;
        let prepared = self.groth16_key.prepare_with_alpha_beta(self.alpha_beta);
        lb_groth16::groth16_verify(&prepared, &proof, public)
            .map_err(|e| ProgramProofError::Verification(e.to_string()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProgramProofError {
    #[error("the program proof does not decompress")]
    Expansion,
    #[error("{0}")]
    Verification(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Programs(rpds::RedBlackTreeMapSync<ProgramId, RegisteredProgram>);

impl Programs {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn get(&self, program_id: &ProgramId) -> Option<&RegisteredProgram> {
        self.0.get(program_id)
    }

    #[must_use]
    pub fn register(self, program: RegisteredProgram) -> (Self, ProgramId) {
        let program_id = derive_program_id(program.key());
        // Registering a key already registered has no effect.
        if self.get(&program_id).is_some() {
            return (self, program_id);
        }
        (Self(self.0.insert(program_id, program)), program_id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.size()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for Programs {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0
            .values()
            .map(RegisteredProgram::key)
            .collect::<Vec<_>>()
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Programs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<ProgramVerificationKey>::deserialize(deserializer)?
            .into_iter()
            .try_fold(Self::new(), |programs, key| {
                Ok(programs
                    .register(RegisteredProgram::new(key).map_err(D::Error::custom)?)
                    .0)
            })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SpendInputFields {
    pub active: bool,
    pub value: Value,
    pub nonce: Fr,
    pub public_key: ZkPublicKey,
    pub data: Fr,
}

#[derive(Clone, Copy, Debug)]
pub struct SpendOutputFields {
    pub active: bool,
    pub value: Value,
    pub nonce: Fr,
    pub public_key: ZkPublicKey,
    pub program: ProgramId,
    pub data: Fr,
}

impl SpendInputFields {
    // The circuit derives the public key of every input.
    #[must_use]
    pub fn inactive() -> Self {
        Self {
            active: false,
            value: 0,
            nonce: Fr::ZERO,
            public_key: ZkKey::new(Fr::ZERO).to_public_key(),
            data: Fr::ZERO,
        }
    }
}

impl SpendOutputFields {
    #[must_use]
    pub const fn inactive() -> Self {
        Self {
            active: false,
            value: 0,
            nonce: Fr::ZERO,
            public_key: ZkPublicKey::zero(),
            program: Fr::ZERO,
            data: Fr::ZERO,
        }
    }

    #[must_use]
    pub const fn of(note: &Note) -> Self {
        Self {
            active: true,
            value: note.value,
            nonce: note.nonce,
            public_key: note.public_key,
            program: note.program,
            data: note.data,
        }
    }
}

#[must_use]
pub fn spend_notes_hash(
    inputs: &[SpendInputFields; MAX_INPUTS],
    outputs: &[SpendOutputFields; MAX_OUTPUTS],
) -> Fr {
    // The fields the prover used, the public key of an inactive input included.
    let inputs = inputs.iter().flat_map(|input| {
        [
            Fr::from(input.active),
            Fr::from(input.value),
            input.nonce,
            *input.public_key.as_fr(),
            input.data,
        ]
    });
    let outputs = outputs.iter().flat_map(|output| {
        [
            Fr::from(output.active),
            Fr::from(output.value),
            output.nonce,
            *output.public_key.as_fr(),
            output.program,
            output.data,
        ]
    });
    ZkHasher::digest(
        &core::iter::once(*PROGRAM_SPEND_NOTES_V1)
            .chain(inputs)
            .chain(outputs)
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use lb_groth16::test_utils::{three_input_prove, three_input_setup};

    use super::*;
    use crate::mantle::{fixtures::programs::placeholder_key, notes::derive_note_nullifier};

    fn test_key() -> (
        ProgramVerificationKey,
        ark_groth16::ProvingKey<lb_groth16::Bn254>,
    ) {
        let (key, proving_key) = three_input_setup(1);
        (ProgramVerificationKey::try_from(&key).unwrap(), proving_key)
    }

    #[test]
    fn program_key_encodes_to_352_bytes_and_derives_its_id_from_them() {
        let key = placeholder_key();
        let bytes = key.encode();
        assert_eq!(bytes.len(), PROGRAM_KEY_SIZE);
        assert_eq!(&bytes[31..33], &[0x3e, 0x3f]);
        assert_eq!(&bytes[351..], &[0x45]);
        let mut preimage = b"PROGRAM_ID_V1".to_vec();
        preimage.extend(&bytes);
        let digest: [u8; 32] = Hasher::digest(&preimage).into();
        assert_eq!(
            derive_program_id(&key),
            Fr::from_le_bytes_mod_order(&digest)
        );
        assert_eq!(
            hex::encode(lb_groth16::fr_to_bytes(&derive_program_id(&key))),
            "bbab28f22350a8f00af6626fffce83a95dd1df91744b085eaf86ccaae0214c25"
        );
    }

    #[test]
    fn registered_program_checks_every_point_and_stores_alpha_beta() {
        let (key, _) = test_key();
        let program = RegisteredProgram::new(key.clone()).unwrap();
        assert_eq!(
            program.alpha_beta(),
            &key.to_groth16().unwrap().alpha_beta()
        );
        assert_eq!(
            RegisteredProgram::new(placeholder_key()),
            Err(ProgramKeyError::InvalidPoint)
        );
        let mut at_infinity = three_input_setup(1).0;
        // An affine point defaults to the point at infinity.
        at_infinity.ic[3] = <_>::default();
        assert_eq!(
            RegisteredProgram::new(ProgramVerificationKey::try_from(&at_infinity).unwrap()),
            Err(ProgramKeyError::InvalidPoint)
        );
    }

    #[test]
    fn program_proof_verifies_on_its_three_public_inputs_only() {
        let (key, proving_key) = test_key();
        let program = RegisteredProgram::new(key).unwrap();
        let public = [Fr::from(11u64), Fr::from(12u64), Fr::from(13u64)];
        let proof =
            CompressedGroth16Proof::try_from(&three_input_prove(&proving_key, public, 3)).unwrap();
        assert_eq!(program.verify(&proof, &public), Ok(true));
        assert_eq!(
            program.verify(&proof, &[Fr::from(11u64), Fr::from(12u64), Fr::from(14u64)]),
            Ok(false)
        );
    }

    #[test]
    fn registering_twice_has_no_effect_and_the_registry_round_trips() {
        let (key, _) = test_key();
        let (programs, id) = Programs::new().register(RegisteredProgram::new(key.clone()).unwrap());
        assert_eq!(id, derive_program_id(&key));
        let (again, same) = programs
            .clone()
            .register(RegisteredProgram::new(key).unwrap());
        assert_eq!((again.len(), same), (1, id));
        assert_eq!(again, programs);
        let json = serde_json::to_string(&programs).unwrap();
        assert_eq!(serde_json::from_str::<Programs>(&json).unwrap(), programs);
        let bytes = bincode::serialize(&programs).unwrap();
        assert_eq!(bincode::deserialize::<Programs>(&bytes).unwrap(), programs);
    }

    #[test]
    fn program_spend_derivations_match_the_circuit() {
        let data = lb_program_spend::test_utils::sample_witness_data();
        let (_, public) = lb_program_spend::prove(data.clone().into()).unwrap();
        let inputs = core::array::from_fn(|i| {
            let input = &data.wallet.inputs[i];
            let public_key = ZkKey::new(input.secret_key).to_public_key();
            if data.chain.inputs_active[i] {
                let note = Note {
                    value: input.value,
                    nonce: input.nonce,
                    public_key,
                    program: data.chain.program_id,
                    data: input.data,
                };
                assert_eq!(
                    public.inputs[i].into_inner(),
                    derive_note_nullifier(&note.commitment(), &input.secret_key).0
                );
            }
            SpendInputFields {
                active: data.chain.inputs_active[i],
                value: input.value,
                nonce: input.nonce,
                public_key,
                data: input.data,
            }
        });
        let outputs = core::array::from_fn(|j| {
            let output = &data.wallet.outputs[j];
            let note = Note {
                value: output.value,
                nonce: output.nonce,
                public_key: ZkPublicKey::new(output.public_key),
                program: output.program,
                data: output.data,
            };
            assert_eq!(public.outputs[j].into_inner(), note.commitment().0);
            SpendOutputFields {
                active: data.chain.outputs_active[j],
                ..SpendOutputFields::of(&note)
            }
        });
        assert_eq!(
            spend_notes_hash(&inputs, &outputs),
            public.spend_notes_hash.into_inner()
        );
    }
}
