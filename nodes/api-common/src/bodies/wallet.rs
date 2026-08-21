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
        mantle::{
            SignedMantleTx, Value, traits::Hashable as _, transactions::states::VerificationState,
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

    impl<State: VerificationState> From<SignedMantleTx<State>> for WalletTransferFundsResponseBody {
        fn from(value: SignedMantleTx<State>) -> Self {
            Self {
                hash: value.mantle_tx().hash(),
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

pub mod fund {
    use lb_core::{
        header::HeaderId,
        mantle::{
            OpProof,
            gas::{FeeHorizonHours, GasCost},
            transactions::{RawMantleTx, builder::MantleTxBuilder},
        },
    };
    use lb_key_management_system_keys::keys::ZkPublicKey;
    use serde::{Deserialize, Serialize, de::Error as _};
    use serde_json::value::RawValue;

    #[expect(
        clippy::unnecessary_wraps,
        reason = "Serde requires the default function to return the optional field type."
    )]
    const fn default_fee_horizon_hours() -> Option<FeeHorizonHours> {
        Some(FeeHorizonHours::from_tenths(10))
    }

    fn deserialize_fee_horizon_hours<'de, D>(
        deserializer: D,
    ) -> Result<Option<FeeHorizonHours>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let Some(raw) = Option::<Box<RawValue>>::deserialize(deserializer)? else {
            return Ok(None);
        };
        let raw = raw.get();
        let value = if raw.starts_with('"') {
            serde_json::from_str::<String>(raw).map_err(D::Error::custom)?
        } else {
            raw.to_owned()
        };
        value.parse().map(Some).map_err(D::Error::custom)
    }

    #[derive(Serialize, Deserialize)]
    pub struct WalletFundRequestBody {
        pub tip: Option<HeaderId>,
        pub tx_builder: MantleTxBuilder,
        pub change_public_key: ZkPublicKey,
        pub funding_public_keys: Vec<ZkPublicKey>,
        /// Absolute hard cap on the final funded transaction fee.
        pub max_tx_fee: GasCost,
        /// Percentage of the mandatory fee at preparation-time prices reserved
        /// as an absolute priority incentive. The percentage applies to the
        /// complete mandatory fee (execution plus storage), not to storage
        /// alone, and is not reapplied to future prices. When omitted, it
        /// defaults to `0`; that requests no explicit priority reserve. The
        /// value is not capped at 100.
        #[serde(default)]
        pub priority_fee_percent: u64,
        /// Optional elapsed-time horizon for deterministic storage-market fee
        /// coverage. This reserve is independent of the priority reserve. An
        /// omitted field defaults to one hour; `null` or `0.0` explicitly
        /// disables horizon coverage. This accepts non-negative decimal hours;
        /// values are rounded up to the next 0.1 hour. The maximum supported
        /// horizon is 168 hours (7 days); larger values are rejected. Any
        /// unused horizon reserve is effective tip immediately and is consumed
        /// as the mandatory storage fee rises.
        #[serde(
            default = "default_fee_horizon_hours",
            deserialize_with = "deserialize_fee_horizon_hours"
        )]
        pub fee_horizon_hours: Option<FeeHorizonHours>,
    }

    #[derive(Serialize, Deserialize)]
    pub struct WalletFundResponseBody {
        /// Tip the transaction was funded against.
        pub tip: HeaderId,
        /// The funded transaction, with the fee transfer appended as the last
        /// op. All ops are still unsigned.
        pub funded_tx: RawMantleTx,
        /// Proof for the appended fee transfer, signed over the funded
        /// transaction hash. `None` if funding required no transfer (zero
        /// fee and no inputs pulled in).
        pub transfer_proof: Option<OpProof>,
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn request_json() -> String {
            let key = ZkPublicKey::zero();
            let request = WalletFundRequestBody {
                tip: None,
                tx_builder: MantleTxBuilder::new(),
                change_public_key: key,
                funding_public_keys: vec![key],
                max_tx_fee: GasCost::new(0),
                priority_fee_percent: 0,
                fee_horizon_hours: None,
            };
            serde_json::to_string(&request).unwrap()
        }

        #[test]
        fn omitted_fee_horizon_defaults_to_one_hour_but_null_and_zero_disable_it() {
            let key = ZkPublicKey::zero();
            let request = WalletFundRequestBody {
                tip: None,
                tx_builder: MantleTxBuilder::new(),
                change_public_key: key,
                funding_public_keys: vec![key],
                max_tx_fee: GasCost::new(0),
                priority_fee_percent: 0,
                fee_horizon_hours: None,
            };
            let mut json = serde_json::to_value(&request).unwrap();
            let object = json.as_object_mut().unwrap();
            object.remove("priority_fee_percent");
            object.remove("fee_horizon_hours");

            let parsed: WalletFundRequestBody =
                serde_json::from_str(&serde_json::to_string(&json).unwrap()).unwrap();
            assert_eq!(parsed.priority_fee_percent, 0);
            assert_eq!(
                parsed.fee_horizon_hours,
                Some(FeeHorizonHours::from_tenths(10))
            );

            json["fee_horizon_hours"] = serde_json::Value::Null;
            let parsed: WalletFundRequestBody =
                serde_json::from_str(&serde_json::to_string(&json).unwrap()).unwrap();
            assert_eq!(parsed.fee_horizon_hours, None);

            json["fee_horizon_hours"] = serde_json::json!(0.0);
            let parsed: WalletFundRequestBody =
                serde_json::from_str(&serde_json::to_string(&json).unwrap()).unwrap();
            assert_eq!(
                parsed.fee_horizon_hours,
                Some(FeeHorizonHours::from_tenths(0))
            );
        }

        #[test]
        fn fee_horizon_hours_api_normalizes_decimal_numbers() {
            let json =
                request_json().replace("\"fee_horizon_hours\":null", "\"fee_horizon_hours\":0.25");

            let parsed: WalletFundRequestBody = serde_json::from_str(&json).unwrap();
            assert_eq!(
                parsed.fee_horizon_hours,
                Some(FeeHorizonHours::from_tenths(3))
            );
        }

        #[test]
        fn fee_horizon_hours_api_reports_supported_maximum() {
            let json = request_json().replace(
                "\"fee_horizon_hours\":null",
                "\"fee_horizon_hours\":168.00001",
            );

            let error = match serde_json::from_str::<WalletFundRequestBody>(&json) {
                Ok(_) => panic!("fee horizon above the supported maximum was accepted"),
                Err(error) => error.to_string(),
            };
            let expected_error = "168.00001"
                .parse::<FeeHorizonHours>()
                .expect_err("test horizon should exceed the supported maximum");
            assert!(
                error.contains(&expected_error),
                "unexpected validation error: {error}"
            );
        }

        #[test]
        fn fee_horizon_hours_api_preserves_decimal_token_precision() {
            let json = request_json();

            let json = json.replace(
                "\"fee_horizon_hours\":null",
                "\"fee_horizon_hours\":1.00000000000000001",
            );
            let parsed: WalletFundRequestBody = serde_json::from_str(&json).unwrap();
            assert_eq!(
                parsed.fee_horizon_hours,
                Some(FeeHorizonHours::from_tenths(11))
            );

            let json = json.replace("1.00000000000000001", "168.00000000000000001");
            let error = match serde_json::from_str::<WalletFundRequestBody>(&json) {
                Ok(_) => panic!("fee horizon above the supported maximum was accepted"),
                Err(error) => error.to_string(),
            };
            let expected_error = "168.00000000000000001"
                .parse::<FeeHorizonHours>()
                .expect_err("test horizon should exceed the supported maximum");
            assert!(
                error.contains(&expected_error),
                "unexpected validation error: {error}"
            );
        }
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
