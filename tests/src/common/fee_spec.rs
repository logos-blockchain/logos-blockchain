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
//! [Execution Market]: https://github.com/logos-co/logos-lips/blob/38916aa474164ac4acd81e62d19715e17626be17/docs/blockchain/raw/execution-market.md
//! [Storage Markets]: https://github.com/logos-co/logos-lips/blob/38916aa474164ac4acd81e62d19715e17626be17/docs/blockchain/raw/storage-markets.md
//! [Gas Cost Determination]: https://github.com/logos-co/logos-lips/blob/38916aa474164ac4acd81e62d19715e17626be17/docs/blockchain/raw/analysis-gas-cost-determination.md

use std::collections::{HashMap, HashSet};

use lb_codec::BinaryEncode as _;
use lb_common_http_client::ApiBlock;
use lb_core::mantle::{
    Note, SignedOps, Utxo,
    gas::{MainnetGasProfile, TxGasCalculator as _},
    ledger::verification_mode::{StandardMode, VerificationMode},
    ops::OpRef,
    traits::Hashable as _,
    transactions::{
        GasPrices, MantleTxBuilder,
        codec::minimum_signed_transaction_size,
        states::{Preverified, VerificationState},
        tx_list::ops::{OpsContext, OpsGasContext},
    },
};
use lb_testing_framework::configs::wallet::WalletAccount;

use crate::common::wallet::transfer_proofs_for_funded_wallet_tx;

/// Execution Market v1.1.0: base fee starts at 1.
pub const SPEC_GENESIS_EXECUTION_PRICE: u128 = 1;
/// Execution Market v1.1.0: EMA keeps 9/10 of history per block.
pub const SPEC_EMA_PREV_WEIGHT: u128 = 9;
/// Execution Market v1.1.0: EMA denominator.
pub const SPEC_EMA_DENOMINATOR: u128 = 10;
/// Execution Market v1.1.0: `7 * G_target`, with `G_target = 1_596_730`.
pub const SPEC_BASE_FEE_NUMERATOR: u128 = 11_177_110;
/// Execution Market v1.1.0: `8 * G_target`.
pub const SPEC_BASE_FEE_DENOMINATOR: u128 = 12_773_840;
/// Gas Cost Determination v1.5.1: a transfer costs 590 gas.
pub const SPEC_TRANSFER_GAS: u64 = 590;

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

/// Builds a signed self-transfer paying its mandatory fee plus `tip`.
///
/// A negative `tip` deliberately underfunds it. Spends the account's genesis
/// note. The fee is recomputed until stable because the encoded size, and
/// with it the storage fee, depends on the output value.
///
/// # Panics
///
/// Panics on broken test setup, e.g. the account owns no genesis note.
#[must_use]
pub fn self_transfer_paying_fee_at(
    genesis_utxos: &[Utxo],
    account: &WalletAccount,
    prices: &GasPrices,
    tip: i128,
) -> SignedOps<Preverified, StandardMode> {
    const MAX_FEE_CONVERGENCE_ATTEMPTS: usize = 32;

    let utxo = genesis_utxos
        .iter()
        .find(|utxo| utxo.note.pk == account.public_key())
        .copied()
        .expect("account should own a genesis note");

    let gas_context = OpsGasContext::new(HashMap::new(), HashMap::new(), prices.clone());
    let context = OpsContext {
        gas_context,
        leader_reward_amount: 0,
    };

    let builder_with_output = |output_value: u64| {
        MantleTxBuilder::new()
            .add_ledger_input(utxo)
            .expect("input should fit")
            .add_ledger_output(Note::new(output_value, account.public_key()))
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
        u64::try_from(i128::from(utxo.note.value) - fee - tip)
            .expect("output value should be positive")
    };

    // The fee depends on the encoded size, which depends on the output value,
    // which depends on the fee - so recompute until the fee stops changing.
    let mut fee = fee_for_output(utxo.note.value);
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

    let mantle_tx = builder.build().expect("transaction should build");
    let op_proofs = transfer_proofs_for_funded_wallet_tx(&mantle_tx, &account.secret_key)
        .expect("transfer proofs should build");

    SignedOps::from_parts(mantle_tx, op_proofs)
        .expect("signed transaction should build")
        .preverify()
        .expect("signed transaction should be valid")
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
    let gas_context = OpsGasContext::new(HashMap::new(), HashMap::new(), prices.clone());
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

/// Net balance (inputs minus outputs) of a transaction - the amount it
/// burns. Inputs are valued against the genesis UTXO set.
///
/// # Errors
///
/// Errors on inputs that are not genesis notes; the fee scenarios only spend
/// genesis notes.
pub fn net_balance_against<State: VerificationState, Mode: VerificationMode>(
    genesis_utxos: &[Utxo],
    tx: &SignedOps<State, Mode>,
) -> Result<u64, String> {
    let mut input_sum = 0u64;
    let mut output_sum = 0u64;
    for op in tx.op_refs() {
        if let OpRef::Transfer(transfer) = op {
            for input in transfer.inputs.iter() {
                let utxo = genesis_utxos
                    .iter()
                    .find(|utxo| utxo.id() == *input)
                    .ok_or_else(|| {
                        format!("transaction {:?} spends a non-genesis note", tx.hash())
                    })?;
                input_sum += utxo.note.value;
            }
            for note in transfer.outputs.iter() {
                output_sum += note.value;
            }
        }
    }
    Ok(input_sum - output_sum)
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
    genesis_utxos: &[Utxo],
    tx: &SignedOps<State, Mode>,
    prices: &GasPrices,
) -> Result<i128, String> {
    let paid = net_balance_against(genesis_utxos, tx)?;
    let required = tx
        .total_gas_cost::<MainnetGasProfile>(prices)
        .map_err(|source| format!("transaction gas cost calculation failed: {source}"))?;

    Ok(i128::from(paid) - i128::from(required.into_inner()))
}
