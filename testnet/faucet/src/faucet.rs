use lb_common_http_client::CommonHttpClient;
use lb_http_api_common::bodies::wallet::transfer_funds::{
    WalletTransferFundsRequestBody, WalletTransferFundsResponseBody,
};
use lb_key_management_system_keys::keys::ZkPublicKey;
use reqwest::Url;

pub struct Faucet {
    faucet_pk: ZkPublicKey,
    drip_amount: u64,
    http_client: CommonHttpClient,
    base_url: Url,
}

impl Faucet {
    pub fn new(base_url: Url, faucet_pk: ZkPublicKey, drip_amount: u64) -> Result<Self, String> {
        let http_client = CommonHttpClient::new(None);

        Ok(Self {
            faucet_pk,
            drip_amount,
            http_client,
            base_url,
        })
    }

    pub async fn transfer_to_pk(
        &self,
        recipient_pk: ZkPublicKey,
    ) -> Result<WalletTransferFundsResponseBody, String> {
        let balance_info = self
            .http_client
            .get_wallet_balance(self.base_url.clone(), self.faucet_pk, None)
            .await
            .map_err(|e| format!("Failed to fetch faucet balance: {e}"))?;

        let current_balance = balance_info.balance;

        let amount_to_send = std::cmp::min(current_balance, self.drip_amount);
        if amount_to_send == 0 {
            return Err(format!(
                "Balance too low to drip (Current: {current_balance}, ask for direct transfter in discord)"
            ));
        }

        println!(
            "Dripping {}% of balance: {amount_to_send} units to {recipient_pk:?}",
            self.drip_amount
        );

        let body = WalletTransferFundsRequestBody {
            tip: None,
            change_public_key: self.faucet_pk,
            funding_public_keys: vec![self.faucet_pk],
            recipient_public_key: recipient_pk,
            amount: amount_to_send,
        };

        self.http_client
            .transfer_funds(self.base_url.clone(), body)
            .await
            .map_err(|e| format!("Faucet transfer failed: {e}"))
    }
}
