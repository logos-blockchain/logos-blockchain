use key_management_system_keys::keys::ZkPublicKey;
use nomos_core::mantle::{SignedMantleTx, Transaction as _};
use nomos_wallet::{WalletService, api::WalletApi};
use num_bigint::BigUint;

use crate::{
    NomosNode,
    api::{
        ValueResult,
        cryptarchia::{Hash, HeaderId, get_cryptarchia_info_sync},
        free,
        types::value::Value,
    },
    errors::OperationStatus,
};

/// Get the balance of a wallet address
///
/// This is a synchronous wrapper around [`WalletApi::get_balance`].
///
/// # Arguments
///
/// - `node`: A [`NomosNode`] instance.
/// - `tip`: The header ID to query the balance at.
/// - `wallet_address`: The public key of the wallet address to query.
///
/// # Returns
///
/// A `Result` containing an [`Option<Value>`] on success, or an
/// [`OperationStatus`] error on failure.
pub(crate) fn get_balance_sync(
    node: &NomosNode,
    tip: nomos_core::header::HeaderId,
    wallet_address: ZkPublicKey,
) -> Result<Option<Value>, OperationStatus> {
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        eprintln!("[Failed]to create tokio runtime. Aborting.");
        return Err(OperationStatus::RuntimeError);
    };

    runtime
        .block_on(async {
            let api = WalletApi::<WalletService<_, _, _, _, _>, _>::from_overwatch_handle(
                node.get_overwatch_handle(),
            )
            .await;
            api.get_balance(tip, wallet_address).await
        })
        .map_err(|_| OperationStatus::DynError)
}

pub type BalanceResult = ValueResult<Value, OperationStatus>;

/// Get the balance of a wallet address
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`NomosNode`] instance.
/// - `wallet_address`: A non-null pointer to the public key bytes of the wallet
///   address to query.
/// - `optional_tip`: An optional pointer to the header ID to query the balance
///   at. If null, the current tip will be used.
///
/// # Returns
///
/// A [`ValueResult`] containing the balance on success, or an
/// [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all pointers are valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_balance(
    node: *const NomosNode,
    wallet_address: *const u8,
    optional_tip: *const HeaderId,
) -> BalanceResult {
    if node.is_null() {
        eprintln!("[get_balance] Received a null `node` pointer. Exiting.");
        return BalanceResult::from_error(OperationStatus::NullPointer);
    }
    if wallet_address.is_null() {
        eprintln!("[get_balance] Received a null `wallet_address` pointer. Exiting.");
        return BalanceResult::from_error(OperationStatus::NullPointer);
    }

    let node = unsafe { &*node };
    let tip = if optional_tip.is_null() {
        match get_cryptarchia_info_sync(node) {
            Ok(cryptarchia_info) => cryptarchia_info.tip,
            Err(error) => return BalanceResult::from_error(error),
        }
    } else {
        nomos_core::header::HeaderId::from(unsafe { *optional_tip })
    };
    let wallet_address_bytes = unsafe { std::slice::from_raw_parts(wallet_address, 32) };
    let wallet_address = ZkPublicKey::from(BigUint::from_bytes_le(wallet_address_bytes));

    match get_balance_sync(node, tip, wallet_address) {
        Ok(Some(balance)) => BalanceResult::from_value(balance),
        Ok(None) => BalanceResult::from_error(OperationStatus::NotFound),
        Err(status) => BalanceResult::from_error(status),
    }
}

#[repr(C)]
pub struct TransferFundsArguments {
    pub optional_tip: *const HeaderId,
    pub change_public_key: *const u8,
    pub funding_public_keys: *const *const u8,
    pub funding_public_keys_len: usize,
    pub recipient_public_key: *const u8,
    pub amount: u64,
}

impl TransferFundsArguments {
    /// Validates the arguments of the [`TransferFundsArguments`] struct.
    ///
    /// # Returns
    ///
    /// A `Result` indicating success or containing an error message and status.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it dereferences raw pointers. The caller
    /// must ensure that all pointers are valid.
    pub unsafe fn validate(&self) -> Result<(), (String, OperationStatus)> {
        if self.change_public_key.is_null() {
            return Err((
                "TransferFunds contains a null `change_public_key` pointer.".to_owned(),
                OperationStatus::NullPointer,
            ));
        }
        if self.funding_public_keys.is_null() {
            return Err((
                "TransferFunds contains a null `funding_public_keys` pointer.".to_owned(),
                OperationStatus::NullPointer,
            ));
        }

        for i in 0..self.funding_public_keys_len {
            let funding_public_key_pointer = unsafe { self.funding_public_keys.add(i) };
            let funding_public_key = unsafe { *funding_public_key_pointer };
            if funding_public_key.is_null() {
                let error_message =
                    format!("TransferFunds contains a null pointer at `funding_public_keys[{i}]`.");
                return Err((error_message, OperationStatus::NullPointer));
            }
        }

        if self.recipient_public_key.is_null() {
            return Err((
                "TransferFunds contains a null `recipient_public_key` pointer.".to_owned(),
                OperationStatus::NullPointer,
            ));
        }
        Ok(())
    }
}

/// Transfer funds from some addresses to another.
///
/// This is a synchronous wrapper around [`WalletApi::transfer_funds`].
///
/// This function does not validate the arguments. It assumes they have already
/// been validated.
///
/// # Arguments
///
/// - `node`: A [`NomosNode`] instance.
/// - `tip`: The header ID at which to perform the transfer.
/// - `change_public_key`: The public key to receive any change from the
///   transaction.
/// - `funding_public_keys`: A vector of public keys to fund the transaction.
/// - `recipient_public_key`: The public key of the recipient.
/// - `amount`: The amount to transfer.
///
/// # Returns
///
/// A `Result` containing a [`SignedMantleTx`] on success, or an
/// [`OperationStatus`] error on failure.
pub(crate) fn transfer_funds_sync(
    node: &NomosNode,
    tip: nomos_core::header::HeaderId,
    change_public_key: ZkPublicKey,
    funding_public_keys: Vec<ZkPublicKey>,
    recipient_public_key: ZkPublicKey,
    amount: u64,
) -> Result<SignedMantleTx, OperationStatus> {
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        eprintln!("[transfer_funds_sync] Failed to create tokio runtime. Aborting.");
        return Err(OperationStatus::RuntimeError);
    };

    runtime
        .block_on(async {
            let api = WalletApi::<WalletService<_, _, _, _, _>, _>::from_overwatch_handle(
                node.get_overwatch_handle(),
            )
            .await;
            api.transfer_funds(
                tip,
                change_public_key,
                funding_public_keys,
                recipient_public_key,
                amount,
            )
            .await
        })
        .map_err(|_| OperationStatus::DynError)
}

pub type TransferFundsResult = ValueResult<Hash, OperationStatus>;

/// Transfer funds from some addresses to another.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`NomosNode`] instance.
/// - `arguments`: A non-null pointer to a [`TransferFundsArguments`] struct
///   containing the transaction arguments.
///
/// # Returns
///
/// A [`TransferFundsResult`] containing a pointer to a [`Hash`] where the
/// transaction hash will be written on success, or an [`OperationStatus`] error
/// on failure. The hash will be written in little-endian format.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all pointers are valid.
///
/// # Memory Management
///
/// This function allocates memory for the output [`CryptarchiaInfo`] struct.
/// The caller must free this memory using the [`free_cryptarchia_info`]
/// function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn transfer_funds(
    node: *const NomosNode,
    arguments: *const TransferFundsArguments,
) -> TransferFundsResult {
    if node.is_null() {
        eprintln!("[transfer_funds] Received a null `node` pointer. Exiting.");
        return TransferFundsResult::from_error(OperationStatus::NullPointer);
    }
    if arguments.is_null() {
        eprintln!("[transfer_funds] Received a null `arguments` pointer. Exiting.");
        return TransferFundsResult::from_error(OperationStatus::NullPointer);
    }
    let arguments = unsafe { &*arguments };
    if let Err((error_message, status)) = unsafe { arguments.validate() } {
        eprintln!("[transfer_funds] {error_message} Exiting.");
        return TransferFundsResult::from_error(status);
    }

    let node = unsafe { &*node };
    let tip = if arguments.optional_tip.is_null() {
        match get_cryptarchia_info_sync(node) {
            Ok(cryptarchia_info) => cryptarchia_info.tip,
            Err(status) => {
                eprintln!("[transfer_funds] Failed to get cryptarchia info. Aborting.");
                return TransferFundsResult::from_error(status);
            }
        }
    } else {
        nomos_core::header::HeaderId::from(unsafe { *arguments.optional_tip })
    };
    let change_public_key = {
        let change_public_key_bytes =
            unsafe { std::slice::from_raw_parts(arguments.change_public_key, 32) };
        ZkPublicKey::from(BigUint::from_bytes_le(change_public_key_bytes))
    };
    let funding_public_keys = {
        let funding_public_keys_pointers = unsafe {
            std::slice::from_raw_parts(
                arguments.funding_public_keys,
                arguments.funding_public_keys_len,
            )
        };
        funding_public_keys_pointers
            .iter()
            .map(|funding_public_key_pointer| {
                let funding_public_key_bytes =
                    unsafe { std::slice::from_raw_parts(*funding_public_key_pointer, 32) };
                ZkPublicKey::from(BigUint::from_bytes_le(funding_public_key_bytes))
            })
            .collect::<Vec<_>>()
    };
    let recipient_public_key = {
        let recipient_public_key_bytes =
            unsafe { std::slice::from_raw_parts(arguments.recipient_public_key, 32) };
        ZkPublicKey::from(BigUint::from_bytes_le(recipient_public_key_bytes))
    };
    let amount = Value::from(arguments.amount);

    match transfer_funds_sync(
        node,
        tip,
        change_public_key,
        funding_public_keys,
        recipient_public_key,
        amount,
    ) {
        Ok(transaction) => {
            let transaction_hash = transaction.hash().as_signing_bytes();
            let Ok(transaction_hash_array) = transaction_hash.iter().as_slice().try_into() else {
                eprintln!("[transfer_funds] Failed to convert transaction hash to array. Exiting.");
                return TransferFundsResult::from_error(OperationStatus::RuntimeError);
            };
            TransferFundsResult::from_value(transaction_hash_array)
        }
        Err(status) => TransferFundsResult::from_error(status),
    }
}

/// Frees the memory allocated for a [`Hash`] value.
///
/// # Arguments
///
/// - `pointer`: A pointer to the [`Hash`] to be freed.
#[unsafe(no_mangle)]
pub extern "C" fn free_transfer_funds(pointer: *mut Hash) {
    free::<Hash>(pointer);
}
