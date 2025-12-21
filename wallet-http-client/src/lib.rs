use common_http_client::{BasicAuthCredentials, CommonHttpClient, Error};
use key_management_system_keys::keys::ZkPublicKey;
use nomos_core::{codec::SerializeOp as _, header::HeaderId};
use nomos_http_api_common::{
    bodies::{
        NoopBody,
        wallet::{
            balance::WalletBalanceResponseBody,
            transfer_funds::{WalletTransferFundsRequestBody, WalletTransferFundsResponseBody},
        },
    },
    paths,
};
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
        let path = paths::wallet::BALANCE.replace(":public_key", wallet_address.as_str());
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
}

#[cfg(test)]
mod tests {
    use nomos_core::codec::DeserializeOp as _;

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
