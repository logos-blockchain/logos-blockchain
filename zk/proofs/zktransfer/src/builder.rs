//! The witness of a `ZkTransfer`, built in two steps: the notes it spends and
//! creates, known to whoever assembles the transfer, then the spending secret
//! keys of its inputs, known only to their holders.
//!
//! The witness proves only once it holds a secret key for every input:
//! [`ZkTransferWitnessBuilder::build`] exists only in the [`Signed`] state.

use std::array;

use lb_groth16::Fr;

use crate::{
    InputPathAndSelectors, MAX_INPUTS, MAX_OUTPUTS, ZkTransferChainInputsData,
    ZkTransferInputNoteData, ZkTransferInputsError, ZkTransferOutputNoteData,
    ZkTransferWalletInputsData, ZkTransferWitnessInputsData,
};

/// An input note of a `ZkTransfer` without the spending secret key, which only
/// its holder knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZkTransferUnsignedInput {
    // The staking key of the note, which the circuit does not prove.
    pub stake_public_key: Fr,
    pub nonce: Fr,
    pub value: u64,
    pub path_and_selectors: InputPathAndSelectors,
}

/// The state of a witness still lacking the secret keys of its inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unsigned;

/// The state of a witness holding the secret key of each of its inputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signed {
    // One per input, in input order
    secret_keys: Vec<Fr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ZkTransferSecretKeysError {
    #[error("the transfer spends {inputs} notes but {keys} secret keys were given")]
    KeyCount { inputs: usize, keys: usize },
}

/// The witness of a `ZkTransfer`, in the state `S`: [`Unsigned`] while its
/// notes are added, [`Signed`] once it holds the secret keys of its inputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZkTransferWitnessBuilder<S = Unsigned> {
    inputs: Vec<ZkTransferUnsignedInput>,
    outputs: Vec<ZkTransferOutputNoteData>,
    tx_commitment_root: Fr,
    msg: Fr,
    state: S,
}

impl<S> ZkTransferWitnessBuilder<S> {
    /// The root the transfer is proven against.
    #[must_use]
    pub const fn tx_commitment_root(&self) -> Fr {
        self.tx_commitment_root
    }

    /// The message the transfer is bound to.
    #[must_use]
    pub const fn msg(&self) -> Fr {
        self.msg
    }

    /// The notes the transfer spends, in input order.
    #[must_use]
    pub fn inputs(&self) -> &[ZkTransferUnsignedInput] {
        &self.inputs
    }

    /// The notes the transfer creates, in output order.
    #[must_use]
    pub fn outputs(&self) -> &[ZkTransferOutputNoteData] {
        &self.outputs
    }
}

impl ZkTransferWitnessBuilder<Unsigned> {
    /// A transfer proven against `tx_commitment_root` and bound to `msg`,
    /// spending and creating no note yet.
    #[must_use]
    pub const fn new(tx_commitment_root: Fr, msg: Fr) -> Self {
        Self {
            inputs: Vec::new(),
            outputs: Vec::new(),
            tx_commitment_root,
            msg,
            state: Unsigned,
        }
    }

    /// Spends one more note, failing if the circuit fits no more.
    pub fn input(mut self, input: ZkTransferUnsignedInput) -> Result<Self, ZkTransferInputsError> {
        if self.inputs.len() == MAX_INPUTS {
            return Err(ZkTransferInputsError::TooManyInputs(MAX_INPUTS + 1));
        }
        self.inputs.push(input);
        Ok(self)
    }

    /// Creates one more note, failing if the circuit fits no more.
    pub fn output(
        mut self,
        output: ZkTransferOutputNoteData,
    ) -> Result<Self, ZkTransferInputsError> {
        if self.outputs.len() == MAX_OUTPUTS {
            return Err(ZkTransferInputsError::TooManyOutputs(MAX_OUTPUTS + 1));
        }
        self.outputs.push(output);
        Ok(self)
    }

    /// The number of notes the transfer spends, each needing a secret key.
    #[must_use]
    pub const fn inputs_len(&self) -> usize {
        self.inputs.len()
    }

    /// Every input spent with `secret_key`, the notes of a single holder.
    #[must_use]
    pub fn with_secret_key(self, secret_key: Fr) -> ZkTransferWitnessBuilder<Signed> {
        let secret_keys = vec![secret_key; self.inputs.len()];
        self.signed(secret_keys)
    }

    /// The i-th input spent with `secret_keys[i]`, the notes of several
    /// holders.
    pub fn with_secret_keys(
        self,
        secret_keys: &[Fr],
    ) -> Result<ZkTransferWitnessBuilder<Signed>, ZkTransferSecretKeysError> {
        if secret_keys.len() != self.inputs.len() {
            return Err(ZkTransferSecretKeysError::KeyCount {
                inputs: self.inputs.len(),
                keys: secret_keys.len(),
            });
        }
        Ok(self.signed(secret_keys.to_vec()))
    }

    fn signed(self, secret_keys: Vec<Fr>) -> ZkTransferWitnessBuilder<Signed> {
        ZkTransferWitnessBuilder {
            inputs: self.inputs,
            outputs: self.outputs,
            tx_commitment_root: self.tx_commitment_root,
            msg: self.msg,
            state: Signed { secret_keys },
        }
    }
}

impl ZkTransferWitnessBuilder<Signed> {
    /// The witness the transfer proves with, the slots past its notes inactive.
    #[must_use]
    pub fn build(self) -> ZkTransferWitnessInputsData {
        let Self {
            inputs,
            outputs,
            tx_commitment_root,
            msg,
            state: Signed { secret_keys },
        } = self;
        let wallet = ZkTransferWalletInputsData {
            inputs: array::from_fn(|i| {
                inputs.get(i).zip(secret_keys.get(i)).map_or_else(
                    ZkTransferInputNoteData::inactive,
                    |(input, secret_key)| ZkTransferInputNoteData {
                        secret_key: *secret_key,
                        stake_public_key: input.stake_public_key,
                        nonce: input.nonce,
                        value: input.value,
                        path_and_selectors: input.path_and_selectors,
                    },
                )
            }),
            outputs: array::from_fn(|j| {
                outputs
                    .get(j)
                    .copied()
                    .unwrap_or_else(ZkTransferOutputNoteData::inactive)
            }),
        };
        let chain = ZkTransferChainInputsData {
            inputs_active: array::from_fn(|i| i < inputs.len()),
            outputs_active: array::from_fn(|j| j < outputs.len()),
            tx_commitment_root,
            msg,
        };
        ZkTransferWitnessInputsData::from_chain_and_wallet_data(chain, wallet)
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::AdditiveGroup as _;

    use super::*;
    use crate::{TX_COMMITMENT_TREE_DEPTH, test_utils::sample_unsigned_witness};

    fn input(value: u64) -> ZkTransferUnsignedInput {
        ZkTransferUnsignedInput {
            stake_public_key: Fr::from(1u64),
            nonce: Fr::from(2u64),
            value,
            path_and_selectors: [(Fr::ZERO, false); TX_COMMITMENT_TREE_DEPTH],
        }
    }

    #[test]
    fn the_circuit_bounds_the_notes() {
        let full = (0..MAX_INPUTS as u64).try_fold(
            ZkTransferWitnessBuilder::new(Fr::ZERO, Fr::ZERO),
            |builder, value| builder.input(input(value)),
        );
        assert_eq!(
            full.unwrap().input(input(9)),
            Err(ZkTransferInputsError::TooManyInputs(MAX_INPUTS + 1))
        );
        let full = (0..MAX_OUTPUTS).try_fold(
            ZkTransferWitnessBuilder::new(Fr::ZERO, Fr::ZERO),
            |builder, _| builder.output(ZkTransferOutputNoteData::inactive()),
        );
        assert_eq!(
            full.unwrap().output(ZkTransferOutputNoteData::inactive()),
            Err(ZkTransferInputsError::TooManyOutputs(MAX_OUTPUTS + 1))
        );
    }

    #[test]
    fn one_key_per_input() {
        let builder = ZkTransferWitnessBuilder::new(Fr::ZERO, Fr::ZERO)
            .input(input(1))
            .and_then(|builder| builder.input(input(2)))
            .unwrap();
        assert_eq!(
            builder.clone().with_secret_keys(&[Fr::from(3u64)]),
            Err(ZkTransferSecretKeysError::KeyCount { inputs: 2, keys: 1 })
        );
        let witness = builder
            .with_secret_keys(&[Fr::from(3u64), Fr::from(4u64)])
            .unwrap()
            .build();
        assert_eq!(witness.wallet.inputs[0].secret_key, Fr::from(3u64));
        assert_eq!(witness.wallet.inputs[1].secret_key, Fr::from(4u64));
        assert_eq!(witness.chain.inputs_active, [true, true, false, false]);
    }

    #[test]
    fn a_single_key_spends_every_input() {
        let witness = ZkTransferWitnessBuilder::new(Fr::ZERO, Fr::ZERO)
            .input(input(1))
            .and_then(|builder| builder.input(input(2)))
            .unwrap()
            .with_secret_key(Fr::from(5u64))
            .build();
        assert_eq!(witness.wallet.inputs[0].secret_key, Fr::from(5u64));
        assert_eq!(witness.wallet.inputs[1].secret_key, Fr::from(5u64));
        assert_eq!(
            witness.wallet.inputs[2],
            ZkTransferInputNoteData::inactive()
        );
    }

    #[test]
    fn the_signed_sample_is_the_sample_on_its_active_notes() {
        let (unsigned, secret_keys) = sample_unsigned_witness();
        let built = unsigned.with_secret_keys(&secret_keys).unwrap().build();
        let sample = crate::test_utils::sample_witness_data();
        assert_eq!(built.chain, sample.chain);
        for (i, active) in sample.chain.inputs_active.iter().enumerate() {
            if *active {
                assert_eq!(built.wallet.inputs[i], sample.wallet.inputs[i]);
            }
        }
        for (j, active) in sample.chain.outputs_active.iter().enumerate() {
            if *active {
                assert_eq!(built.wallet.outputs[j], sample.wallet.outputs[j]);
            }
        }
    }
}
