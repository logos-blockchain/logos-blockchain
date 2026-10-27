//! Shared helpers for the fee e2e scenarios.
//!
//! `ExecutionMarketReference` and the `SPEC_*` constants are the spec's price
//! update written out again by hand from the spec documents (not imported
//! from node code), so the conformance scenario notices if the node's
//! pricing math changes. The rest are plain helpers for building and
//! checking fee-paying transactions.
//!
//! Source specifications, pinned to the revision these helpers implement:
//! - [Execution Market]
//! - [Storage Markets]
//! - [Gas Cost Determination]
//!
//! [Execution Market]: https://github.com/logos-co/logos-lips/blob/035c01f78728d8bba23d89c798fb9fa640870126/docs/blockchain/raw/execution-market.md
//! [Storage Markets]: https://github.com/logos-co/logos-lips/blob/38916aa474164ac4acd81e62d19715e17626be17/docs/blockchain/raw/storage-markets.md
//! [Gas Cost Determination]: https://github.com/logos-co/logos-lips/blob/035c01f78728d8bba23d89c798fb9fa640870126/docs/blockchain/raw/analysis-gas-cost-determination.md

use std::collections::{HashMap, HashSet};

use lb_binary_codec::canonical::BinaryEncode as _;
use lb_common_http_client::ApiBlock;
use lb_core::{
    crypto::ZkHasher,
    mantle::{
        Note, OpProof, SignedOps,
        gas::{MainnetGasProfile, TxGasCalculator as _},
        ledger::verification_mode::{StandardMode, VerificationMode},
        note_set::LEDGER_SET,
        notes::derive_note_nullifier,
        ops::{OpRef, ZkTransferProof, transfer::prove_transfer},
        traits::Hashable as _,
        transactions::{
            GasPrices, MantleTxBuilder, OpProofs,
            builder::FundingNote,
            codec::minimum_signed_transaction_size,
            states::{Preverified, VerificationState},
            tx_list::ops::{OpsContext, OpsGasContext},
        },
    },
};
use lb_testing_framework::configs::wallet::WalletAccount;
use lb_wallet::{KnownNote, KnownNotes, WalletState};

use crate::common::wallet::{GenesisNotes, random_nonce};

/// Execution Market v1.3.0: base fee starts at 1 LGO, `10^9` units.
pub const SPEC_GENESIS_EXECUTION_PRICE: u128 = 1_000_000_000;
/// Execution Market v1.3.0: EMA keeps 9/10 of history per block.
pub const SPEC_EMA_PREV_WEIGHT: u128 = 9;
/// Execution Market v1.3.0: EMA denominator.
pub const SPEC_EMA_DENOMINATOR: u128 = 10;
/// Execution Market v1.3.0: `7 * G_target`, with `G_target = 1_586_120`.
pub const SPEC_BASE_FEE_NUMERATOR: u128 = 11_102_840;
/// Execution Market v1.3.0: `8 * G_target`.
pub const SPEC_BASE_FEE_DENOMINATOR: u128 = 12_688_960;
/// Gas Cost Determination v1.8.0: a transfer costs 619 gas.
pub const SPEC_TRANSFER_GAS: u64 = 619;

/// Computes a percentage reserve with checked widened arithmetic and integer
/// ceiling, matching the transaction funding rule.
pub fn priority_fee_amount(mandatory_fee: u64, priority_fee_percent: u64) -> Result<u64, String> {
    let numerator = u128::from(mandatory_fee)
        .checked_mul(u128::from(priority_fee_percent))
        .and_then(|value| value.checked_add(99))
        .ok_or_else(|| "priority fee percentage arithmetic overflowed".to_owned())?;
    u64::try_from(numerator / 100)
        .map_err(|_| "priority fee percentage result does not fit in u64".to_owned())
}

/// The spec's execution price update, written out from the spec document.
#[derive(Debug, Clone)]
pub struct ExecutionMarketReference {
    pub base_fee: u128,
    pub gas_ema: u128,
}

impl Default for ExecutionMarketReference {
    fn default() -> Self {
        Self::at_genesis()
    }
}

impl ExecutionMarketReference {
    #[must_use]
    pub const fn at_genesis() -> Self {
        Self {
            base_fee: SPEC_GENESIS_EXECUTION_PRICE,
            gas_ema: 0,
        }
    }

    /// One block's update: EMA first, then the base fee from the new EMA,
    /// in the spec's order.
    pub fn apply_block(&mut self, block_execution_gas: u64) {
        self.gas_ema = (u128::from(block_execution_gas) + SPEC_EMA_PREV_WEIGHT * self.gas_ema)
            / SPEC_EMA_DENOMINATOR;
        self.base_fee = (self.base_fee * (SPEC_BASE_FEE_NUMERATOR + self.gas_ema))
            .div_ceil(SPEC_BASE_FEE_DENOMINATOR);
    }
}

/// The gas prices and execution gas of one recorded block.
#[derive(Debug, Clone)]
pub struct GasPriceRecord {
    pub slot: u64,
    pub execution_price: u64,
    pub storage_price: u64,
    pub block_execution_gas: u64,
}

/// Execution gas of one block, from the spec's per-op table.
///
/// Only ops the fee scenarios create are mapped.
///
/// # Errors
///
/// Errors on any other op so the table gets extended instead of silently
/// miscounting.
pub fn spec_block_execution_gas(block: &ApiBlock) -> Result<u64, String> {
    let mut total = 0;

    for tx in &block.transactions {
        for op in tx.op_refs() {
            match op {
                OpRef::Transfer(_) => total += SPEC_TRANSFER_GAS,
                other => {
                    return Err(format!(
                        "unexpected operation {} in a fee-scenario block; extend the spec \
                         gas table",
                        other.as_str()
                    ));
                }
            }
        }
    }

    Ok(total)
}

/// Builds a signed self-transfer paying its mandatory fee plus `tip`, with the
/// openings of the notes it creates.
///
/// A negative `tip` deliberately underfunds it. Spends the account's genesis
/// note with a `ZkTransfer` against the genesis ledger set root. The fee is
/// recomputed until stable because the encoded size, and with it the storage
/// fee, depends on the output value.
///
/// # Panics
///
/// Panics on broken test setup, e.g. the account owns no genesis note.
#[must_use]
pub fn self_transfer_paying_fee_at(
    genesis: &GenesisNotes,
    account: &WalletAccount,
    prices: &GasPrices,
    tip: i128,
) -> (SignedOps<Preverified, StandardMode>, Vec<Note>) {
    const MAX_FEE_CONVERGENCE_ATTEMPTS: usize = 32;

    let secret_key = *account.secret_key.as_fr();
    let note = genesis
        .notes
        .iter()
        .find(|note| note.public_key == account.public_key())
        .copied()
        .expect("account should own a genesis note");
    let nullifier = derive_note_nullifier(&note.commitment(), &secret_key);
    let known = KnownNotes::from([(
        note.commitment(),
        KnownNote {
            note,
            note_set: LEDGER_SET,
            nullifier,
        },
    )]);
    let owned = WalletState::from_genesis(&known, &genesis.notes, &genesis.service_notes, 0.into())
        .expect("genesis notes should fit their note sets")
        .notes_owned_by_pks([account.public_key()])
        .pop()
        .expect("account should own a genesis note");
    let root = owned.path.root::<ZkHasher>(note.commitment().0);

    let context = OpsContext {
        gas_context: OpsGasContext::new(HashMap::new(), prices.clone()),
    };
    let output_nonce = random_nonce();

    let builder_with_output = |output_value: u64| {
        MantleTxBuilder::new()
            .extend_funding_inputs(root, [FundingNote { note, nullifier }])
            .expect("input should fit")
            .add_ledger_output(Note::new(output_value, output_nonce, account.public_key()))
            .expect("output should fit")
    };

    let fee_for_output = |output_value: u64| {
        i128::from(
            builder_with_output(output_value)
                .minimum_gas_cost::<MainnetGasProfile>(&context)
                .expect("gas cost should calculate")
                .into_inner(),
        )
    };

    let output_for_fee = |fee: i128| {
        u64::try_from(i128::from(note.value) - fee - tip).expect("output value should be positive")
    };

    // The fee depends on the encoded size, which depends on the output value,
    // which depends on the fee - so recompute until the fee stops changing.
    let mut fee = fee_for_output(note.value);
    let mut seen_fees = HashSet::new();
    let mut converged = false;

    for _ in 0..MAX_FEE_CONVERGENCE_ATTEMPTS {
        assert!(
            seen_fees.insert(fee),
            "fee calculation oscillated at repeated fee {fee}"
        );

        let recomputed = fee_for_output(output_for_fee(fee));

        if recomputed == fee {
            converged = true;
            break;
        }
        fee = recomputed;
    }

    assert!(
        converged,
        "fee calculation did not converge after {MAX_FEE_CONVERGENCE_ATTEMPTS} attempts; last \
         fee was {fee}"
    );

    let builder = builder_with_output(output_for_fee(fee));

    assert_eq!(
        builder
            .funding_delta::<MainnetGasProfile>(&context)
            .expect("funding delta should calculate"),
        tip,
        "the built transaction must carry exactly the requested tip"
    );

    let outputs = builder
        .openings()
        .into_values()
        .next()
        .expect("the self-transfer should have a funding opening")
        .outputs;
    let mantle_tx = builder.build().expect("transaction should build");
    let proof = prove_transfer(
        &[(note, secret_key, owned.path)],
        &outputs,
        root,
        mantle_tx.hash().to_fr(),
    );
    let op_proofs = OpProofs::from([OpProof::ZkTransfer(ZkTransferProof::new(proof))]);

    let signed_tx = SignedOps::from_parts(mantle_tx, op_proofs)
        .expect("signed transaction should build")
        .preverify()
        .expect("signed transaction should be valid");
    (signed_tx, outputs)
}

/// The storage fee is charged on a predicted size; it has to match the real
/// encoding.
///
/// # Errors
///
/// Returns both sizes when they differ.
pub fn check_size_prediction<State: VerificationState, Mode: VerificationMode>(
    tx: &SignedOps<State, Mode>,
    prices: &GasPrices,
) -> Result<(), String> {
    let gas_context = OpsGasContext::new(HashMap::new(), prices.clone());
    let predicted = minimum_signed_transaction_size(&tx.op_refs(), &gas_context) as u64;
    let actual = tx.encode().len() as u64;
    if predicted == actual {
        Ok(())
    } else {
        Err(format!(
            "predicted signed size {predicted} diverges from the actual encoding {actual}"
        ))
    }
}

/// Net balance of a transaction, the amount it burns: the excess of each
/// Transfer is its inputs minus its outputs.
///
/// # Errors
///
/// Errors when the excess total overflows.
pub fn net_balance<State: VerificationState, Mode: VerificationMode>(
    tx: &SignedOps<State, Mode>,
) -> Result<u64, String> {
    tx.op_refs()
        .into_iter()
        .filter_map(|op| match op {
            OpRef::Transfer(transfer) => Some(transfer.excess_value),
            _ => None,
        })
        .try_fold(0u64, |total, excess| {
            total
                .checked_add(excess)
                .ok_or_else(|| format!("transaction {:?} excess overflowed", tx.hash()))
        })
}

/// Surplus balance left after charging a transaction at `prices`.
///
/// A non-negative value means the transaction remains funded; the surplus is
/// its effective tip at those prices.
///
/// # Errors
///
/// Returns an error when the transaction's balance or gas cost cannot be
/// calculated.
pub fn fee_surplus_at<State: VerificationState, Mode: VerificationMode>(
    tx: &SignedOps<State, Mode>,
    prices: &GasPrices,
) -> Result<i128, String> {
    let paid = net_balance(tx)?;
    let gas_context = OpsGasContext::new(HashMap::new(), prices.clone());
    let required = tx
        .op_refs()
        .total_gas_cost::<MainnetGasProfile>(&gas_context)
        .map_err(|source| format!("transaction gas cost calculation failed: {source}"))?;

    Ok(i128::from(paid) - i128::from(required.into_inner()))
}
