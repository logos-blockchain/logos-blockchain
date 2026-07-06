use core::ptr;
use std::time::Duration;

use lb_api_service::http::mempool;
use lb_core::{
    header::HeaderId as CoreHeaderId,
    mantle::{
        MantleTx, Note, NoteId as CoreNoteId, Op, OpProof, SignedMantleTx, Transaction,
        gas::GasCost,
        ledger::{Inputs, Outputs},
        ops::{
            channel::{
                ChannelId,
                deposit::{DepositOp, Metadata},
            },
            transfer::TransferOp,
        },
        transactions::MantleTxBuilder,
    },
};
use lb_groth16::{fr_from_bytes, fr_to_bytes};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_node::{
    RuntimeServiceId,
    generic_services::{CryptarchiaService, WalletService as NodeWalletService},
};
use lb_wallet_service::{ClaimableVoucherInfo, TipResponse, api::WalletApi};
use overwatch::services::status::ServiceStatus;

use crate::{
    LogosBlockchainNode,
    api::{
        cryptarchia::{Hash, HeaderId, NoteId, get_cryptarchia_info_sync},
        types::{
            claimable_vouchers::{ClaimableVoucher, ClaimableVouchers},
            known_addresses::KnownAddresses,
            value::Value,
            wallet_notes::{WalletNote, WalletNotes},
        },
    },
    errors::{OperationStatus, OperationStatusCode},
    result::{FfiStatusResult, StatusResult},
    return_error_if_null_pointer, unwrap_or_return_error,
};

pub type FfiClaimableVouchersResult = FfiStatusResult<ClaimableVouchers>;

type WalletService = NodeWalletService<CryptarchiaService<RuntimeServiceId>, RuntimeServiceId>;

/// Resolved tip plus the `(note ID, value)` pairs for a wallet address.
type WalletNotesData = (lb_core::header::HeaderId, Vec<(CoreNoteId, Value)>);

/// Gets the known wallet addresses from the wallet service.
///
/// This is a synchronous wrapper around [`WalletApi::get_known_addresses`].
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A [`Result`] containing a vector of [`ZkPublicKey`] on success, or an
/// [`OperationStatus`] error on failure.
pub(crate) fn get_known_addresses_sync(
    node: &LogosBlockchainNode,
) -> StatusResult<Vec<ZkPublicKey>> {
    let runtime_handle = node.get_runtime_handle();
    runtime_handle.block_on(async {
        let api = WalletApi::<WalletService, RuntimeServiceId>::from_overwatch_handle(
            node.get_overwatch_handle(),
        )
        .await;
        api.get_known_addresses()
            .await
            .map_err(|e| OperationStatus::error(OperationStatusCode::NotFound, format!("{e:?}")))
    })
}

pub type FfiKnownAddressesResult = FfiStatusResult<KnownAddresses>;

/// Retrieves the list of known wallet addresses from the Logos Blockchain node.
///
/// This function queries the wallet service for all known zero-knowledge public
/// keys (wallet addresses) and returns them as a C-compatible structure
/// containing an array of byte pointers.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance from
///   which the known addresses will be retrieved.
///
/// # Returns
///
/// Returns a [`FfiKnownAddressesResult`] containing:
/// - On success: A [`KnownAddresses`] struct with an array of pointers to
///   32-byte address representations and the array length.
/// - On failure: An [`OperationStatus`] error indicating the reason for
///   failure.
///
/// # Errors
///
/// This function returns an error in the following cases:
/// - [`OperationStatusCode::NullPointer`](OperationStatusCode::NullPointer) if
///   the `node` pointer is null.
/// - [`OperationStatusCode::NotFound`](OperationStatusCode::NotFound) if the
///   wallet addresses cannot be retrieved from the wallet service.
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
/// if (result.error.is_ok()) {
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
) -> FfiKnownAddressesResult {
    return_error_if_null_pointer!(node);

    let node = unsafe { &*node };
    let addresses = unwrap_or_return_error!(get_known_addresses_sync(node));

    let address_pointers: Vec<*mut u8> = addresses
        .into_iter()
        .map(|pk| {
            let bytes = fr_to_bytes(pk.as_fr());
            Box::into_raw(Box::new(bytes)).cast::<u8>()
        })
        .collect();
    let len = address_pointers.len();
    let addresses_ptr = Box::leak(address_pointers.into_boxed_slice()).as_mut_ptr();

    FfiKnownAddressesResult::ok(KnownAddresses {
        addresses: addresses_ptr,
        len,
    })
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
/// - `addresses`: A [`KnownAddresses`] structure previously returned by
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
/// if (result.error.is_ok()) {
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
pub unsafe extern "C" fn free_known_addresses(addresses: KnownAddresses) -> OperationStatus {
    return_error_if_null_pointer!(addresses.addresses);
    let address_pointers = unsafe {
        Box::from_raw(ptr::slice_from_raw_parts_mut(
            addresses.addresses,
            addresses.len,
        ))
    };
    for address_pointer in address_pointers {
        return_error_if_null_pointer!(address_pointer);
        unsafe { drop(Box::from_raw(address_pointer.cast::<[u8; 32]>())) };
    }
    OperationStatus::OK
}

/// Gets the claimable vouchers tracked by the wallet.
///
/// This is a synchronous wrapper around [`WalletApi::get_claimable_vouchers`].
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
/// - `tip`: The header ID to query claimable vouchers at.
///
/// # Returns
///
/// A [`Result`] containing the tip-scoped claimable vouchers on success, or an
/// [`OperationStatus`] error on failure.
pub(crate) fn get_claimable_vouchers_sync(
    node: &LogosBlockchainNode,
    tip: Option<CoreHeaderId>,
) -> StatusResult<TipResponse<Vec<ClaimableVoucherInfo>>> {
    let runtime_handle = node.get_runtime_handle();
    runtime_handle.block_on(async {
        if let Err(status) = node
            .get_overwatch_handle()
            .status_watcher::<WalletService>()
            .await
            .wait_for(ServiceStatus::Ready, Some(Duration::from_millis(100)))
            .await
        {
            return Err(OperationStatus::error(
                OperationStatusCode::ServiceError,
                format!("Wallet service is not ready: {status:?}"),
            ));
        }

        let api = WalletApi::<WalletService, RuntimeServiceId>::from_overwatch_handle(
            node.get_overwatch_handle(),
        )
        .await;
        api.get_claimable_vouchers(tip).await.map_err(|error| {
            OperationStatus::error(OperationStatusCode::DynError, format!("{error:?}"))
        })
    })
}

/// Gets the claimable vouchers tracked by the wallet.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
/// - `optional_tip`: An optional pointer to the header ID to query at. If null,
///   the current tip will be used.
///
/// # Returns
///
/// A [`FfiClaimableVouchersResult`] containing the tip and claimable vouchers
/// on success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all pointers are valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_claimable_vouchers(
    node: *const LogosBlockchainNode,
    optional_tip: *const HeaderId,
) -> FfiClaimableVouchersResult {
    return_error_if_null_pointer!(node);
    let node = unsafe { &*node };
    let tip = if optional_tip.is_null() {
        None
    } else {
        Some(CoreHeaderId::from(unsafe { *optional_tip }))
    };

    let response = unwrap_or_return_error!(get_claimable_vouchers_sync(node, tip));
    let vouchers: Vec<ClaimableVoucher> = response
        .response
        .into_iter()
        .map(|voucher| {
            let nullifier = voucher.nullifier.into();
            ClaimableVoucher {
                commitment: voucher.commitment.to_bytes(),
                nullifier: fr_to_bytes(&nullifier),
            }
        })
        .collect();

    let len = vouchers.len();
    let vouchers_ptr = Box::leak(vouchers.into_boxed_slice()).as_mut_ptr();

    FfiClaimableVouchersResult::ok(ClaimableVouchers {
        tip: response.tip.into(),
        vouchers: vouchers_ptr,
        len,
    })
}

/// Frees the memory allocated for a [`ClaimableVouchers`] structure.
///
/// # Arguments
///
/// - `vouchers`: A [`ClaimableVouchers`] structure previously returned by
///   [`get_claimable_vouchers`].
///
/// # Safety
///
/// This function is unsafe because it reconstructs a boxed slice from a raw
/// pointer. The caller must only pass values returned by
/// [`get_claimable_vouchers`] and must call this exactly once per result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_claimable_vouchers(vouchers: ClaimableVouchers) -> OperationStatus {
    return_error_if_null_pointer!(vouchers.vouchers);
    let vouchers = unsafe {
        Box::from_raw(ptr::slice_from_raw_parts_mut(
            vouchers.vouchers,
            vouchers.len,
        ))
    };

    drop(vouchers);
    OperationStatus::OK
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
) -> StatusResult<Option<Value>> {
    let runtime_handle = node.get_runtime_handle();
    runtime_handle
        .block_on(async {
            let api = WalletApi::<WalletService, RuntimeServiceId>::from_overwatch_handle(
                node.get_overwatch_handle(),
            )
            .await;
            api.get_balance(Some(tip), wallet_address)
                .await
                .map(|tip_response| tip_response.response.map(|balance| balance.balance))
        })
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::DynError,
                format!("Failed to get balance: {error}"),
            )
        })
}

pub type FfiBalanceResult = FfiStatusResult<Value>;

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
/// A [`FfiStatusResult`] containing the balance on success, or an
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
) -> FfiBalanceResult {
    return_error_if_null_pointer!(node);
    return_error_if_null_pointer!(wallet_address);
    let node = unsafe { &*node };
    let tip = if optional_tip.is_null() {
        unwrap_or_return_error!(get_cryptarchia_info_sync(node))
            .cryptarchia_info
            .tip
    } else {
        lb_core::header::HeaderId::from(unsafe { *optional_tip })
    };
    let wallet_address_bytes = unsafe { std::slice::from_raw_parts(wallet_address, 32) };
    let wallet_address = unwrap_or_return_error!(
        fr_from_bytes(wallet_address_bytes)
            .map(ZkPublicKey::new)
            .map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::DynError,
                    format!("Invalid wallet address: {error:?}"),
                )
            })
    );

    match get_balance_sync(node, tip, wallet_address) {
        Ok(Some(balance)) => FfiBalanceResult::ok(balance),
        Ok(None) => FfiBalanceResult::err(OperationStatus::error(
            OperationStatusCode::NotFound,
            "Unknown wallet address.",
        )),
        Err(status) => FfiBalanceResult::err(status),
    }
}

/// Gets the spendable notes (UTXOs) of a wallet address.
///
/// This is a synchronous wrapper around [`WalletApi::get_balance`] that, unlike
/// [`get_balance_sync`], preserves the individual notes rather than collapsing
/// them into a single total. Callers need the note IDs to build operations that
/// consume specific notes (e.g. [`channel_deposit_with_notes`]).
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
/// - `tip`: The header ID to query the notes at.
/// - `wallet_address`: The public key of the wallet address to query.
///
/// # Returns
///
/// A [`Result`] containing the resolved tip and the `(note ID, value)` pairs on
/// success (or `None` if the address is unknown), or an [`OperationStatus`]
/// error on failure.
pub(crate) fn get_wallet_notes_sync(
    node: &LogosBlockchainNode,
    tip: lb_core::header::HeaderId,
    wallet_address: ZkPublicKey,
) -> StatusResult<Option<WalletNotesData>> {
    let runtime_handle = node.get_runtime_handle();
    runtime_handle
        .block_on(async {
            let api = WalletApi::<WalletService, RuntimeServiceId>::from_overwatch_handle(
                node.get_overwatch_handle(),
            )
            .await;
            api.get_balance(Some(tip), wallet_address)
                .await
                .map(|tip_response| {
                    let resolved_tip = tip_response.tip;
                    tip_response
                        .response
                        .map(|balance| (resolved_tip, balance.notes.into_iter().collect()))
                })
        })
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::DynError,
                format!("Failed to get balance: {error}"),
            )
        })
}

pub type FfiWalletNotesResult = FfiStatusResult<WalletNotes>;

/// Retrieves the spendable notes (UTXOs) of a wallet address.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
/// - `wallet_address`: A non-null pointer to the 32-byte public key of the
///   wallet address to query.
/// - `optional_tip`: An optional pointer to the header ID to query at. If null,
///   the current tip will be used.
///
/// # Returns
///
/// A [`FfiWalletNotesResult`] containing the tip and the array of notes on
/// success, or an [`OperationStatus`] error on failure. Note IDs are in
/// little-endian format.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all pointers are valid, and must free the returned
/// [`WalletNotes`] with [`free_wallet_notes`] exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_wallet_notes(
    node: *const LogosBlockchainNode,
    wallet_address: *const u8,
    optional_tip: *const HeaderId,
) -> FfiWalletNotesResult {
    return_error_if_null_pointer!(node);
    return_error_if_null_pointer!(wallet_address);
    let node = unsafe { &*node };
    let tip = if optional_tip.is_null() {
        unwrap_or_return_error!(get_cryptarchia_info_sync(node))
            .cryptarchia_info
            .tip
    } else {
        lb_core::header::HeaderId::from(unsafe { *optional_tip })
    };
    let wallet_address_bytes = unsafe { std::slice::from_raw_parts(wallet_address, 32) };
    let wallet_address = unwrap_or_return_error!(
        fr_from_bytes(wallet_address_bytes)
            .map(ZkPublicKey::new)
            .map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::DynError,
                    format!("Invalid wallet address: {error:?}"),
                )
            })
    );

    match get_wallet_notes_sync(node, tip, wallet_address) {
        Ok(Some((resolved_tip, notes))) => {
            let notes: Vec<WalletNote> = notes
                .into_iter()
                .map(|(id, value)| WalletNote {
                    id: fr_to_bytes(id.as_fr()),
                    value,
                })
                .collect();
            let len = notes.len();
            let notes_ptr = Box::leak(notes.into_boxed_slice()).as_mut_ptr();
            FfiWalletNotesResult::ok(WalletNotes {
                tip: resolved_tip.into(),
                notes: notes_ptr,
                len,
            })
        }
        Ok(None) => FfiWalletNotesResult::err(OperationStatus::error(
            OperationStatusCode::NotFound,
            "Unknown wallet address.",
        )),
        Err(status) => FfiWalletNotesResult::err(status),
    }
}

/// Frees the memory allocated for a [`WalletNotes`] structure.
///
/// # Safety
///
/// This function is unsafe because it reconstructs a boxed slice from a raw
/// pointer. The caller must only pass values returned by [`get_wallet_notes`]
/// and must call this exactly once per result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_wallet_notes(notes: WalletNotes) -> OperationStatus {
    if notes.notes.is_null() {
        return OperationStatus::OK;
    }
    let notes = unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(notes.notes, notes.len)) };
    drop(notes);
    OperationStatus::OK
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
    /// A `Result` indicating success or containing an [`OperationStatus`]
    /// error describing the first invalid argument found.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it dereferences raw pointers. The caller
    /// must ensure that all pointers are valid.
    pub unsafe fn validate(&self) -> Result<(), OperationStatus> {
        if self.change_public_key.is_null() {
            return Err(OperationStatus::error(
                OperationStatusCode::NullPointer,
                "TransferFunds contains a null `change_public_key` pointer.",
            ));
        }
        if self.funding_public_keys.is_null() {
            return Err(OperationStatus::error(
                OperationStatusCode::NullPointer,
                "TransferFunds contains a null `funding_public_keys` pointer.",
            ));
        }

        for i in 0..self.funding_public_keys_len {
            let funding_public_key_pointer = unsafe { self.funding_public_keys.add(i) };
            let funding_public_key = unsafe { *funding_public_key_pointer };
            if funding_public_key.is_null() {
                return Err(OperationStatus::error(
                    OperationStatusCode::NullPointer,
                    format!("TransferFunds contains a null pointer at `funding_public_keys[{i}]`."),
                ));
            }
        }

        if self.recipient_public_key.is_null() {
            return Err(OperationStatus::error(
                OperationStatusCode::NullPointer,
                "TransferFunds contains a null `recipient_public_key` pointer.",
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
) -> StatusResult<SignedMantleTx> {
    let runtime_handle = node.get_runtime_handle();
    runtime_handle.block_on(async {
        let handle = node.get_overwatch_handle();
        let api = WalletApi::<WalletService, RuntimeServiceId>::from_overwatch_handle(handle).await;

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
            .map(|tip_response| tip_response.response)
            .map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::DynError,
                    format!("Failed to transfer funds: {error}"),
                )
            })?;

        if let Err(error) = mempool::add_tx(handle, signed_tx.clone(), Transaction::hash).await {
            return Err(OperationStatus::error(
                OperationStatusCode::DynError,
                format!("Failed to add transaction to mempool: {error}"),
            ));
        }
        Ok(signed_tx)
    })
}

pub type FfiTransferFundsResult = FfiStatusResult<Hash>;

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
/// A [`FfiTransferFundsResult`] containing the transaction [`Hash`] on success,
/// or an [`OperationStatus`] error on failure. The hash is in little-endian
/// format.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all pointers are valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn transfer_funds(
    node: *const LogosBlockchainNode,
    arguments: *const TransferFundsArguments,
) -> FfiTransferFundsResult {
    return_error_if_null_pointer!(node);
    return_error_if_null_pointer!(arguments);
    let arguments = unsafe { &*arguments };
    unwrap_or_return_error!(unsafe { arguments.validate() });

    let node = unsafe { &*node };
    let tip = if arguments.optional_tip.is_null() {
        unwrap_or_return_error!(get_cryptarchia_info_sync(node))
            .cryptarchia_info
            .tip
    } else {
        lb_core::header::HeaderId::from(unsafe { *arguments.optional_tip })
    };
    let change_public_key =
        unwrap_or_return_error!(unsafe { parse_public_key(arguments.change_public_key) });
    let funding_public_keys = {
        let funding_public_keys_pointers = unsafe {
            std::slice::from_raw_parts(
                arguments.funding_public_keys,
                arguments.funding_public_keys_len,
            )
        };
        unwrap_or_return_error!(
            funding_public_keys_pointers
                .iter()
                .map(|funding_public_key_pointer| unsafe {
                    parse_public_key(*funding_public_key_pointer)
                })
                .collect::<StatusResult<Vec<_>>>()
        )
    };
    let recipient_public_key =
        unwrap_or_return_error!(unsafe { parse_public_key(arguments.recipient_public_key) });
    let amount = Value::from(arguments.amount);

    let transaction = unwrap_or_return_error!(transfer_funds_sync(
        node,
        tip,
        change_public_key,
        funding_public_keys,
        recipient_public_key,
        amount,
    ));
    let transaction_hash = transaction.hash().as_signing_bytes();
    let Ok(transaction_hash_array) = transaction_hash.iter().as_slice().try_into() else {
        return FfiTransferFundsResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            "Failed to convert transaction hash to array.",
        ));
    };
    FfiTransferFundsResult::ok(transaction_hash_array)
}

/// Parses a 32-byte little-endian buffer into a [`ZkPublicKey`].
///
/// Validates that the bytes encode a canonical field element (rather than
/// silently reducing modulo the field order), matching the parsing used by
/// [`get_balance`] and [`get_wallet_notes`].
///
/// # Safety
///
/// `pointer` must be non-null and point to at least 32 readable bytes.
unsafe fn parse_public_key(pointer: *const u8) -> StatusResult<ZkPublicKey> {
    let bytes = unsafe { std::slice::from_raw_parts(pointer, 32) };
    fr_from_bytes(bytes).map(ZkPublicKey::new).map_err(|error| {
        OperationStatus::error(
            OperationStatusCode::DynError,
            format!("Invalid public key: {error:?}"),
        )
    })
}

pub type FfiChannelDepositResult = FfiStatusResult<Hash>;

/// Arguments for [`channel_deposit_with_notes`].
///
/// Mirrors the node's `POST /channel/deposit` request body: the caller supplies
/// the exact notes to deposit (their whole value enters the channel), so
/// callers must select notes themselves (see [`get_wallet_notes`]).
#[repr(C)]
pub struct ChannelDepositWithNotesArguments {
    /// Optional pointer to the header ID to build against. If null, the current
    /// tip is used.
    pub optional_tip: *const HeaderId,
    /// The 32-byte channel ID to deposit into.
    pub channel_id: *const u8,
    /// Array of note IDs to deposit. Their combined value is the deposited
    /// amount.
    pub input_note_ids: *const NoteId,
    pub input_note_ids_len: usize,
    /// Optional metadata bytes attached to the deposit. May be null iff
    /// `metadata_len` is 0.
    pub metadata: *const u8,
    pub metadata_len: usize,
    /// The 32-byte public key to receive any change from funding the gas fee.
    pub change_public_key: *const u8,
    /// Array of pointers to 32-byte public keys used to fund the gas fee.
    pub funding_public_keys: *const *const u8,
    pub funding_public_keys_len: usize,
    /// The maximum gas fee the caller is willing to pay. The deposit is
    /// rejected if the computed fee exceeds this.
    pub max_tx_fee: u64,
}

impl ChannelDepositWithNotesArguments {
    /// Validates that all required pointers are non-null.
    ///
    /// # Safety
    ///
    /// This function dereferences raw pointers. The caller must ensure the
    /// pointer-array lengths are accurate.
    pub unsafe fn validate(&self) -> Result<(), OperationStatus> {
        if self.channel_id.is_null() {
            return Err(OperationStatus::error(
                OperationStatusCode::NullPointer,
                "ChannelDeposit contains a null `channel_id` pointer.",
            ));
        }
        if self.input_note_ids.is_null() {
            return Err(OperationStatus::error(
                OperationStatusCode::NullPointer,
                "ChannelDeposit contains a null `input_note_ids` pointer.",
            ));
        }
        if self.input_note_ids_len == 0 {
            return Err(OperationStatus::error(
                OperationStatusCode::RuntimeError,
                "ChannelDeposit requires at least one input note.",
            ));
        }
        if self.metadata.is_null() && self.metadata_len != 0 {
            return Err(OperationStatus::error(
                OperationStatusCode::NullPointer,
                "ChannelDeposit contains a null `metadata` pointer with non-zero length.",
            ));
        }
        if self.change_public_key.is_null() {
            return Err(OperationStatus::error(
                OperationStatusCode::NullPointer,
                "ChannelDeposit contains a null `change_public_key` pointer.",
            ));
        }
        if self.funding_public_keys.is_null() {
            return Err(OperationStatus::error(
                OperationStatusCode::NullPointer,
                "ChannelDeposit contains a null `funding_public_keys` pointer.",
            ));
        }
        for i in 0..self.funding_public_keys_len {
            let element_pointer = unsafe { self.funding_public_keys.add(i) };
            let funding_public_key_pointer = unsafe { *element_pointer };
            if funding_public_key_pointer.is_null() {
                return Err(OperationStatus::error(
                    OperationStatusCode::NullPointer,
                    format!(
                        "ChannelDeposit contains a null pointer at `funding_public_keys[{i}]`."
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// Builds, funds, signs and submits a channel deposit consuming the given
/// notes.
///
/// This is a synchronous wrapper that mirrors the node's `channel_deposit` HTTP
/// handler: it builds a [`DepositOp`], funds the gas fee from
/// `funding_public_keys` (change to `change_public_key`), rejects the deposit
/// if the fee exceeds `max_tx_fee`, signs the transaction and adds it to the
/// mempool.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
/// - `tip`: Optional header ID to build against (`None` uses the current tip).
/// - `deposit`: The fully-formed [`DepositOp`].
/// - `change_public_key`: Receives any change from funding the fee.
/// - `funding_public_keys`: Fund the gas fee.
/// - `max_tx_fee`: Maximum acceptable gas fee.
///
/// # Returns
///
/// A [`Result`] containing the submitted [`SignedMantleTx`] on success, or an
/// [`OperationStatus`] error on failure.
pub(crate) fn channel_deposit_with_notes_sync(
    node: &LogosBlockchainNode,
    tip: lb_core::header::HeaderId,
    deposit: DepositOp,
    change_public_key: ZkPublicKey,
    funding_public_keys: Vec<ZkPublicKey>,
    max_tx_fee: GasCost,
) -> StatusResult<SignedMantleTx> {
    let runtime_handle = node.get_runtime_handle();
    runtime_handle.block_on(async {
        let handle = node.get_overwatch_handle();
        let api = WalletApi::<WalletService, RuntimeServiceId>::from_overwatch_handle(handle).await;

        // Mirrors the node's `channel_deposit` handler: build -> fund -> check
        // fee -> sign -> submit.
        let tx_builder = MantleTxBuilder::new()
            .push_op(Op::ChannelDeposit(deposit))
            .map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::DynError,
                    format!("Failed to push deposit op: {error}"),
                )
            })?;
        let TipResponse {
            tip: funded_tip,
            response: funded_tx_builder,
        } = api
            .fund_tx(
                Some(tip),
                tx_builder,
                change_public_key,
                funding_public_keys,
            )
            .await
            .map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::DynError,
                    format!("Failed to fund tx: {error}"),
                )
            })?;

        let tx_fee = funded_tx_builder.tx_fee().map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::DynError,
                format!("Failed to compute tx fee: {error}"),
            )
        })?;
        if tx_fee > max_tx_fee {
            return Err(OperationStatus::error(
                OperationStatusCode::DynError,
                format!("tx_fee({tx_fee}) exceeds max_tx_fee({max_tx_fee})"),
            ));
        }

        let signed_tx = api
            .sign_tx(Some(funded_tip), funded_tx_builder)
            .await
            .map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::DynError,
                    format!("Failed to sign tx: {error}"),
                )
            })?
            .response;

        if let Err(error) = mempool::add_tx(handle, signed_tx.clone(), Transaction::hash).await {
            return Err(OperationStatus::error(
                OperationStatusCode::DynError,
                format!("Failed to add transaction to mempool: {error}"),
            ));
        }
        Ok(signed_tx)
    })
}

/// Submits a channel deposit consuming caller-specified notes.
///
/// The whole value of each supplied note enters the channel, so the deposited
/// amount equals the sum of the notes' values. Callers that want to deposit an
/// arbitrary amount should use the amount-based variant instead.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
/// - `arguments`: A non-null pointer to a [`ChannelDepositWithNotesArguments`].
///
/// # Returns
///
/// A [`FfiChannelDepositResult`] containing the submitted transaction [`Hash`]
/// (little-endian) on success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all pointers are valid and that the array lengths are
/// accurate.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn channel_deposit_with_notes(
    node: *const LogosBlockchainNode,
    arguments: *const ChannelDepositWithNotesArguments,
) -> FfiChannelDepositResult {
    return_error_if_null_pointer!(node);
    return_error_if_null_pointer!(arguments);
    let arguments = unsafe { &*arguments };
    unwrap_or_return_error!(unsafe { arguments.validate() });

    let node = unsafe { &*node };
    let tip = if arguments.optional_tip.is_null() {
        unwrap_or_return_error!(get_cryptarchia_info_sync(node))
            .cryptarchia_info
            .tip
    } else {
        CoreHeaderId::from(unsafe { *arguments.optional_tip })
    };

    let channel_id = {
        let bytes = unsafe { std::slice::from_raw_parts(arguments.channel_id, 32) };
        let Ok(array): Result<[u8; 32], _> = bytes.try_into() else {
            return FfiChannelDepositResult::err(OperationStatus::error(
                OperationStatusCode::RuntimeError,
                "Invalid channel_id length.",
            ));
        };
        ChannelId::from(array)
    };

    let input_note_ids = unsafe {
        std::slice::from_raw_parts(arguments.input_note_ids, arguments.input_note_ids_len)
    };
    let mut note_ids = Vec::with_capacity(arguments.input_note_ids_len);
    for note_id_bytes in input_note_ids {
        let note_id =
            unwrap_or_return_error!(fr_from_bytes(note_id_bytes).map(CoreNoteId).map_err(
                |error| {
                    OperationStatus::error(
                        OperationStatusCode::DynError,
                        format!("Invalid note ID: {error:?}"),
                    )
                }
            ));
        note_ids.push(note_id);
    }
    let inputs = unwrap_or_return_error!(Inputs::try_new(note_ids).map_err(|error| {
        OperationStatus::error(
            OperationStatusCode::DynError,
            format!("Invalid deposit inputs: {error:?}"),
        )
    }));

    let metadata_bytes = if arguments.metadata_len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(arguments.metadata, arguments.metadata_len) }.to_vec()
    };
    let metadata = unwrap_or_return_error!(Metadata::try_from(metadata_bytes).map_err(|error| {
        OperationStatus::error(
            OperationStatusCode::DynError,
            format!("Invalid metadata: {error:?}"),
        )
    }));

    let deposit = DepositOp {
        channel_id,
        inputs,
        metadata,
    };

    let change_public_key =
        unwrap_or_return_error!(unsafe { parse_public_key(arguments.change_public_key) });
    let funding_public_key_pointers = unsafe {
        std::slice::from_raw_parts(
            arguments.funding_public_keys,
            arguments.funding_public_keys_len,
        )
    };
    let funding_public_keys = unwrap_or_return_error!(
        funding_public_key_pointers
            .iter()
            .map(|pointer| unsafe { parse_public_key(*pointer) })
            .collect::<StatusResult<Vec<_>>>()
    );
    let max_tx_fee = GasCost::new(arguments.max_tx_fee);

    let transaction = unwrap_or_return_error!(channel_deposit_with_notes_sync(
        node,
        tip,
        deposit,
        change_public_key,
        funding_public_keys,
        max_tx_fee,
    ));
    let transaction_hash = transaction.hash().as_signing_bytes();
    let Ok(transaction_hash_array) = transaction_hash.iter().as_slice().try_into() else {
        return FfiChannelDepositResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            "Failed to convert transaction hash to array.",
        ));
    };
    FfiChannelDepositResult::ok(transaction_hash_array)
}

/// Selects notes (largest-first) whose combined value covers `amount`.
///
/// Returns the selected note IDs and their combined value, or `None` if the
/// supplied notes cannot cover `amount`.
fn select_notes_covering(
    mut notes: Vec<(CoreNoteId, Value)>,
    amount: Value,
) -> Option<(Vec<CoreNoteId>, Value)> {
    notes.sort_by_key(|(_, value)| std::cmp::Reverse(*value));
    let mut selected = Vec::new();
    let mut total: Value = 0;
    for (id, value) in notes {
        if total >= amount {
            break;
        }
        selected.push(id);
        total = total.saturating_add(value);
    }
    (total >= amount).then_some((selected, total))
}

/// Arguments for [`channel_deposit`].
///
/// The binding selects funding notes itself and, when no exact-value note
/// exists, splits one via a transfer so the channel receives exactly `amount`.
/// The `funding_public_key` owns the funding notes, the deposit note and any
/// change.
#[repr(C)]
pub struct ChannelDepositArguments {
    /// Optional pointer to the header ID to build against. If null, the current
    /// tip is used.
    pub optional_tip: *const HeaderId,
    /// The 32-byte channel ID to deposit into.
    pub channel_id: *const u8,
    /// The 32-byte public key funding the deposit (owns the funding notes, the
    /// deposit note and any change).
    pub funding_public_key: *const u8,
    /// The exact amount to deposit into the channel.
    pub amount: u64,
    /// Optional metadata bytes attached to the deposit. May be null iff
    /// `metadata_len` is 0.
    pub metadata: *const u8,
    pub metadata_len: usize,
}

impl ChannelDepositArguments {
    /// Validates that all required pointers are non-null.
    ///
    /// # Safety
    ///
    /// This function dereferences raw pointers.
    pub unsafe fn validate(&self) -> Result<(), OperationStatus> {
        if self.channel_id.is_null() {
            return Err(OperationStatus::error(
                OperationStatusCode::NullPointer,
                "ChannelDeposit contains a null `channel_id` pointer.",
            ));
        }
        if self.funding_public_key.is_null() {
            return Err(OperationStatus::error(
                OperationStatusCode::NullPointer,
                "ChannelDeposit contains a null `funding_public_key` pointer.",
            ));
        }
        if self.metadata.is_null() && self.metadata_len != 0 {
            return Err(OperationStatus::error(
                OperationStatusCode::NullPointer,
                "ChannelDeposit contains a null `metadata` pointer with non-zero length.",
            ));
        }
        if self.amount == 0 {
            return Err(OperationStatus::error(
                OperationStatusCode::RuntimeError,
                "ChannelDeposit `amount` must be greater than zero.",
            ));
        }
        Ok(())
    }
}

/// Builds, signs and submits an amount-based channel deposit, splitting a note
/// via a transfer when no exact-value note is available.
///
/// This mirrors the atomic deposit path used in the e2e tests: a [`TransferOp`]
/// produces the exact deposit note (plus change), the [`DepositOp`] consumes
/// that freshly-created note, and both ops are signed with a single ZK
/// signature over the transaction by `funding_public_key` (which owns every
/// input). Gas is not funded because the platform gas price is currently 0.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
/// - `tip`: The header ID to query notes and sign against.
/// - `channel_id`: The channel to deposit into.
/// - `funding_public_key`: Owns the funding notes, deposit note and change.
/// - `amount`: The exact amount to deposit.
/// - `metadata`: Metadata attached to the deposit.
///
/// # Returns
///
/// A [`Result`] containing the submitted [`SignedMantleTx`] on success, or an
/// [`OperationStatus`] error on failure.
pub(crate) fn channel_deposit_sync(
    node: &LogosBlockchainNode,
    tip: lb_core::header::HeaderId,
    channel_id: ChannelId,
    funding_public_key: ZkPublicKey,
    amount: Value,
    metadata: Metadata,
) -> StatusResult<SignedMantleTx> {
    let runtime_handle = node.get_runtime_handle();
    runtime_handle.block_on(async {
        let handle = node.get_overwatch_handle();
        let api = WalletApi::<WalletService, RuntimeServiceId>::from_overwatch_handle(handle).await;

        // 1. Fetch the funding key's spendable notes.
        let balance = api
            .get_balance(Some(tip), funding_public_key)
            .await
            .map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::DynError,
                    format!("Failed to get balance: {error}"),
                )
            })?;
        let notes: Vec<(CoreNoteId, Value)> = balance
            .response
            .map(|balance| balance.notes.into_iter().collect())
            .ok_or_else(|| {
                OperationStatus::error(OperationStatusCode::NotFound, "Unknown funding address.")
            })?;

        // 2. Select notes covering the deposit amount.
        let (selected_ids, selected_total) =
            select_notes_covering(notes, amount).ok_or_else(|| {
                OperationStatus::error(
                    OperationStatusCode::DynError,
                    "Insufficient funds to cover deposit amount.",
                )
            })?;
        let change = selected_total - amount;

        // 3. Build a transfer whose output[0] is the exact deposit note, with any
        //    remainder returned as change to the same key.
        let mut outputs = vec![Note::new(amount, funding_public_key)];
        if change > 0 {
            outputs.push(Note::new(change, funding_public_key));
        }
        let transfer = TransferOp {
            inputs: Inputs::try_new(selected_ids).map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::DynError,
                    format!("Invalid transfer inputs: {error:?}"),
                )
            })?,
            outputs: Outputs::try_new(outputs).map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::DynError,
                    format!("Invalid transfer outputs: {error:?}"),
                )
            })?,
        };

        // 4. The deposit consumes the freshly-created note at output index 0.
        let deposit_note_id = transfer
            .outputs
            .utxo_by_index(0, &transfer)
            .ok_or_else(|| {
                OperationStatus::error(
                    OperationStatusCode::RuntimeError,
                    "Transfer did not produce a deposit note.",
                )
            })?
            .id();
        let deposit = DepositOp {
            channel_id,
            inputs: Inputs::new([deposit_note_id]),
            metadata,
        };

        // 5. Assemble [transfer, deposit] in order and sign both ops with a single ZK
        //    signature by the funding key (which owns every input).
        //
        //    NOTE: we deliberately sign with `sign_tx_with_zk` (explicit keys) rather
        //    than the usual `WalletApi::sign_tx`. `sign_tx` resolves each op's input
        //    public keys from the *committed* ledger state, but the deposit consumes
        //    the note this same transaction's transfer creates (it is not on-chain
        //    yet), so `sign_tx` would fail with `MissingInputNote`. Both the transfer
        //    inputs and the deposit's input note are owned by `funding_public_key`, so
        //    one signature over the tx hash satisfies both op proofs. Do not "simplify"
        //    this to `sign_tx`.
        let tx = MantleTx([Op::Transfer(transfer), Op::ChannelDeposit(deposit)].into());
        let tx_hash = tx.hash();
        let user_sig = api
            .sign_tx_with_zk(tx_hash, vec![funding_public_key])
            .await
            .map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::DynError,
                    format!("Failed to sign deposit tx: {error}"),
                )
            })?;
        let signed_tx = SignedMantleTx::new(
            tx,
            vec![OpProof::ZkSig(user_sig.clone()), OpProof::ZkSig(user_sig)],
        )
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::DynError,
                format!("Failed to assemble signed tx: {error:?}"),
            )
        })?;

        // 6. Submit to the mempool.
        if let Err(error) = mempool::add_tx(handle, signed_tx.clone(), Transaction::hash).await {
            return Err(OperationStatus::error(
                OperationStatusCode::DynError,
                format!("Failed to add transaction to mempool: {error}"),
            ));
        }
        Ok(signed_tx)
    })
}

/// Submits an amount-based channel deposit.
///
/// The binding selects the funding notes itself and splits one via a transfer
/// when no exact-value note exists, so the channel receives exactly `amount`.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
/// - `arguments`: A non-null pointer to a [`ChannelDepositArguments`].
///
/// # Returns
///
/// A [`FfiChannelDepositResult`] containing the submitted transaction [`Hash`]
/// (little-endian) on success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all pointers are valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn channel_deposit(
    node: *const LogosBlockchainNode,
    arguments: *const ChannelDepositArguments,
) -> FfiChannelDepositResult {
    return_error_if_null_pointer!(node);
    return_error_if_null_pointer!(arguments);
    let arguments = unsafe { &*arguments };
    unwrap_or_return_error!(unsafe { arguments.validate() });

    let node = unsafe { &*node };
    let tip = if arguments.optional_tip.is_null() {
        unwrap_or_return_error!(get_cryptarchia_info_sync(node))
            .cryptarchia_info
            .tip
    } else {
        lb_core::header::HeaderId::from(unsafe { *arguments.optional_tip })
    };

    let channel_id = {
        let bytes = unsafe { std::slice::from_raw_parts(arguments.channel_id, 32) };
        let Ok(array): Result<[u8; 32], _> = bytes.try_into() else {
            return FfiChannelDepositResult::err(OperationStatus::error(
                OperationStatusCode::RuntimeError,
                "Invalid channel_id length.",
            ));
        };
        ChannelId::from(array)
    };
    let funding_public_key =
        unwrap_or_return_error!(unsafe { parse_public_key(arguments.funding_public_key) });
    let amount = Value::from(arguments.amount);

    let metadata_bytes = if arguments.metadata_len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(arguments.metadata, arguments.metadata_len) }.to_vec()
    };
    let metadata = unwrap_or_return_error!(Metadata::try_from(metadata_bytes).map_err(|error| {
        OperationStatus::error(
            OperationStatusCode::DynError,
            format!("Invalid metadata: {error:?}"),
        )
    }));

    let transaction = unwrap_or_return_error!(channel_deposit_sync(
        node,
        tip,
        channel_id,
        funding_public_key,
        amount,
        metadata,
    ));
    let transaction_hash = transaction.hash().as_signing_bytes();
    let Ok(transaction_hash_array) = transaction_hash.iter().as_slice().try_into() else {
        return FfiChannelDepositResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            "Failed to convert transaction hash to array.",
        ));
    };
    FfiChannelDepositResult::ok(transaction_hash_array)
}
