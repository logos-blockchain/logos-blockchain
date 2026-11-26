use lb_api_common::bodies::wallet::{
    aged_notes::{LeaderAgedNoteResponseBody, LeaderAgedNotesResponseBody},
    balance::WalletBalanceResponseBody,
    claimable_vouchers::{ClaimableVoucherInfoResponseBody, WalletClaimableVouchersResponseBody},
    fund::{WalletFundRequestBody, WalletFundResponseBody},
    sign::{
        WalletSignTxEd25519RequestBody, WalletSignTxEd25519ResponseBody, WalletSignTxZkRequestBody,
        WalletSignTxZkResponseBody,
    },
    transfer_funds::{WalletTransferFundsRequestBody, WalletTransferFundsResponseBody},
};
use lb_core::{
    header::HeaderId,
    mantle::{OpProof, gas::GasCost, traits::Hashable as _},
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_wallet_service::{
    TipResponse,
    api::{WalletApi, WalletApiError},
};
use overwatch::services::status::ServiceStatus;

use super::{Error, Handle, mempool, time::fork_digest_in_force};
use crate::{RuntimeServiceId, WalletService};

impl From<WalletApiError> for Error {
    fn from(error: WalletApiError) -> Self {
        match error {
            WalletApiError::RelaySend(_) | WalletApiError::RelayRecv(_) => {
                Self::unavailable(error.to_string())
            }
            _ => Self::internal(error.to_string()),
        }
    }
}

pub(super) async fn wallet_api(
    handle: &Handle,
) -> Result<WalletApi<WalletService, RuntimeServiceId>, Error> {
    Ok(WalletApi::new(handle.relay::<WalletService>().await?))
}

pub(super) fn check_max_tx_fee(tx_fee: GasCost, max_tx_fee: GasCost) -> Result<(), Error> {
    if tx_fee > max_tx_fee {
        return Err(Error::internal(format!(
            "tx_fee({tx_fee}) exceeds max_tx_fee({max_tx_fee})"
        )));
    }
    Ok(())
}

pub async fn known_addresses(handle: &Handle) -> Result<Vec<ZkPublicKey>, Error> {
    Ok(wallet_api(handle).await?.get_known_addresses().await?)
}

pub async fn balance(
    handle: &Handle,
    tip: Option<HeaderId>,
    address: ZkPublicKey,
) -> Result<WalletBalanceResponseBody, Error> {
    let TipResponse { tip, response } = wallet_api(handle).await?.get_balance(tip, address).await?;
    let balance = response.ok_or_else(|| {
        Error::NotFound("The requested address could not be found in the wallet".into())
    })?;
    Ok(WalletBalanceResponseBody {
        tip,
        balance: balance.balance,
        notes: balance.notes,
        address,
    })
}

pub async fn leader_aged_notes(
    handle: &Handle,
    tip: Option<HeaderId>,
) -> Result<LeaderAgedNotesResponseBody, Error> {
    let TipResponse { tip, response } = wallet_api(handle)
        .await?
        .get_leader_aged_notes_info(tip)
        .await?;
    Ok(LeaderAgedNotesResponseBody {
        tip,
        count: response.count(),
        total_value: response.total_value,
        notes: response
            .notes
            .into_iter()
            .map(|note| LeaderAgedNoteResponseBody {
                note_id: note.note_id,
                value: note.value,
                public_key: note.public_key,
            })
            .collect(),
    })
}

pub async fn claimable_vouchers(
    handle: &Handle,
    tip: Option<HeaderId>,
) -> Result<WalletClaimableVouchersResponseBody, Error> {
    let status = handle.status_watcher::<WalletService>().await?.current();
    if status != ServiceStatus::Ready {
        return Err(Error::unavailable(format!(
            "Wallet service is not ready: {status:?}"
        )));
    }
    let TipResponse { tip, response } = wallet_api(handle)
        .await?
        .get_claimable_vouchers(tip)
        .await?;
    Ok(WalletClaimableVouchersResponseBody {
        tip,
        reward_amount: response.reward_amount,
        total_claimable: response.total_claimable,
        vouchers: response
            .vouchers
            .into_iter()
            .map(|voucher| ClaimableVoucherInfoResponseBody {
                commitment: voucher.commitment,
                nullifier: voucher.nullifier,
            })
            .collect(),
    })
}

/// Builds, signs and submits a transfer to this node's mempool.
pub async fn transfer_funds(
    handle: &Handle,
    request: WalletTransferFundsRequestBody,
) -> Result<WalletTransferFundsResponseBody, Error> {
    let transaction = wallet_api(handle)
        .await?
        .transfer_funds(
            request.tip,
            request.change_public_key,
            request.funding_public_keys,
            request.recipient_public_key,
            request.amount,
        )
        .await?
        .response;
    let hash = transaction.hash();
    mempool::add_tx(handle, transaction).await?;
    Ok(WalletTransferFundsResponseBody { hash })
}

pub async fn sign_tx_ed25519(
    handle: &Handle,
    request: WalletSignTxEd25519RequestBody,
) -> Result<WalletSignTxEd25519ResponseBody, Error> {
    let sig = wallet_api(handle)
        .await?
        .sign_tx_with_ed25519(request.tx_hash, request.pk)
        .await?;
    Ok(WalletSignTxEd25519ResponseBody { sig })
}

pub async fn sign_tx_zk(
    handle: &Handle,
    request: WalletSignTxZkRequestBody,
) -> Result<WalletSignTxZkResponseBody, Error> {
    let sig = wallet_api(handle)
        .await?
        .sign_tx_with_zk(request.tx_hash, request.pks)
        .await?;
    Ok(WalletSignTxZkResponseBody { sig })
}

/// Funds the transaction in `request` and proves the fee transfer it adds,
/// leaving every other op for the caller to sign.
pub async fn fund(
    handle: &Handle,
    request: WalletFundRequestBody,
) -> Result<WalletFundResponseBody, Error> {
    let wallet = wallet_api(handle).await?;
    let TipResponse {
        tip,
        response: funded_tx_builder,
    } = wallet
        .fund_tx(
            request.tip,
            request.tx_builder,
            request.change_public_key,
            request.funding_public_keys,
            request.priority_fee_percent,
        )
        .await?;

    let tx_fee = funded_tx_builder.tx_fee().map_err(Error::internal)?;
    check_max_tx_fee(tx_fee, request.max_tx_fee)?;

    // Owners of the funding inputs, in input order — the ledger
    // verifies the transfer proof against this exact list.
    let funding_note_pks: Vec<ZkPublicKey> = funded_tx_builder
        .ledger_inputs()
        .iter()
        .map(|utxo| utxo.note.pk)
        .collect();

    let funded_tx = funded_tx_builder
        .build(fork_digest_in_force(handle).await?)
        .map_err(Error::internal)?;
    let transfer_proof = if funding_note_pks.is_empty() {
        None
    } else {
        Some(OpProof::ZkSig(
            wallet
                .sign_tx_with_zk(funded_tx.hash(), funding_note_pks)
                .await?,
        ))
    };

    Ok(WalletFundResponseBody {
        tip,
        funded_tx,
        transfer_proof,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_tx_fee_rejects_a_percentage_funded_final_fee() {
        let mandatory_fee: u64 = 794;
        let priority_fee_percent: u64 = 12;
        let priority_fee_amount = (mandatory_fee * priority_fee_percent).div_ceil(100);
        let funded_tx_fee = GasCost::new(mandatory_fee + priority_fee_amount);

        let error = check_max_tx_fee(funded_tx_fee, GasCost::new(mandatory_fee + 95))
            .expect_err("the percentage reserve should be included in the hard cap");
        assert!(error.to_string().contains("exceeds max_tx_fee"));
    }
}
