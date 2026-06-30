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
        mantle::ops::leader_claim::{VoucherCm, VoucherNullifier},
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
        mantle::{SignedMantleTx, Transaction as _, Value},
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
        pub hash: lb_core::mantle::tx::TxHash,
    }

    impl From<SignedMantleTx> for WalletTransferFundsResponseBody {
        fn from(value: SignedMantleTx) -> Self {
            Self {
                hash: value.mantle_tx.hash(),
            }
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

pub mod sign {
    use lb_core::mantle::TxHash;
    use lb_key_management_system_keys::keys::{
        Ed25519Key, ZkPublicKey, ZkSignature, secured_key::SecuredKey,
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
        pub pks: Vec<ZkPublicKey>,
    }

    #[derive(Serialize, Deserialize)]
    pub struct WalletSignTxZkResponseBody {
        pub sig: ZkSignature,
    }
}
