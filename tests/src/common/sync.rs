use std::time::Duration;

use futures_util::{StreamExt as _, stream};
use lb_chain_service::CryptarchiaInfo;
use lb_zone_sdk::Slot;
use tokio::time::timeout;

use crate::nodes::validator::Validator;

pub async fn wait_for_validators_mode_and_height(
    validators: &[Validator],
    mode: lb_cryptarchia_engine::State,
    min_height: u64,
    timeout_duration: Duration,
) {
    wait_for_validators(
        validators,
        timeout_duration,
        |info| info.mode == mode && info.height >= min_height,
        format!("All validators reached are in mode {mode:?} and height {min_height}").as_str(),
        format!("Failed to wait for validators to reach mode {mode:?} and height {min_height}")
            .as_str(),
    )
    .await;
}

pub async fn wait_for_validators_mode_and_slot(
    validators: &[Validator],
    mode: lb_cryptarchia_engine::State,
    min_slot: Slot,
    timeout_duration: Duration,
) {
    wait_for_validators(
        validators,
        timeout_duration,
        |info| info.mode == mode && info.slot >= min_slot,
        format!("All validators reached are in mode {mode:?} and slot {min_slot:?}").as_str(),
        format!("Failed to wait for validators to reach mode {mode:?} and slot {min_slot:?}")
            .as_str(),
    )
    .await;
}

pub async fn wait_for_validators_mode(
    validators: &[Validator],
    mode: lb_cryptarchia_engine::State,
    timeout_duration: Duration,
) {
    wait_for_validators(
        validators,
        timeout_duration,
        |info| info.mode == mode,
        format!("All validators reached are in mode {mode:?}").as_str(),
        format!("Failed to wait for validators to reach mode {mode:?}").as_str(),
    )
    .await;
}

async fn wait_for_validators(
    validators: &[Validator],
    timeout_duration: Duration,
    criteria: impl Fn(&CryptarchiaInfo) -> bool + Send + Sync,
    success_msg: &str,
    failure_msg: &str,
) {
    timeout(timeout_duration, async {
        loop {
            let infos: Vec<_> = stream::iter(validators)
                .then(async |n| n.consensus_info(false).await)
                .collect()
                .await;
            print_validators_info(&infos);

            if infos.iter().all(&criteria) {
                println!("{success_msg}");
                return;
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("Timeout ({timeout_duration:?}): {failure_msg}"));
}

fn print_validators_info(infos: &[CryptarchiaInfo]) {
    println!("   Validators: {:?}", format_cryptarhica_info(infos));
}

#[must_use]
pub fn format_cryptarhica_info(infos: &[CryptarchiaInfo]) -> Vec<String> {
    infos
        .iter()
        .map(|info| format!("Height({})/{:?}/{:?}", info.height, info.slot, info.mode))
        .collect()
}
