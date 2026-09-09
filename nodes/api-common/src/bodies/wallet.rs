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

    #[derive(Serialize, Deserialize)]
    pub struct WalletTransferFundsRequestBody {
        pub tip: Option<HeaderId>,
        pub change_public_key: ZkPublicKey,
        pub funding_public_keys: Vec<ZkPublicKey>,
        pub recipient_public_key: ZkPublicKey,
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
