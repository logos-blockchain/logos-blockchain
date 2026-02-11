use std::fmt::{Debug, Display};

use lb_core::{
    header::HeaderId,
    mantle::{
        Op, SignedMantleTx, Value, gas::MainnetGasConstants, ops::leader_claim::LeaderClaimOp,
        tx_builder::MantleTxBuilder,
    },
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_wallet_service::api::{WalletApi, WalletApiError, WalletServiceData};
use overwatch::services::AsServiceId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderWalletConfig {
    // Hard cap on the transaction fee for LEADER_CLAIM
    pub max_tx_fee: Value,

    // The key to use for paying transaction fees for LEADER_CLAIM.
    // Change notes will be returned to this same funding pk.
    pub funding_pk: ZkPublicKey,
}

pub async fn fund_and_sign_leader_claim_tx<Wallet, RuntimeServiceId>(
    op: LeaderClaimOp,
    tip: HeaderId,
    wallet: &WalletApi<Wallet, RuntimeServiceId>,
    config: &LeaderWalletConfig,
) -> Result<SignedMantleTx, LeaderWalletError>
where
    Wallet: WalletServiceData,
    RuntimeServiceId: Debug + Send + Sync + Display + 'static + AsServiceId<Wallet>,
{
    let tx_builder = MantleTxBuilder::new().push_op(Op::LeaderClaim(op));
    let funded_tx_builder = wallet
        .fund_tx(
            Some(tip),
            tx_builder,
            config.funding_pk,
            vec![config.funding_pk],
        )
        .await
        .map_err(|e| LeaderWalletError::WalletApi(Box::new(e)))?
        .response;

    let tx_fee = funded_tx_builder.gas_cost::<MainnetGasConstants>();
    if tx_fee > config.max_tx_fee {
        return Err(LeaderWalletError::TxFeeExceedsMaxFee {
            tx_fee,
            max_fee: config.max_tx_fee,
        });
    }

    Ok(wallet
        .sign_tx(Some(tip), funded_tx_builder)
        .await
        .map_err(|e| LeaderWalletError::WalletApi(Box::new(e)))?
        .response)
}

#[derive(Debug, thiserror::Error)]
pub enum LeaderWalletError {
    #[error(transparent)]
    WalletApi(#[from] Box<WalletApiError>),
    #[error("Transaction fee exceeded the configured max fee. tx_fee={tx_fee} > max_fee={max_fee}")]
    TxFeeExceedsMaxFee { max_fee: Value, tx_fee: Value },
}
