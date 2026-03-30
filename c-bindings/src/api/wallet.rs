//! ```rust
//! A module for performing wallet transactions and operations in the Logos Blockchain network.
//!
//! Gets the known wallet addresses from the wallet service.
//!
//! This function queries the known addresses synchronously by using the wallet API.
//!
//! # Arguments
//!
//! - `node`: A reference to an instance of [`LogosBlockchainNode`].
//!
//! # Returns
//!
//! A `Result` containing a vector of [`ZkPublicKey`] on success, or an [`OperationStatus`] error
//! on failure.
//!
//! # Errors
//!
//! - Returns [`OperationStatus::NotFound`] if the wallet addresses cannot be retrieved.
//! pub

use core::ptr;

use lb_api_service::http::mempool;
use lb_core::mantle::{SignedMantleTx, Transaction};
use lb_groth16::{fr_from_bytes, fr_to_bytes};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_wallet_service::{WalletService, api::WalletApi};
use num_bigint::BigUint;

use crate::{
    LogosBlockchainNode,
    api::{
        ValueResult,
        cryptarchia::{Hash, HeaderId, get_cryptarchia_info_sync},
        free,
        types::value::Value,
    },
    errors::OperationStatus,
};

/// ```rust
/// 
/// Fetches the list of known addresses from the `LogosBlockchainNode`.
///
/// # Parameters
/// - `node`: A reference to the `LogosBlockchainNode` instance from which the known addresses are to be retrieved.
///
/// # Returns
/// This function returns a `Result`:
/// - `Ok(Vec<ZkPublicKey>)`: A vector containing the known zero-knowledge public keys (`ZkPublicKey`).
/// - `Err(OperationStatus)`: An error status (`OperationStatus::NotFound`) if the operation fails.
///
/// # Errors
/// This function will return `OperationStatus::NotFound` in case of an error during communication with the Wallet API or if the addresses cannot be fetched.
///
/// # Implementation Details
/// - The function asynchronously interacts with the `WalletApi` via the runtime handle of the provided `node`.
/// - Any errors encountered during the retrieval are logged to the standard error stream (`eprintln`).
///
/// # Example
/// ```rust
/// let node = LogosBlockchainNode::new(); // Assume LogosBlockchainNode is
/// initialized match get_known_addresses(&node) {
///     Ok(addresses) => println!("Known Addresses: {:?}", addresses),
///     Err(status) => eprintln!("Failed to fetch addresses: {:?}", status),
/// }
/// ```
/// 
/// # Notes
/// - This function utilizes the runtime's `block_on` method to execute the asynchronous operations in a blocking manner.
/// - The returned public keys represent a set of addresses known to the Wallet API.
/// ```
pub(crate) fn get_known_addresses_sync(
    node: &LogosBlockchainNode,
) -> Result<Vec<ZkPublicKey>, OperationStatus> {
    let runtime_handle = node.get_overwatch_handle().runtime();
    runtime_handle.block_on(async {
        let api = WalletApi::<WalletService<_, _, _, _, _>, _>::from_overwatch_handle(
            node.get_overwatch_handle(),
        )
        .await;
        api.get_known_addresses().await.map_err(|e| {
            log::error!("{e:?}");
            OperationStatus::NotFound
        })
    })
}

#[repr(C)]
pub struct KnownAddresses {
    pub addresses: *mut *mut u8,
    pub len: usize,
}

pub type KnownAddressesResult = ValueResult<KnownAddresses, OperationStatus>;

impl Default for KnownAddresses {
    fn default() -> Self {
        Self {
            addresses: ptr::null_mut(),
            len: 0,
        }
    }
}

/// Retrieves the list of known wallet addresses from the Logos Blockchain node.
///
/// This function queries the wallet service for all known zero-knowledge public
/// keys (wallet addresses) and returns them as a C-compatible structure
/// containing an array of byte pointers.
///
/// # Arguments
///
/// * `node` - A non-null pointer to a [`LogosBlockchainNode`] instance from
///   which the known addresses will be retrieved.
///
/// # Returns
///
/// Returns a [`KnownAddressesResult`] containing:
/// - On success: A [`KnownAddresses`] struct with an array of pointers to
///   32-byte address representations and the array length.
/// - On failure: An [`OperationStatus`] error indicating the reason for
///   failure.
///
/// # Errors
///
/// This function returns an error in the following cases:
/// - [`OperationStatus::NullPointer`] if the `node` pointer is null.
/// - [`OperationStatus::NotFound`] if the wallet addresses cannot be retrieved
///   from the wallet service.
///
/// # Safety
///
/// This function is unsafe because it:
/// - Dereferences the raw `node` pointer, which must be valid and properly
///   aligned.
/// - Returns raw pointers to heap-allocated memory that must be properly freed.
///
/// The caller must ensure:
/// - The `node` pointer is non-null and points to a valid
///   [`LogosBlockchainNode`] instance.
/// - The `node` pointer remains valid for the duration of this function call.
/// - The returned [`KnownAddresses`] is properly freed using
///   [`free_known_addresses`] to prevent memory leaks.
///
/// # Memory Management
///
/// This function allocates memory for:
/// - An array of pointers to 32-byte address data.
/// - Each individual 32-byte address array.
///
/// The caller **must** free this memory using the [`free_known_addresses`]
/// function when the addresses are no longer needed. Failure to do so will
/// result in memory leaks.
///
/// # Example
///
/// ```c
/// // C usage example
/// LogosBlockchainNode* node = create_node();
/// KnownAddressesResult result = get_known_addresses(node);
///
/// if (result.status == OperationStatus_Ok) {
///     KnownAddresses addresses = result.value;
///     for (size_t i = 0; i < addresses.len; i++) {
///         uint8_t* address = addresses.addresses[i];
///         // Use the 32-byte address...
///     }
///     free_known_addresses(addresses);
/// }
/// ```
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_known_addresses(
    node: *const LogosBlockchainNode,
) -> KnownAddressesResult {
    if node.is_null() {
        log::error!("[get_known_addresses] Received a null `node` pointer. Exiting.");
        return KnownAddressesResult::from_error(OperationStatus::NullPointer);
    }

    let node = unsafe { &*node };

    match get_known_addresses_sync(node) {
        Ok(addresses) => {
            let address_pointers: Vec<*mut u8> = addresses
                .into_iter()
                .map(|pk| {
                    let bytes = fr_to_bytes(pk.as_fr());
                    Box::into_raw(Box::new(bytes)).cast::<u8>()
                })
                .collect();
            let len = address_pointers.len();
            let addresses_ptr = Box::leak(address_pointers.into_boxed_slice()).as_mut_ptr();

            KnownAddressesResult::from_value(KnownAddresses {
                addresses: addresses_ptr,
                len,
            })
        }
        Err(status) => KnownAddressesResult::from_error(status),
    }
}

/// Frees the memory allocated for a [`KnownAddresses`] structure.
///
/// This function deallocates all memory associated with a [`KnownAddresses`]
/// structure, including:
/// - The array of pointers to individual address data.
/// - Each individual 32-byte address array.
///
/// This function **must** be called to free the memory allocated by
/// [`get_known_addresses`] to prevent memory leaks.
///
/// # Arguments
///
/// * `addresses` - A [`KnownAddresses`] structure previously returned by
///   [`get_known_addresses`]. This structure will be consumed and all its
///   associated memory will be freed.
///
/// # Safety
///
/// This function is unsafe because it:
/// - Reconstructs `Vec` and `Box` types from raw pointers using
///   [`Vec::from_raw_parts`] and [`Box::from_raw`].
/// - Assumes the pointers in `addresses` were allocated by
///   [`get_known_addresses`].
///
/// The caller must ensure:
/// - The `addresses` parameter was obtained from [`get_known_addresses`].
/// - The `addresses` parameter has not been previously freed.
/// - No other references to the address data exist after this call.
/// - This function is called exactly once per [`KnownAddresses`] instance.
///
/// Violating these requirements will result in undefined behavior, including
/// double-free errors or use-after-free bugs.
///
/// # Example
///
/// ```c
/// // C usage example
/// LogosBlockchainNode* node = create_node();
/// KnownAddressesResult result = get_known_addresses(node);
///
/// if (result.status == OperationStatus_Ok) {
///     KnownAddresses addresses = result.value;
///
///     // Use the addresses...
///     for (size_t i = 0; i < addresses.len; i++) {
///         uint8_t* address = addresses.addresses[i];
///         // Process the 32-byte address...
///     }
///
///     // Free the memory when done
///     free_known_addresses(addresses);
/// }
/// ```
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_known_addresses(addresses: KnownAddresses) {
    if !addresses.addresses.is_null() {
        let address_pointers = unsafe {
            Box::from_raw(ptr::slice_from_raw_parts_mut(
                addresses.addresses,
                addresses.len,
            ))
        };
        for ptr in address_pointers {
            if !ptr.is_null() {
                unsafe {
                    drop(Box::from_raw(ptr.cast::<[u8; 32]>()));
                }
            }
        }
    }
}

/// Get the balance of a wallet address
///
/// This is a synchronous wrapper around [`WalletApi::get_balance`].
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
/// - `tip`: The header ID to query the balance at.
/// - `wallet_address`: The public key of the wallet address to query.
///
/// # Returns
///
/// A `Result` containing an [`Option<Value>`] on success, or an
/// [`OperationStatus`] error on failure.
pub(crate) fn get_balance_sync(
    node: &LogosBlockchainNode,
    tip: lb_core::header::HeaderId,
    wallet_address: ZkPublicKey,
) -> Result<Option<Value>, OperationStatus> {
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        log::error!("[Failed]to create tokio runtime. Aborting.");
        return Err(OperationStatus::RuntimeError);
    };

    runtime
        .block_on(async {
            let api = WalletApi::<WalletService<_, _, _, _, _>, _>::from_overwatch_handle(
                node.get_overwatch_handle(),
            )
            .await;
            api.get_balance(Some(tip), wallet_address)
                .await
                .map(|tip_response| tip_response.response.map(|balance| balance.balance))
        })
        .map_err(|_| OperationStatus::DynError)
}

pub type BalanceResult = ValueResult<Value, OperationStatus>;

/// Get the balance of a wallet address
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
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
    node: *const LogosBlockchainNode,
    wallet_address: *const u8,
    optional_tip: *const HeaderId,
) -> BalanceResult {
    if node.is_null() {
        log::error!("[get_balance] Received a null `node` pointer. Exiting.");
        return BalanceResult::from_error(OperationStatus::NullPointer);
    }
    if wallet_address.is_null() {
        log::error!("[get_balance] Received a null `wallet_address` pointer. Exiting.");
        return BalanceResult::from_error(OperationStatus::NullPointer);
    }

    let node = unsafe { &*node };
    let tip = if optional_tip.is_null() {
        match get_cryptarchia_info_sync(node) {
            Ok(cryptarchia_info) => cryptarchia_info.tip,
            Err(error) => return BalanceResult::from_error(error),
        }
    } else {
        lb_core::header::HeaderId::from(unsafe { *optional_tip })
    };
    let wallet_address_bytes = unsafe { std::slice::from_raw_parts(wallet_address, 32) };
    let wallet_address = match fr_from_bytes(wallet_address_bytes) {
        Ok(bytes) => ZkPublicKey::new(bytes),
        Err(e) => {
            log::error!("{e:?}");
            return BalanceResult::from_error(OperationStatus::DynError);
        }
    };

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
/// - `node`: A [`LogosBlockchainNode`] instance.
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
    node: &LogosBlockchainNode,
    tip: lb_core::header::HeaderId,
    change_public_key: ZkPublicKey,
    funding_public_keys: Vec<ZkPublicKey>,
    recipient_public_key: ZkPublicKey,
    amount: u64,
) -> Result<SignedMantleTx, OperationStatus> {
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        log::error!("[transfer_funds_sync] Failed to create tokio runtime. Aborting.");
        return Err(OperationStatus::RuntimeError);
    };

    runtime.block_on(async {
        let handle = node.get_overwatch_handle();
        let api = WalletApi::<WalletService<_, _, _, _, _>, _>::from_overwatch_handle(handle).await;

        // The following calls are a rough copy-pate of
        // `post_transactions_transfer_funds`. TODO: Abstract into a common API
        let signed_tx = api
            .transfer_funds(
                Some(tip),
                change_public_key,
                funding_public_keys,
                recipient_public_key,
                amount,
            )
            .await
            .inspect_err(|error| {
                log::error!("[transfer_funds_sync] Failed to transfer funds: {error}");
            })
            .map(|tip_response| tip_response.response)
            .map_err(|_| OperationStatus::DynError)?;

        if let Err(error) = mempool::add_tx(handle, signed_tx.clone(), Transaction::hash).await {
            log::error!("[transfer_funds_sync] Failed to add transaction to mempool: {error}");
            return Err(OperationStatus::DynError);
        }
        Ok(signed_tx)
    })
}

pub type TransferFundsResult = ValueResult<Hash, OperationStatus>;

/// Transfer funds from some addresses to another.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
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
    node: *const LogosBlockchainNode,
    arguments: *const TransferFundsArguments,
) -> TransferFundsResult {
    if node.is_null() {
        log::error!("[transfer_funds] Received a null `node` pointer. Exiting.");
        return TransferFundsResult::from_error(OperationStatus::NullPointer);
    }
    if arguments.is_null() {
        log::error!("[transfer_funds] Received a null `arguments` pointer. Exiting.");
        return TransferFundsResult::from_error(OperationStatus::NullPointer);
    }
    let arguments = unsafe { &*arguments };
    if let Err((error_message, status)) = unsafe { arguments.validate() } {
        log::error!("[transfer_funds] {error_message} Exiting.");
        return TransferFundsResult::from_error(status);
    }

    let node = unsafe { &*node };
    let tip = if arguments.optional_tip.is_null() {
        match get_cryptarchia_info_sync(node) {
            Ok(cryptarchia_info) => cryptarchia_info.tip,
            Err(status) => {
                log::error!("[transfer_funds] Failed to get cryptarchia info. Aborting.");
                return TransferFundsResult::from_error(status);
            }
        }
    } else {
        lb_core::header::HeaderId::from(unsafe { *arguments.optional_tip })
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
                log::error!(
                    "[transfer_funds] Failed to convert transaction hash to array. Exiting."
                );
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
