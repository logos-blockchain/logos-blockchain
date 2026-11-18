use lb_binary_codec::bincode::SerializeOp as _;
use lb_common_http_client::{BasicAuthCredentials, CommonHttpClient, Error};
use lb_core::header::HeaderId;
use lb_http_api_common::{
    bodies::{
        NoopBody,
        wallet::{
            balance::WalletBalanceResponseBody,
            delivery::{
                WalletAddressResponseBody, WalletImportDeliveryMessagesRequestBody,
                WalletImportDeliveryMessagesResponseBody, WalletNameStakingKeyRequestBody,
                WalletRedeliverRequestBody, WalletRedeliverResponseBody,
                WalletTransferFundsToAddressRequestBody, WalletTransferFundsToAddressResponseBody,
            },
            transfer_funds::{WalletTransferFundsRequestBody, WalletTransferFundsResponseBody},
        },
    },
    paths,
};
use lb_key_management_system_keys::keys::ZkPublicKey;
use url::Url;

pub struct WalletHttpClient {
    client: CommonHttpClient,
}

impl WalletHttpClient {
    #[must_use]
    pub fn new(basic_auth: Option<BasicAuthCredentials>) -> Self {
        Self {
            client: CommonHttpClient::new(basic_auth),
        }
    }

    fn build_get_balance_url(
        base_url: &Url,
        wallet_address: ZkPublicKey,
        tip: Option<HeaderId>,
    ) -> Result<Url, Error> {
        let Ok(wallet_address) = wallet_address.to_bytes() else {
            return Err(Error::Client(String::from(
                "The wallet address is not a valid public key.",
            )));
        };
        let wallet_address = hex::encode(wallet_address.iter().as_slice());
        let path = paths::wallet::BALANCE.replace("{public_key}", wallet_address.as_str());
        let mut url = base_url.join(path.as_str()).map_err(Error::Url)?;
        if let Some(tip) = tip {
            let Ok(tip) = tip.to_bytes() else {
                return Err(Error::Client(String::from(
                    "The tip is not a valid header id.",
                )));
            };
            let tip = hex::encode(tip);
            let tip_query = format!("tip={tip}");
            url.set_query(Some(tip_query.as_str()));
        }
        Ok(url)
    }

    pub async fn get_balance(
        self,
        base_url: &Url,
        wallet_address: ZkPublicKey,
        tip: Option<HeaderId>,
    ) -> Result<WalletBalanceResponseBody, Error> {
        let url = Self::build_get_balance_url(base_url, wallet_address, tip)?;
        self.client.get(url, Option::<&NoopBody>::None).await
    }

    pub async fn transfer_funds(
        self,
        base_url: Url,
        body: WalletTransferFundsRequestBody,
    ) -> Result<WalletTransferFundsResponseBody, Error> {
        let url = base_url
            .join(paths::wallet::TRANSACTIONS_TRANSFER_FUNDS)
            .map_err(Error::Url)?;
        self.client.post(url, &body).await
    }

    /// The address senders pay `wallet_address` at: its Bech32m string, with
    /// the key note ciphertexts are encrypted to.
    pub async fn get_address(
        self,
        base_url: &Url,
        wallet_address: ZkPublicKey,
    ) -> Result<WalletAddressResponseBody, Error> {
        let Ok(wallet_address) = wallet_address.to_bytes() else {
            return Err(Error::Client(String::from(
                "The wallet address is not a valid public key.",
            )));
        };
        let wallet_address = hex::encode(wallet_address.iter().as_slice());
        let path = paths::wallet::ADDRESS.replace("{public_key}", wallet_address.as_str());
        let url = base_url.join(path.as_str()).map_err(Error::Url)?;
        self.client.get(url, Option::<&NoopBody>::None).await
    }

    /// Transfers funds to an address, and gets the delivery message of the
    /// note the transfer pays.
    pub async fn transfer_funds_to_address(
        self,
        base_url: Url,
        body: WalletTransferFundsToAddressRequestBody,
    ) -> Result<WalletTransferFundsToAddressResponseBody, Error> {
        let url = base_url
            .join(paths::wallet::TRANSACTIONS_TRANSFER_FUNDS_TO_ADDRESS)
            .map_err(Error::Url)?;
        self.client.post(url, &body).await
    }

    /// Hands the wallet delivery messages of notes paid to it.
    pub async fn import_delivery_messages(
        self,
        base_url: Url,
        body: WalletImportDeliveryMessagesRequestBody,
    ) -> Result<WalletImportDeliveryMessagesResponseBody, Error> {
        let url = base_url
            .join(paths::wallet::IMPORT_DELIVERY_MESSAGES)
            .map_err(Error::Url)?;
        self.client.post(url, &body).await
    }

    /// Asks the wallet to deliver again a note it paid.
    pub async fn redeliver_note(
        self,
        base_url: Url,
        body: WalletRedeliverRequestBody,
    ) -> Result<WalletRedeliverResponseBody, Error> {
        let url = base_url
            .join(paths::wallet::REDELIVER_NOTE)
            .map_err(Error::Url)?;
        self.client.post(url, &body).await
    }

    /// Rebuilds the wallet over the whole chain, from the genesis block.
    pub async fn rescan(self, base_url: Url) -> Result<(), Error> {
        let url = base_url.join(paths::wallet::RESCAN).map_err(Error::Url)?;
        self.client.post(url, &NoopBody).await
    }

    /// Names a staking key the wallet asked a sender to use.
    pub async fn name_staking_key(
        self,
        base_url: Url,
        body: WalletNameStakingKeyRequestBody,
    ) -> Result<(), Error> {
        let url = base_url
            .join(paths::wallet::NAME_STAKING_KEY)
            .map_err(Error::Url)?;
        self.client.post(url, &body).await
    }
}

#[cfg(test)]
mod tests {
    use lb_binary_codec::bincode::DeserializeOp as _;

    use super::*;

    fn get_ordered_hex_array() -> [u8; 32] {
        std::array::from_fn(|i| (i % 16) as u8)
    }

    fn get_wallet_address() -> ZkPublicKey {
        ZkPublicKey::from_bytes(get_ordered_hex_array().as_slice()).unwrap()
    }

    #[test]
    fn get_balance_url() {
        let base_url = Url::parse("http://localhost:8080").unwrap();
        let wallet_address = get_wallet_address();

        let url = WalletHttpClient::build_get_balance_url(&base_url, wallet_address, None).unwrap();
        assert_eq!(
            url.as_str(),
            "http://localhost:8080/wallet/000102030405060708090a0b0c0d0e0f000102030405060708090a0b0c0d0e0f/balance"
        );

        let tip = {
            let header_id = get_ordered_hex_array()
                .into_iter()
                .rev()
                .collect::<Vec<u8>>();
            HeaderId::from_bytes(header_id.as_slice()).unwrap()
        };
        let url =
            WalletHttpClient::build_get_balance_url(&base_url, wallet_address, Some(tip)).unwrap();
        assert_eq!(
            url.as_str(),
            "http://localhost:8080/wallet/000102030405060708090a0b0c0d0e0f000102030405060708090a0b0c0d0e0f/balance?tip=0f0e0d0c0b0a090807060504030201000f0e0d0c0b0a09080706050403020100"
        );
    }
}
