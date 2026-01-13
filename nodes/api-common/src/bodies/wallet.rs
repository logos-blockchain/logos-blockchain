pub mod balance {
    use axum::{
        http::StatusCode,
        response::{IntoResponse, Response},
    };
    use key_management_system_keys::keys::ZkPublicKey;
    use nomos_core::{header::HeaderId, mantle::Value};
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub struct WalletBalanceResponseBody {
        pub tip: HeaderId,
        pub balance: Value,
        pub address: ZkPublicKey,
    }

    impl IntoResponse for WalletBalanceResponseBody {
        fn into_response(self) -> Response {
            (StatusCode::OK, serde_json::to_string(&self).unwrap()).into_response()
        }
    }
}

pub mod transfer_funds {
    use axum::{
        http::StatusCode,
        response::{IntoResponse, Response},
    };
    use key_management_system_keys::keys::ZkPublicKey;
    use nomos_core::{
        header::HeaderId,
        mantle::{SignedMantleTx, Transaction as _, Value},
    };
    use serde::{Deserialize, Serialize};

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
        pub hash: nomos_core::mantle::tx::TxHash,
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
            (StatusCode::CREATED, serde_json::to_string(&self).unwrap()).into_response()
        }
    }
}
