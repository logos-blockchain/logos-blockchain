use std::path::Path;

use lb_common_http_client::CommonHttpClient;
use lb_http_api_common::bodies::wallet::transfer_funds::{
    WalletTransferFundsRequestBody, WalletTransferFundsResponseBody,
};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_node::{
    UserConfig,
    config::{OnUnknownKeys, deserialize_config_at_path},
};
use reqwest::Url;

pub struct Faucet {
    faucet_pk: ZkPublicKey,
    http_client: CommonHttpClient,
    base_url: Url,
    drip_rate: u64, // Percentage from the faucet balance to send to the receiver.
}

impl Faucet {
    pub fn new(node_config_path: &Path, drip_rate: u64) -> Result<Self, String> {
        if drip_rate > 100 {
            return Err("Drip percentage cannot exceed 100%".to_owned());
        }

        let user_config =
            deserialize_config_at_path::<UserConfig>(node_config_path, OnUnknownKeys::Warn)
                .map_err(|e| format!("Failed to deserialize node config: {e}"))?;

        let (_, faucet_pk) = user_config
            .wallet
            .known_keys
            .into_iter()
            .find(|(id, _)| *id != user_config.wallet.voucher_master_key_id)
            .ok_or_else(|| {
                "Faucet config contains no usable keys (only master key found or empty)".to_owned()
            })?;

        let base_url = Url::parse(&format!(
            "http://{}",
            user_config.http.backend_settings.address
        ))
        .map_err(|e| format!("Invalid node address in config: {e}"))?;

        let http_client = CommonHttpClient::new(None);

        Ok(Self {
            faucet_pk,
            http_client,
            base_url,
            drip_rate,
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

        let amount_to_send = current_balance
            .checked_mul(self.drip_rate)
            .map_or(0, |val| val / 100);

        if amount_to_send == 0 {
            return Err(format!(
                "Balance too low to drip (Current: {current_balance}, 5%: 0)"
            ));
        }

        println!(
            "Dripping {}% of balance: {amount_to_send} units to {recipient_pk:?}",
            self.drip_rate
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
