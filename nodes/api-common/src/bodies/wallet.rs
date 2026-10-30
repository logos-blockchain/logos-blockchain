pub mod balance {
    use std::collections::HashMap;

    use axum::{
        http::StatusCode,
        response::{IntoResponse, Response},
    };
    use lb_core::{
        header::HeaderId,
        mantle::{NoteId, Value},
    };
    use lb_key_management_system_keys::keys::ZkPublicKey;
    use lb_log_targets::api;
    use serde::{Deserialize, Serialize};
    use tracing::error;

    const LOG_TARGET: &str = api::http::wallet::BALANCE;

    #[derive(Serialize, Deserialize)]
    pub struct WalletBalanceResponseBody {
        pub tip: HeaderId,
        pub balance: Value,
        pub notes: HashMap<NoteId, Value>,
        pub address: ZkPublicKey,
    }

    impl IntoResponse for WalletBalanceResponseBody {
        fn into_response(self) -> Response {
            let json = serde_json::to_string(&self).unwrap_or_else(|e| {
                error!(
                    target: LOG_TARGET,
                    "WalletBalanceResponseBody serialization error: {e}"
                );
                // We panic here because this should never happen, and if it does, it's a
                // critical error that we want to be immediately visible during
                // development and testing.
                panic!("WalletBalanceResponseBody serialization failed: {e}")
            });

            (StatusCode::OK, json).into_response()
        }
    }
}

pub mod claimable_vouchers {
    use axum::{
        http::StatusCode,
        response::{IntoResponse, Response},
    };
    use lb_core::{
        header::HeaderId,
        mantle::{
            Value,
            ops::leader_claim::{VoucherCm, VoucherNullifier},
        },
    };
    use lb_log_targets::api;
    use serde::{Deserialize, Serialize};
    use tracing::error;

    const LOG_TARGET: &str = api::http::wallet::CLAIMABLE_VOUCHERS;

    #[derive(Serialize, Deserialize)]
    pub struct ClaimableVoucherInfoResponseBody {
        pub commitment: VoucherCm,
        pub nullifier: VoucherNullifier,
    }

    #[derive(Serialize, Deserialize)]
    pub struct WalletClaimableVouchersResponseBody {
        pub tip: HeaderId,
        pub vouchers: Vec<ClaimableVoucherInfoResponseBody>,
        /// What a single voucher pays out at `tip`.
        ///
        /// The reward pool is split evenly across every unclaimed voucher on
        /// the chain, so this is the same for each entry in `vouchers` and it
        /// moves as other leaders claim. It is a snapshot at `tip`, not a
        /// guarantee of what a claim submitted now will settle for.
        pub reward_amount: Value,
        /// `reward_amount` times the number of `vouchers`: what this wallet
        /// could claim in total at `tip`.
        pub total_claimable: Value,
    }

    impl IntoResponse for WalletClaimableVouchersResponseBody {
        fn into_response(self) -> Response {
            let json = serde_json::to_string(&self).unwrap_or_else(|e| {
                error!(
                    target: LOG_TARGET,
                    "WalletClaimableVouchersResponseBody serialization failed: {e}"
                );
                // We panic here because this should never happen, and if it does, it's a
                // critical error that we want to be immediately visible during
                // development and testing.
                panic!("WalletClaimableVouchersResponseBody serialization failed: {e}")
            });

            (StatusCode::OK, json).into_response()
        }
    }
}

pub mod aged_notes {
    use axum::{
        http::StatusCode,
        response::{IntoResponse, Response},
    };
    use lb_core::{
        header::HeaderId,
        mantle::{NoteId, Value},
    };
    use lb_key_management_system_keys::keys::ZkPublicKey;
    use lb_log_targets::api;
    use serde::{Deserialize, Serialize};
    use tracing::error;

    const LOG_TARGET: &str = api::http::wallet::AGED_NOTES;

    /// One wallet-owned UTXO old enough to take part in the leadership
    /// lottery.
    #[derive(Serialize, Deserialize)]
    pub struct LeaderAgedNoteResponseBody {
        pub note_id: NoteId,
        pub value: Value,
        /// The wallet address holding the note.
        pub public_key: ZkPublicKey,
    }

    /// The wallet's UTXOs that are eligible to lead at `tip`.
    ///
    /// A note is eligible when it is present in the epoch's aged UTXO
    /// snapshot — the same stake distribution the leadership proof is built
    /// against — and its public key is one the wallet holds a key for. An
    /// empty `notes` means this node cannot win a slot at `tip`: either it
    /// owns no notes, or none of them have aged into the current epoch's
    /// snapshot yet.
    ///
    /// The set is reported unfiltered. The leader service additionally skips
    /// the UTXOs of the keys in `consensus_excluded_keys`.
    #[derive(Serialize, Deserialize)]
    pub struct LeaderAgedNotesResponseBody {
        pub tip: HeaderId,
        pub notes: Vec<LeaderAgedNoteResponseBody>,
        /// Number of eligible notes. `notes.len()`, repeated so a caller can
        /// answer "am I eligible?" without walking the list.
        pub count: usize,
        /// Total value staked across `notes`, saturating.
        pub total_value: Value,
    }

    impl IntoResponse for LeaderAgedNotesResponseBody {
        fn into_response(self) -> Response {
            let json = serde_json::to_string(&self).unwrap_or_else(|e| {
                error!(
                    target: LOG_TARGET,
                    "LeaderAgedNotesResponseBody serialization failed: {e}"
                );
                // We panic here because this should never happen, and if it does, it's a
                // critical error that we want to be immediately visible during
                // development and testing.
                panic!("LeaderAgedNotesResponseBody serialization failed: {e}")
            });

            (StatusCode::OK, json).into_response()
        }
    }
}

pub mod transfer_funds {
    use axum::{
        http::StatusCode,
        response::{IntoResponse, Response},
    };
    use lb_core::{
        header::HeaderId,
        mantle::{
            SignedOps, Value, ledger::verification_mode::StandardMode, traits::Hashable as _,
            transactions::states::VerificationState,
        },
    };
    use lb_key_management_system_keys::keys::ZkPublicKey;
    use lb_log_targets::api;
    use serde::{Deserialize, Serialize};
    use tracing::error;

    const LOG_TARGET: &str = api::http::wallet::TRANSFER_FUNDS;

    /// Unknown fields are rejected, so a misspelled field fails the request
    /// instead of being silently ignored.
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct WalletTransferFundsRequestBody {
        /// Block whose ledger state funds the transfer. Defaults to the
        /// current chain tip.
        pub tip: Option<HeaderId>,
        /// Key that receives the change output, if any.
        pub change_public_key: ZkPublicKey,
        /// Keys whose notes may be spent to fund the transfer.
        pub funding_public_keys: Vec<ZkPublicKey>,
        /// Key that receives `amount`.
        pub recipient_public_key: ZkPublicKey,
        /// Value sent to `recipient_public_key`.
        pub amount: Value,
    }

    #[derive(Serialize, Deserialize)]
    pub struct WalletTransferFundsResponseBody {
        pub hash: lb_core::mantle::transactions::TxHash,
    }

    impl<State: VerificationState> From<SignedOps<State, StandardMode>>
        for WalletTransferFundsResponseBody
    {
        fn from(value: SignedOps<State, StandardMode>) -> Self {
            Self { hash: value.hash() }
        }
    }

    impl IntoResponse for WalletTransferFundsResponseBody {
        fn into_response(self) -> Response {
            let json = serde_json::to_string(&self).unwrap_or_else(|e| {
                error!(
                    target: LOG_TARGET,
                    "WalletTransferFundsResponseBody serialization failed: {e}"
                );
                // We panic here because this should never happen, and if it does, it's a
                // critical error that we want to be immediately visible during
                // development and testing.
                panic!("WalletTransferFundsResponseBody serialization failed: {e}")
            });

            (StatusCode::CREATED, json).into_response()
        }
    }

    #[cfg(test)]
    mod tests {
        use serde_json::{Value as JsonValue, json};

        use super::*;

        fn valid_request_json() -> JsonValue {
            serde_json::to_value(WalletTransferFundsRequestBody {
                tip: None,
                change_public_key: ZkPublicKey::zero(),
                funding_public_keys: vec![ZkPublicKey::zero()],
                recipient_public_key: ZkPublicKey::zero(),
                amount: 1000,
            })
            .unwrap()
        }

        #[test]
        fn request_body_accepts_its_own_fields() {
            serde_json::from_value::<WalletTransferFundsRequestBody>(valid_request_json()).unwrap();
        }

        #[test]
        fn request_body_rejects_unknown_fields() {
            let mut request = valid_request_json();
            request["value"] = json!(1000);

            let error = serde_json::from_value::<WalletTransferFundsRequestBody>(request).err();

            assert!(
                error.is_some_and(|error| error.to_string().contains("unknown field `value`")),
                "an unknown field must be rejected, not silently ignored"
            );
        }
    }
}

pub mod fund {
    use lb_core::{
        header::HeaderId,
        mantle::{
            OpProof,
            gas::GasCost,
            transactions::{Ops, builder::MantleTxBuilder, tx_list::ops::mantle_spec},
        },
    };
    use lb_key_management_system_keys::keys::ZkPublicKey;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub struct WalletFundRequestBody {
        pub tip: Option<HeaderId>,
        pub tx_builder: MantleTxBuilder,
        pub change_public_key: ZkPublicKey,
        pub funding_public_keys: Vec<ZkPublicKey>,
        /// Absolute hard cap on the final funded transaction fee.
        pub max_tx_fee: GasCost,
        /// Percentage of the final mandatory fee reserved as a priority fee.
        /// The percentage applies to the complete mandatory fee (execution
        /// plus storage), not to storage alone. Only the unused reserve is an
        /// effective priority tip. The default used by the Zone SDK and TUI
        /// is 12%, a practical reserve intended to absorb normal fee movement,
        /// including approximately one storage-market epoch increase at
        /// normal price levels. It is not a protocol guarantee at very low
        /// prices or when execution fees also rise materially. Storage prices
        /// use integer arithmetic, so a low price can jump proportionally more
        /// (for example, 1 to 2). `0` funds exactly to the mandatory fee; the
        /// value is not capped at 100.
        #[serde(default)]
        pub priority_fee_percent: u64,
    }

    #[derive(Serialize, Deserialize)]
    pub struct WalletFundResponseBody {
        /// Tip the transaction was funded against.
        pub tip: HeaderId,
        /// The funded transaction, with the fee transfer appended as the last
        /// op. All ops are still unsigned.
        #[serde(with = "mantle_spec")]
        pub funded_tx: Ops,
        /// Proof for the appended fee transfer, signed over the funded
        /// transaction hash. `None` if funding required no transfer (zero
        /// fee and no inputs pulled in).
        pub transfer_proof: Option<OpProof>,
    }
}

pub mod sign {
    use lb_core::mantle::transactions::hash::TxHash;
    use lb_key_management_system_keys::keys::{
        Ed25519Key, ZkPublicKeys, ZkSignature, secured_key::SecuredKey,
    };
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub struct WalletSignTxEd25519RequestBody {
        pub tx_hash: TxHash,
        pub pk: <Ed25519Key as SecuredKey>::PublicKey,
    }

    #[derive(Serialize, Deserialize)]
    pub struct WalletSignTxEd25519ResponseBody {
        pub sig: <Ed25519Key as SecuredKey>::Signature,
    }

    #[derive(Serialize, Deserialize)]
    pub struct WalletSignTxZkRequestBody {
        pub tx_hash: TxHash,
        pub pks: ZkPublicKeys,
    }

    #[derive(Serialize, Deserialize)]
    pub struct WalletSignTxZkResponseBody {
        pub sig: ZkSignature,
    }
}
