use std::fmt::{Debug, Display};

use nomos_core::{
    header::HeaderId,
    mantle::{Note, SignedMantleTx, Value, tx_builder::MantleTxBuilder},
};
use nomos_wallet::{WalletMsg, WalletService, WalletServiceError};
use overwatch::{
    overwatch::OverwatchHandle,
    services::{AsServiceId, relay::RelayError},
};
use tokio::sync::oneshot;
use zksign::PublicKey;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Relay(RelayError),
    #[error(transparent)]
    Recv(oneshot::error::RecvError),
    #[error(transparent)]
    Service(WalletServiceError),
}

/// Get the balance of a wallet address.
///
/// # Arguments
///
/// - `handle`: An [`OverwatchHandle`] to communicate with the wallet service.
/// - `tip`: The header ID at which to query the balance.
/// - `wallet_address`: The public key of the wallet address.
///
/// # Returns
///
/// A `Result` containing an `Option<Value>` representing the balance on
/// success, where `None` indicates that the address does not exist, or an error
/// on failure.
pub async fn get_balance<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
    tip: HeaderId,
    wallet_address: PublicKey,
) -> Result<Option<Value>, Error>
where
    RuntimeServiceId: Debug
        + Display
        + Sync
        + AsServiceId<WalletService<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId>>,
{
    let relay = handle.relay().await.map_err(Error::Relay)?;
    let (sender, receiver) = oneshot::channel();

    relay
        .send(WalletMsg::GetBalance {
            tip,
            pk: wallet_address,
            resp_tx: sender,
        })
        .await
        .map_err(|(error, _)| Error::Relay(error))?;

    receiver.await.map_err(Error::Recv)?.map_err(Error::Service)
}

/// Transfer funds from some addresses to another.
///
/// # Arguments
///
/// - `handle`: An [`OverwatchHandle`] to communicate with the wallet service.
/// - `tip`: The header ID at which to perform the transfer.
/// - `change_public_key`: The public key to receive any change from the
///   transaction.
/// - `funding_public_keys`: A vector of public keys to fund the transaction.
/// - `recipient_public_key`: The public key of the recipient.
/// - `amount`: The amount to transfer.
///
/// # Returns
/// A `Result` containing the signed transaction on success, or an error on
/// failure.
pub async fn transfer_funds<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
    tip: HeaderId,
    change_public_key: PublicKey,
    funding_public_keys: Vec<PublicKey>,
    recipient_public_key: PublicKey,
    amount: u64,
) -> Result<SignedMantleTx, Error>
where
    RuntimeServiceId: Debug
        + Display
        + Sync
        + AsServiceId<WalletService<Kms, Cryptarchia, Tx, Storage, RuntimeServiceId>>,
{
    let relay = handle.relay().await.map_err(Error::Relay)?;
    let (sender, receiver) = oneshot::channel();
    let transaction_builder =
        MantleTxBuilder::new().add_ledger_output(Note::new(amount, recipient_public_key));

    relay
        .send(WalletMsg::FundAndSignTx {
            tip,
            tx_builder: transaction_builder,
            change_pk: change_public_key,
            funding_pks: funding_public_keys,
            resp_tx: sender,
        })
        .await
        .map_err(|(error, _)| Error::Relay(error))?;

    receiver.await.map_err(Error::Recv)?.map_err(Error::Service)
}
