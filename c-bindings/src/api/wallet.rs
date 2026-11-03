use core::ptr;
use std::ffi::{CStr, CString, c_char};

use lb_api_service::http::mempool;
use lb_c_macros::panic_to_error;
use lb_core::{
    header::HeaderId as CoreHeaderId,
    mantle::{
        Note, Op, SignedOps,
        ledger::verification_mode::StandardMode,
        notes::NoteCommitment as CoreNoteCommitment,
        ops::channel::{
            ChannelId,
            deposit::{DepositOp, Metadata},
        },
        traits::Hashable,
        transactions::{
            MantleTxBuilder,
            builder::ZkTransferOpening,
            states::{Preverified, Unverified},
        },
    },
};
use lb_groth16::{fr_from_bytes, fr_from_mod_bytes, fr_to_bytes};
use lb_http_api_common::bodies::wallet::fund::{WalletFundRequestBody, WalletFundResponseBody};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_node::{
    RuntimeServiceId,
    generic_services::{
        CryptarchiaService, WalletService as NodeWalletService, sdp::wallet::split_root,
    },
};
use lb_wallet_service::{LeaderAgedNotesInfo, TipResponse, api::WalletApi};
use rand::{RngCore as _, rngs::OsRng};

use crate::{
    LogosBlockchainNode,
    api::{
        cryptarchia::{Hash, HeaderId, get_cryptarchia_info_sync},
        types::{
            known_addresses::KnownAddresses,
            leader_aged_notes::{LeaderAgedNote, LeaderAgedNotes},
            value::Value,
            wallet_notes::{WalletNote, WalletNotes},
        },
    },
    errors::{OperationStatus, OperationStatusCode},
    result::{FfiStatusResult, StatusResult},
    return_error_if_null_pointer, unwrap_or_return_error,
};

type WalletService = NodeWalletService<CryptarchiaService<RuntimeServiceId>, RuntimeServiceId>;

/// Resolved tip plus the `(note commitment, value)` pairs for a wallet address.
type WalletNotesData = (lb_core::header::HeaderId, Vec<(CoreNoteCommitment, Value)>);

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
    let runtime_handle = node.get_runtime_handle()?;
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
#[panic_to_error]
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
///
/// The value must be passed back exactly as it was returned: its pointer and
/// length decide what is freed.
#[panic_to_error]
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
        // A null entry owns nothing. Skipping it rather than returning keeps
        // the entries after it from leaking.
        if address_pointer.is_null() {
            continue;
        }
        unsafe { drop(Box::from_raw(address_pointer.cast::<[u8; 32]>())) };
    }
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
    let runtime_handle = node.get_runtime_handle()?;
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
/// - `wallet_address`: A non-null pointer to the 32-byte public key of the
///   wallet address to query.
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
#[panic_to_error]
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
/// them into a single total.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
/// - `tip`: The header ID to query the notes at.
/// - `wallet_address`: The public key of the wallet address to query.
///
/// # Returns
///
/// A [`Result`] containing the resolved tip and the `(note commitment, value)`
/// pairs on success (or `None` if the address is unknown), or an
/// [`OperationStatus`] error on failure.
pub(crate) fn get_wallet_notes_sync(
    node: &LogosBlockchainNode,
    tip: lb_core::header::HeaderId,
    wallet_address: ZkPublicKey,
) -> StatusResult<Option<WalletNotesData>> {
    let runtime_handle = node.get_runtime_handle()?;
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
/// success, or an [`OperationStatus`] error on failure. Note commitments are in
/// little-endian format.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all pointers are valid, and must free the returned
/// [`WalletNotes`] with [`free_wallet_notes`] exactly once.
#[panic_to_error]
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
                .map(|(commitment, value)| WalletNote {
                    commitment: fr_to_bytes(&commitment.0),
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

/// Gets the wallet notes that are aged enough to take part in the leadership
/// lottery.
///
/// This is a synchronous wrapper around
/// [`WalletApi::get_leader_aged_notes_info`].
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
/// - `tip`: The header ID to query at, or `None` for the current tip.
///
/// # Returns
///
/// A [`Result`] containing the resolved tip and the eligible UTXOs on success,
/// or an [`OperationStatus`] error on failure.
pub(crate) fn get_leader_aged_notes_sync(
    node: &LogosBlockchainNode,
    tip: Option<CoreHeaderId>,
) -> StatusResult<TipResponse<LeaderAgedNotesInfo>> {
    node.get_runtime_handle()?.block_on(async {
        let api = WalletApi::<WalletService, RuntimeServiceId>::from_overwatch_handle(
            node.get_overwatch_handle(),
        )
        .await;
        api.get_leader_aged_notes_info(tip).await.map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::DynError,
                format!("Failed to get leader aged notes: {error:?}"),
            )
        })
    })
}

pub type FfiLeaderAgedNotesResult = FfiStatusResult<LeaderAgedNotes>;

/// Reports which of the wallet's notes are old enough to take part in the
/// leadership lottery, i.e. whether this node can currently win a slot.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
/// - `optional_tip`: An optional pointer to the header ID to query at. If null,
///   the current tip is used.
///
/// # Returns
///
/// A [`FfiLeaderAgedNotesResult`] containing the tip and the eligible notes on
/// success, or an [`OperationStatus`] error on failure. A successful result
/// with `len == 0` means the node has no eligible notes at that tip. Note
/// commitments and public keys are in little-endian format.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all pointers are valid, and must free the returned
/// [`LeaderAgedNotes`] with [`free_leader_aged_notes`] exactly once.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_leader_aged_notes(
    node: *const LogosBlockchainNode,
    optional_tip: *const HeaderId,
) -> FfiLeaderAgedNotesResult {
    return_error_if_null_pointer!(node);
    let node = unsafe { &*node };
    let tip = if optional_tip.is_null() {
        None
    } else {
        Some(CoreHeaderId::from(unsafe { *optional_tip }))
    };

    let TipResponse { tip, response } =
        unwrap_or_return_error!(get_leader_aged_notes_sync(node, tip));

    let notes: Vec<LeaderAgedNote> = response
        .notes
        .into_iter()
        .map(|note| LeaderAgedNote {
            commitment: fr_to_bytes(&note.commitment.0),
            note_set: note.note_set,
            value: note.value,
            public_key: fr_to_bytes(&note.public_key.into()),
        })
        .collect();

    let len = notes.len();
    let notes_ptr = Box::leak(notes.into_boxed_slice()).as_mut_ptr();

    FfiLeaderAgedNotesResult::ok(LeaderAgedNotes {
        tip: tip.into(),
        notes: notes_ptr,
        len,
        total_value: response.total_value,
    })
}

/// Frees the memory allocated for a [`LeaderAgedNotes`] structure.
///
/// # Safety
///
/// This function is unsafe because it reconstructs a boxed slice from a raw
/// pointer. The caller must only pass values returned by
/// [`get_leader_aged_notes`] and must call this exactly once per result.
///
/// The value must be passed back exactly as it was returned: its pointer and
/// length decide what is freed.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_leader_aged_notes(notes: LeaderAgedNotes) -> OperationStatus {
    return_error_if_null_pointer!(notes.notes);
    let notes = unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(notes.notes, notes.len)) };
    drop(notes);
    OperationStatus::OK
}

/// Frees the memory allocated for a [`WalletNotes`] structure.
///
/// # Safety
///
/// This function is unsafe because it reconstructs a boxed slice from a raw
/// pointer. The caller must only pass values returned by [`get_wallet_notes`]
/// and must call this exactly once per result.
///
/// The value must be passed back exactly as it was returned: its pointer and
/// length decide what is freed.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_wallet_notes(notes: WalletNotes) -> OperationStatus {
    return_error_if_null_pointer!(notes.notes);
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
/// A `Result` containing a [`SignedOps`] and the recipient [`Note`] it creates
/// on success, or an [`OperationStatus`] error on failure.
pub(crate) fn transfer_funds_sync(
    node: &LogosBlockchainNode,
    tip: lb_core::header::HeaderId,
    change_public_key: ZkPublicKey,
    funding_public_keys: Vec<ZkPublicKey>,
    recipient_public_key: ZkPublicKey,
    amount: u64,
) -> StatusResult<(SignedOps<Preverified, StandardMode>, Note)> {
    let runtime_handle = node.get_runtime_handle()?;
    runtime_handle.block_on(async {
        let handle = node.get_overwatch_handle();
        let api = WalletApi::<WalletService, RuntimeServiceId>::from_overwatch_handle(handle).await;

        // The following calls are a rough copy-pate of
        // `post_transactions_transfer_funds`. TODO: Abstract into a common API
        // The created recipient note is returned with the transfer: only the
        // wallet's own recipient keys adopt it, another recipient imports it.
        let (signed_tx, recipient_note) = api
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

        if let Err(error) = mempool::add_tx(handle, signed_tx.clone(), Hashable::hash).await {
            return Err(OperationStatus::error(
                OperationStatusCode::DynError,
                format!("Failed to add transaction to mempool: {error}"),
            ));
        }
        Ok((signed_tx, recipient_note))
    })
}

/// A submitted transfer and the nonce of the note it creates for the
/// recipient, who needs it, with the amount and its public key, to import
/// the note.
#[repr(C)]
#[derive(Default)]
pub struct TransferFunds {
    /// The transaction hash, in little-endian format.
    pub hash: Hash,
    /// The recipient note's nonce, a little-endian field element.
    pub recipient_note_nonce: Hash,
}

pub type FfiTransferFundsResult = FfiStatusResult<TransferFunds>;

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
/// A [`FfiTransferFundsResult`] containing the transaction [`Hash`] and the
/// recipient note's nonce on success, or an [`OperationStatus`] error on
/// failure. Both are in little-endian format.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all pointers are valid.
#[panic_to_error]
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

    let (transaction, recipient_note) = unwrap_or_return_error!(transfer_funds_sync(
        node,
        tip,
        change_public_key,
        funding_public_keys,
        recipient_public_key,
        amount,
    ));
    FfiTransferFundsResult::ok(TransferFunds {
        hash: transaction.hash().0,
        recipient_note_nonce: fr_to_bytes(&recipient_note.nonce),
    })
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
pub(crate) unsafe fn parse_public_key(pointer: *const u8) -> StatusResult<ZkPublicKey> {
    let bytes = unsafe { std::slice::from_raw_parts(pointer, 32) };
    fr_from_bytes(bytes).map(ZkPublicKey::new).map_err(|error| {
        OperationStatus::error(
            OperationStatusCode::DynError,
            format!("Invalid public key: {error:?}"),
        )
    })
}

pub type FfiChannelDepositResult = FfiStatusResult<Hash>;

/// Arguments for [`channel_deposit`].
///
/// The node's wallet splits a note of exactly `amount` from the notes of
/// `funding_public_key` (which owns them and any change), and the deposit
/// moves it into the channel as a note of `holder_public_key`.
#[repr(C)]
pub struct ChannelDepositArguments {
    /// Optional pointer to the header ID to build against. If null, the current
    /// tip is used.
    pub optional_tip: *const HeaderId,
    /// The 32-byte channel ID to deposit into.
    pub channel_id: *const u8,
    /// The 32-byte public key funding the deposit (owns the funding notes and
    /// any change).
    pub funding_public_key: *const u8,
    /// The 32-byte public key holding the deposited note in the channel.
    pub holder_public_key: *const u8,
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
        if self.holder_public_key.is_null() {
            return Err(OperationStatus::error(
                OperationStatusCode::NullPointer,
                "ChannelDeposit contains a null `holder_public_key` pointer.",
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

/// Builds, signs and submits a channel deposit of exactly `amount`.
///
/// This mirrors the node's `channel_deposit` HTTP handler: the wallet splits a
/// note of exactly `amount` from the notes of `funding_public_key` (change to
/// the same key), and the [`DepositOp`] spends it to a note of
/// `holder_public_key`. Gas is not funded because the platform gas price is
/// currently 0.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
/// - `tip`: The header ID to split the note and sign against.
/// - `channel_id`: The channel to deposit into.
/// - `funding_public_key`: Owns the funding notes and the change.
/// - `holder_public_key`: Holds the deposited note.
/// - `amount`: The exact amount to deposit.
/// - `metadata`: Metadata attached to the deposit.
///
/// # Returns
///
/// A [`Result`] containing the submitted [`SignedOps`] on success, or
/// an [`OperationStatus`] error on failure.
pub(crate) fn channel_deposit_sync(
    node: &LogosBlockchainNode,
    tip: lb_core::header::HeaderId,
    channel_id: ChannelId,
    funding_public_key: ZkPublicKey,
    holder_public_key: ZkPublicKey,
    amount: Value,
    metadata: Metadata,
) -> StatusResult<SignedOps<Preverified, StandardMode>> {
    let runtime_handle = node.get_runtime_handle()?;
    runtime_handle.block_on(async {
        let handle = node.get_overwatch_handle();
        let api = WalletApi::<WalletService, RuntimeServiceId>::from_overwatch_handle(handle).await;

        // 1. The wallet splits a note of exactly `amount`, change to the funding key.
        let TipResponse {
            tip: split_tip,
            response: (split_builder, exact),
        } = api
            .prepare_exact_note(
                Some(tip),
                MantleTxBuilder::new(),
                amount,
                funding_public_key,
                vec![funding_public_key],
            )
            .await
            .map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::DynError,
                    format!("Failed to split the deposit note: {error}"),
                )
            })?;
        let commitment_merkle_root = split_root(&split_builder).map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::RuntimeError,
                format!("Failed to read the split root: {error}"),
            )
        })?;

        // 2. The deposit spends the exact note to a note of the holder, which the
        //    wallet adopts when it owns the holder key.
        let mut nonce = [0u8; 32];
        OsRng.fill_bytes(&mut nonce);
        let holder_note = Note::new(amount, fr_from_mod_bytes(&nonce), holder_public_key);
        let known_addresses = api.get_known_addresses().await.map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::DynError,
                format!("Failed to get known addresses: {error}"),
            )
        })?;
        if known_addresses.contains(&holder_public_key)
            && let Some(note_set) =
                lb_api_service::http::mantle::channel_state::<RuntimeServiceId>(handle, channel_id)
                    .await
                    .map_err(|error| {
                        OperationStatus::error(
                            OperationStatusCode::ServiceError,
                            format!("Failed to get channel state: {error}"),
                        )
                    })?
                    .and_then(|channel| channel.note_set)
        {
            api.import_note(holder_note, note_set)
                .await
                .map_err(|error| {
                    OperationStatus::error(
                        OperationStatusCode::DynError,
                        format!("Failed to import the holder note: {error}"),
                    )
                })?;
        }
        let tx_builder = split_builder
            .push_op_with_opening(
                Op::ChannelDeposit(DepositOp {
                    channel_id,
                    inputs: [exact.nullifier].into(),
                    commitment_merkle_root,
                    outputs: [holder_note.commitment()].into(),
                    excess_value: 0,
                    metadata,
                }),
                ZkTransferOpening {
                    inputs: vec![exact.note],
                    outputs: vec![holder_note],
                },
            )
            .map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::DynError,
                    format!("Failed to push deposit op: {error}"),
                )
            })?;

        // 3. Sign against the tip the split is proven at.
        let signed_tx = api
            .sign_tx(Some(split_tip), tx_builder)
            .await
            .map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::DynError,
                    format!("Failed to sign deposit tx: {error}"),
                )
            })?
            .response;

        // 4. Submit to the mempool.
        if let Err(error) = mempool::add_tx(handle, signed_tx.clone(), Hashable::hash).await {
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
/// The node's wallet splits a note of exactly `amount` from the funding notes,
/// so the channel receives exactly `amount`, held by `holder_public_key`.
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
#[panic_to_error]
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
    let holder_public_key =
        unwrap_or_return_error!(unsafe { parse_public_key(arguments.holder_public_key) });
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
        holder_public_key,
        amount,
        metadata,
    ));
    FfiChannelDepositResult::ok(transaction.hash().0)
}

/// Funds a transaction from the node's wallet.
///
/// This is a synchronous wrapper that mirrors the node's `POST /wallet/fund`
/// HTTP handler: it funds the request's transaction builder from
/// `funding_public_keys` (change to `change_public_key`), rejects it if the
/// fee exceeds `max_tx_fee`, and signs only the appended fee transfer. All
/// other ops are left unsigned for the caller to prove.
pub(crate) fn wallet_fund_tx_sync(
    node: &LogosBlockchainNode,
    request: WalletFundRequestBody,
) -> StatusResult<WalletFundResponseBody> {
    let runtime_handle = node.get_runtime_handle()?;
    runtime_handle.block_on(async {
        let handle = node.get_overwatch_handle();
        let api = WalletApi::<WalletService, RuntimeServiceId>::from_overwatch_handle(handle).await;

        let TipResponse {
            tip,
            response: funded_tx_builder,
        } = api
            .fund_tx(
                request.tip,
                request.tx_builder,
                request.change_public_key,
                request.funding_public_keys,
                request.priority_fee_percent,
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
        if tx_fee > request.max_tx_fee {
            return Err(OperationStatus::error(
                OperationStatusCode::DynError,
                format!(
                    "tx_fee({tx_fee}) exceeds max_tx_fee({})",
                    request.max_tx_fee
                ),
            ));
        }

        let (funded_tx, transfer_proof) = api
            .prove_funding(Some(tip), funded_tx_builder)
            .await
            .map_err(|error| {
                OperationStatus::error(
                    OperationStatusCode::DynError,
                    format!("Failed to prove fee transfer: {error}"),
                )
            })?
            .response;

        Ok(WalletFundResponseBody {
            tip,
            funded_tx,
            transfer_proof,
        })
    })
}

pub type FfiWalletFundResult = FfiStatusResult<*mut c_char>;

/// Funds a transaction from the node's wallet.
///
/// The node adds fee inputs and change from its own wallet, signs only the
/// appended fee transfer, and returns the funded — still unsigned —
/// transaction together with the transfer proof. The caller signs its own
/// ops over the funded transaction hash, assembles the proofs in op order
/// and submits the result via [`submit_signed_transaction`].
///
/// The request and response are JSON strings with the exact same schemas as
/// the node's `POST /wallet/fund` HTTP request and response bodies. The
/// optional `priority_fee_percent` field (default 0) is interpreted as a
/// percentage of the final mandatory fee, including execution and storage
/// cost. The rounded-up percentage is reserved as excess balance above that
/// fee; only the unused reserve is paid to the block producer as the effective
/// priority tip. The Zone SDK and TUI default to 12%, a practical reserve
/// intended to absorb normal fee movement, including approximately one
/// storage-market epoch increase at normal price levels. This is not a
/// protocol guarantee at very low prices or when execution fees also rise
/// materially; integer storage-price arithmetic can make 1 become 2.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
/// - `request_json`: A non-null, NUL-terminated JSON encoding of the fund
///   request.
///
/// # Returns
///
/// A [`FfiWalletFundResult`] containing the JSON-encoded fund response on
/// success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all pointers are valid.
///
/// # Memory Management
///
/// This function allocates the returned C string. The caller must free it
/// using the [`free_cstring`](crate::api::free_cstring) function.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wallet_fund_tx(
    node: *const LogosBlockchainNode,
    request_json: *const c_char,
) -> FfiWalletFundResult {
    return_error_if_null_pointer!(node);
    return_error_if_null_pointer!(request_json);
    let node = unsafe { &*node };

    let request_json = match unsafe { CStr::from_ptr(request_json) }.to_str() {
        Ok(request_json) => request_json,
        Err(error) => {
            return FfiWalletFundResult::err(OperationStatus::error(
                OperationStatusCode::ValidationError,
                format!("Request is not valid UTF-8: {error}"),
            ));
        }
    };
    let request: WalletFundRequestBody = match serde_json::from_str(request_json) {
        Ok(request) => request,
        Err(error) => {
            return FfiWalletFundResult::err(OperationStatus::error(
                OperationStatusCode::ValidationError,
                format!("Failed to parse fund request: {error}"),
            ));
        }
    };

    let response = unwrap_or_return_error!(wallet_fund_tx_sync(node, request));

    let response_json = match serde_json::to_string(&response) {
        Ok(response_json) => response_json,
        Err(error) => {
            return FfiWalletFundResult::err(OperationStatus::error(
                OperationStatusCode::RuntimeError,
                format!("Failed to serialize fund response: {error}"),
            ));
        }
    };
    match CString::new(response_json) {
        Ok(response_json) => FfiWalletFundResult::ok(response_json.into_raw()),
        Err(error) => FfiWalletFundResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to create response CString: {error}"),
        )),
    }
}

pub type FfiSubmitTransactionResult = FfiStatusResult<Hash>;

/// Submits a signed transaction to the mempool.
///
/// Mirrors the node's `POST /mempool/add/tx` HTTP handler. The transaction is
/// a JSON string with the exact same schema as the HTTP request body — for
/// example, a transaction funded via [`wallet_fund_tx`] and completed with the
/// caller's op proofs.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a [`LogosBlockchainNode`] instance.
/// - `signed_tx_json`: A non-null, NUL-terminated JSON encoding of the signed
///   transaction.
///
/// # Returns
///
/// A [`FfiSubmitTransactionResult`] containing the submitted transaction
/// [`Hash`] on success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that all pointers are valid.
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn submit_signed_transaction(
    node: *const LogosBlockchainNode,
    signed_tx_json: *const c_char,
) -> FfiSubmitTransactionResult {
    return_error_if_null_pointer!(node);
    return_error_if_null_pointer!(signed_tx_json);
    let node = unsafe { &*node };

    let preverified_tx = {
        let signed_tx_json = match unsafe { CStr::from_ptr(signed_tx_json) }.to_str() {
            Ok(signed_tx_json) => signed_tx_json,
            Err(error) => {
                return FfiSubmitTransactionResult::err(OperationStatus::error(
                    OperationStatusCode::ValidationError,
                    format!("Transaction is not valid UTF-8: {error}"),
                ));
            }
        };
        let signed_tx: SignedOps<Unverified, StandardMode> =
            match serde_json::from_str(signed_tx_json) {
                Ok(signed_tx) => signed_tx,
                Err(error) => {
                    return FfiSubmitTransactionResult::err(OperationStatus::error(
                        OperationStatusCode::ValidationError,
                        format!("Failed to parse signed transaction: {error}"),
                    ));
                }
            };
        match signed_tx.preverify() {
            Ok(preverified_tx) => preverified_tx,
            Err(error) => {
                return FfiSubmitTransactionResult::err(OperationStatus::error(
                    OperationStatusCode::ValidationError,
                    format!("Failed to preverify signed transaction: {error}"),
                ));
            }
        }
    };

    let transaction_hash = preverified_tx.hash().0;
    let runtime_handle = unwrap_or_return_error!(node.get_runtime_handle());
    let submit_result = runtime_handle.block_on(async {
        mempool::add_tx(node.get_overwatch_handle(), preverified_tx, Hashable::hash).await
    });
    if let Err(error) = submit_result {
        return FfiSubmitTransactionResult::err(OperationStatus::error(
            OperationStatusCode::DynError,
            format!("Failed to add transaction to mempool: {error}"),
        ));
    }

    FfiSubmitTransactionResult::ok(transaction_hash)
}
