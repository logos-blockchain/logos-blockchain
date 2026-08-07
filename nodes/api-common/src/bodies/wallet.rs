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

/// Public wallet fee-policy and fee-quote DTOs.
pub mod fee {
    use lb_core::{
        header::HeaderId,
        mantle::{
            EpochHeadroom, ExecutionProjectionModel, FeeHorizonQuote, FeePolicy, Value,
            transactions::GasPrices,
        },
    };
    use serde::{Deserialize, Serialize};

    /// Public request policy for projecting mandatory fees and adding a tip.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    pub struct WalletFeePolicy {
        /// Future elapsed chain time, measured in epochs, to provision for.
        #[serde(default)]
        pub epoch_headroom: Option<EpochHeadroom>,
        /// Explicit inclusion incentive, independent of the projected reserve.
        #[serde(default)]
        pub priority_fee: Value,
    }

    impl From<WalletFeePolicy> for FeePolicy {
        fn from(value: WalletFeePolicy) -> Self {
            Self {
                epoch_headroom: value.epoch_headroom,
                priority_fee: value.priority_fee,
            }
        }
    }

    /// Stable public identifier for the execution fee-estimation model.
    #[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
    pub enum WalletExecutionModel {
        /// Target-load produced blocks with the version-one recurrence.
        #[serde(rename = "target_load_v1")]
        TargetLoadV1,
    }

    impl From<ExecutionProjectionModel> for WalletExecutionModel {
        fn from(value: ExecutionProjectionModel) -> Self {
            match value {
                ExecutionProjectionModel::TargetLoadV1 => Self::TargetLoadV1,
            }
        }
    }

    /// Public execution-estimation assumptions included in a fee quote.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    pub struct WalletExecutionProjection {
        /// Execution EMA at the preparation state.
        pub starting_ema: u64,
        /// Assumed execution gas in every simulated future block.
        pub assumed_future_execution_gas: u64,
        /// Versioned estimator model identifier.
        pub estimation_model: WalletExecutionModel,
        /// Expected slots between produced blocks under active consensus.
        pub average_slots_per_block: u64,
    }

    /// Public gas-price pair used in a wallet fee quote.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    pub struct WalletGasPrices {
        /// Execution gas price in the quote state.
        pub execution_base_gas_price: u64,
        /// Storage gas price in the quote state.
        pub storage_gas_price: u64,
    }

    impl From<GasPrices> for WalletGasPrices {
        fn from(value: GasPrices) -> Self {
            Self {
                execution_base_gas_price: value.execution_base_gas_price.into_inner(),
                storage_gas_price: value.storage_gas_price.into_inner(),
            }
        }
    }

    /// Public fee quote returned for a policy-funded transaction.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    pub struct WalletFeeQuote {
        /// Requested elapsed-time headroom.
        pub epoch_headroom: EpochHeadroom,
        /// Preparation tip used for every quote input.
        pub prepared_at_tip: HeaderId,
        /// Slot at which the quote was prepared.
        pub prepared_at_slot: u64,
        /// Epoch at which the quote was prepared.
        pub prepared_at_epoch: u64,
        /// Active slots per epoch.
        pub slots_per_epoch: u64,
        /// Authoritative slot through which prices were projected.
        pub valid_until_slot: u64,
        /// Epoch containing the valid-until slot.
        pub valid_until_epoch: u64,
        /// Number of storage boundaries crossed by the slot horizon.
        pub storage_boundaries_crossed: u64,
        /// Expected produced blocks simulated for execution.
        pub expected_execution_blocks: u64,
        /// Live prices at the preparation tip.
        pub live_prices: WalletGasPrices,
        /// Prices used to fund the transaction.
        pub projected_prices: WalletGasPrices,
        /// Execution assumptions and starting state.
        pub execution_projection: WalletExecutionProjection,
        /// Mandatory fee at live prices.
        pub mandatory_fee_live: u64,
        /// Mandatory fee at projected prices.
        pub mandatory_fee_projected: u64,
        /// Explicit priority fee requested by the caller.
        pub explicit_priority_fee: Value,
        /// Projected mandatory fee plus explicit priority fee.
        pub total_fee: u64,
    }

    impl From<FeeHorizonQuote> for WalletFeeQuote {
        fn from(value: FeeHorizonQuote) -> Self {
            Self {
                epoch_headroom: value.epoch_headroom,
                prepared_at_tip: value.prepared_at_tip,
                prepared_at_slot: value.prepared_at_slot.into(),
                prepared_at_epoch: u64::from(value.prepared_at_epoch.into_inner()),
                slots_per_epoch: value.slots_per_epoch,
                valid_until_slot: value.valid_until_slot.into(),
                valid_until_epoch: u64::from(value.valid_until_epoch.into_inner()),
                storage_boundaries_crossed: value.storage_boundaries_crossed,
                expected_execution_blocks: value.expected_execution_blocks,
                live_prices: value.live_prices.into(),
                projected_prices: value.projected_prices.into(),
                execution_projection: WalletExecutionProjection {
                    starting_ema: value.execution_projection.starting_ema.into_inner(),
                    assumed_future_execution_gas: value
                        .execution_projection
                        .assumed_future_execution_gas
                        .into_inner(),
                    estimation_model: value.execution_projection.estimation_model.into(),
                    average_slots_per_block: value.execution_projection.average_slots_per_block,
                },
                mandatory_fee_live: value.mandatory_fee_live.into_inner(),
                mandatory_fee_projected: value.mandatory_fee_projected.into_inner(),
                explicit_priority_fee: value.explicit_priority_fee,
                total_fee: value.total_fee.into_inner(),
            }
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
            SignedMantleTx, Value, gas::GasCost, traits::Hashable as _,
            transactions::states::VerificationState,
        },
    };
    use lb_key_management_system_keys::keys::ZkPublicKey;
    use lb_log_targets::api;
    use serde::{Deserialize, Serialize};
    use tracing::error;

    use super::fee::{WalletFeePolicy, WalletFeeQuote};

    const LOG_TARGET: &str = api::http::wallet::TRANSFER_FUNDS;

    #[derive(Serialize, Deserialize)]
    /// Request body for the high-level wallet transfer endpoint.
    pub struct WalletTransferFundsRequestBody {
        /// Optional chain tip at which to prepare the transfer.
        pub tip: Option<HeaderId>,
        /// Public key receiving transaction change.
        pub change_public_key: ZkPublicKey,
        /// Public keys whose notes may fund the transfer.
        pub funding_public_keys: Vec<ZkPublicKey>,
        /// Public key receiving the requested amount.
        pub recipient_public_key: ZkPublicKey,
        /// Amount transferred to the recipient.
        pub amount: Value,
        /// Optional projected mandatory-fee policy. Omitting it preserves the
        /// legacy zero-priority-fee transfer behaviour.
        #[serde(default)]
        pub fee_policy: Option<WalletFeePolicy>,
        /// Optional maximum for the final funded transaction fee.
        #[serde(default)]
        pub max_tx_fee: Option<GasCost>,
    }

    #[derive(Serialize, Deserialize)]
    /// Response body for a high-level wallet transfer.
    pub struct WalletTransferFundsResponseBody {
        /// Hash of the submitted transaction.
        pub hash: lb_core::mantle::transactions::TxHash,
        /// Fee-horizon metadata when the request supplied a policy.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub fee_quote: Option<WalletFeeQuote>,
    }

    impl<State: VerificationState> From<SignedMantleTx<State>> for WalletTransferFundsResponseBody {
        fn from(value: SignedMantleTx<State>) -> Self {
            Self {
                hash: value.mantle_tx().hash(),
                fee_quote: None,
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
            OpProof, Value,
            gas::GasCost,
            transactions::{RawMantleTx, builder::MantleTxBuilder},
        },
    };
    use lb_key_management_system_keys::keys::ZkPublicKey;
    use serde::{Deserialize, Serialize};

    use super::fee::{WalletFeePolicy, WalletFeeQuote};

    #[derive(Serialize, Deserialize)]
    /// Request body for funding an existing transaction builder.
    pub struct WalletFundRequestBody {
        /// Optional chain tip at which to fund the builder.
        pub tip: Option<HeaderId>,
        /// Unsigned transaction builder to fund.
        pub tx_builder: MantleTxBuilder,
        /// Public key receiving transaction change.
        pub change_public_key: ZkPublicKey,
        /// Public keys whose notes may fund the transaction.
        pub funding_public_keys: Vec<ZkPublicKey>,
        /// Maximum fee accepted for the final funded transaction.
        pub max_tx_fee: GasCost,
        /// Legacy top-level explicit priority fee.
        #[serde(default)]
        pub priority_fee: Value,
        /// Optional projected mandatory-fee policy. Its priority fee is
        /// independent from the legacy top-level field.
        #[serde(default)]
        pub fee_policy: Option<WalletFeePolicy>,
    }

    #[derive(Serialize, Deserialize)]
    /// Response body for a funded transaction builder.
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
        /// Fee-horizon metadata when the request supplied a policy.
        pub fee_quote: Option<WalletFeeQuote>,
    }
}

pub mod sign {
    use lb_core::mantle::transactions::hash::TxHash;
    use lb_key_management_system_keys::keys::{
        Ed25519Key, ZkPublicKeys, ZkSignature, secured_key::SecuredKey,
    };
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    /// Request body for an Ed25519 transaction signature.
    pub struct WalletSignTxEd25519RequestBody {
        /// Transaction hash to sign.
        pub tx_hash: TxHash,
        /// Ed25519 public key identifying the signer.
        pub pk: <Ed25519Key as SecuredKey>::PublicKey,
    }

    #[derive(Serialize, Deserialize)]
    /// Response body containing an Ed25519 signature.
    pub struct WalletSignTxEd25519ResponseBody {
        /// Generated signature.
        pub sig: <Ed25519Key as SecuredKey>::Signature,
    }

    #[derive(Serialize, Deserialize)]
    /// Request body for a zero-knowledge transaction signature.
    pub struct WalletSignTxZkRequestBody {
        /// Transaction hash to sign.
        pub tx_hash: TxHash,
        /// ZK public keys identifying the signers.
        pub pks: ZkPublicKeys,
    }

    #[derive(Serialize, Deserialize)]
    /// Response body containing a zero-knowledge signature.
    pub struct WalletSignTxZkResponseBody {
        /// Generated signature.
        pub sig: ZkSignature,
    }
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::EpochHeadroom;

    use super::fee::WalletFeePolicy;

    #[test]
    fn public_fee_policy_round_trips_decimal_headroom_and_priority_fee() {
        let policy: WalletFeePolicy =
            serde_json::from_str(r#"{"epoch_headroom":1.3,"priority_fee":500}"#).unwrap();
        assert_eq!(
            policy.epoch_headroom,
            Some(EpochHeadroom::from_tenths(13).unwrap())
        );
        assert_eq!(policy.priority_fee, 500);

        let serialized = serde_json::to_value(policy).unwrap();
        assert_eq!(serialized["epoch_headroom"], 1.3);
        assert_eq!(serialized["priority_fee"], 500);
    }

    #[test]
    fn public_fee_policy_truncates_extra_decimal_precision() {
        let policy: WalletFeePolicy =
            serde_json::from_str(r#"{"epoch_headroom":1.39,"priority_fee":500}"#).unwrap();
        assert_eq!(
            policy.epoch_headroom,
            Some(EpochHeadroom::from_tenths(13).unwrap())
        );

        let policy: WalletFeePolicy =
            serde_json::from_str(r#"{"epoch_headroom":100.09,"priority_fee":500}"#).unwrap();
        assert_eq!(
            policy.epoch_headroom,
            Some(EpochHeadroom::from_tenths(1_000).unwrap())
        );

        assert!(
            serde_json::from_str::<WalletFeePolicy>(
                r#"{"epoch_headroom":100.10,"priority_fee":500}"#
            )
            .is_err()
        );
    }
}
