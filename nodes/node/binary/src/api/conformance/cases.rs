//! The conformance cases, one or more per route.

use lb_core::{
    mantle::{
        SignedOps,
        ledger::verification_mode::StandardMode,
        transactions::{builder::MantleTxBuilder, states::Preverified},
    },
    sdp::{ActiveMessage, DeclarationMessage},
};
use lb_http_api_common::paths;
use serde_json::{Value, json};

use super::{
    Case,
    stubs::{ED25519_PUBLIC_KEY, missing, zero_hex},
};

fn to_json(value: impl serde::Serialize) -> Value {
    serde_json::to_value(value).expect("fixture serializes")
}

fn tx() -> Value {
    to_json(SignedOps::<Preverified, StandardMode>::empty())
}

fn with_id(route: &str, id: &str) -> String {
    route.replace(":id", id).replace(":public_key", id)
}

#[expect(clippy::too_many_lines, reason = "one entry per route")]
pub fn all() -> Vec<Case> {
    let zero = zero_hex();
    vec![
        // Node
        Case::new("get", paths::NODE_VERSION),
        Case::new("get", paths::CHAIN_ID),
        Case::new("put", paths::admin::TRACING_FILTER).body(json!({"filters": {"*": "info"}})),
        // Chain
        Case::new("get", paths::CRYPTARCHIA_INFO),
        Case::new("get", paths::CRYPTARCHIA_HEADERS),
        Case::new("get", paths::CRYPTARCHIA_HEADERS).uri(format!(
            "{}?from={zero}&to={zero}",
            paths::CRYPTARCHIA_HEADERS
        )),
        Case::new("get", paths::CRYPTARCHIA_LIB_STREAM),
        Case::new("get", paths::TIME_INFO),
        Case::new("get", paths::BLOCKS).uri(format!("{}?slot_from=0&slot_to=10", paths::BLOCKS)),
        Case::new("get", paths::BLOCKS_DETAIL).uri(with_id(paths::BLOCKS_DETAIL, &zero)),
        Case::new("get", paths::BLOCKS_DETAIL)
            .uri(with_id(paths::BLOCKS_DETAIL, missing::HEADER_ID))
            .status(404),
        Case::new("get", paths::BLOCK_EVENTS).uri(with_id(paths::BLOCK_EVENTS, &zero)),
        Case::new("get", paths::BLOCK_EVENTS)
            .uri(with_id(paths::BLOCK_EVENTS, missing::HEADER_ID))
            .status(404),
        Case::new("get", paths::BLOCKS_STREAM),
        Case::new("get", paths::BLOCKS_RANGE_STREAM).uri(format!(
            "{}?block_filter=immutable_only&blocks_limit=1",
            paths::BLOCKS_RANGE_STREAM
        )),
        Case::new("get", paths::BLOCKS_RANGE_STREAM)
            .uri(format!(
                "{}?slot_from=10&slot_to=5&order=ascending",
                paths::BLOCKS_RANGE_STREAM
            ))
            .status(400),
        Case::new("get", paths::TRANSACTION).uri(with_id(paths::TRANSACTION, &zero)),
        Case::new("get", paths::TRANSACTION)
            .uri(with_id(paths::TRANSACTION, missing::HEADER_ID))
            .status(404),
        // Mantle and mempool
        Case::new("get", paths::MANTLE_METRICS),
        Case::new("post", paths::MANTLE_STATUS).body(json!([zero])),
        Case::new("get", paths::MANTLE_GAS_PRICES),
        Case::new("get", paths::MANTLE_GAS_PRICES)
            .uri(format!(
                "{}?tip={}",
                paths::MANTLE_GAS_PRICES,
                missing::HEADER_ID
            ))
            .status(404),
        Case::new("get", paths::MANTLE_SDP_DECLARATIONS),
        Case::new("get", paths::MANTLE_SDP_SNAPSHOT),
        Case::new("post", paths::MEMPOOL_ADD_TX).body(tx()),
        Case::new("get", paths::MEMPOOL_VIEW),
        // Channels
        Case::new("get", paths::CHANNEL).uri(with_id(paths::CHANNEL, &zero)),
        Case::new("get", paths::CHANNEL)
            .uri(with_id(paths::CHANNEL, missing::HEADER_ID))
            .status(404),
        Case::new("post", paths::CHANNEL_DEPOSIT).body(json!({
            "tip": null,
            "deposit": {"channel_id": zero, "inputs": [], "metadata": []},
            "change_public_key": zero,
            "funding_public_keys": [],
            "max_tx_fee": 0,
        })),
        // Network
        Case::new("get", paths::NETWORK_INFO),
        Case::new("post", paths::DIAL_PEER)
            .body(json!({"addr": "/ip4/127.0.0.1/udp/3000/quic-v1"})),
        // Blend
        Case::new("get", paths::BLEND_NETWORK_INFO),
        Case::new("post", paths::BLEND_JOIN_NETWORK).body(json!({
            "locator": "/ip4/127.0.0.1/udp/3000/quic-v1",
            "service_note_id": zero,
        })),
        Case::new("get", paths::BLEND_PENDING_TRANSACTIONS),
        Case::new("post", paths::BLEND_DISPERSE_TRANSACTION).body(tx()),
        // SDP
        Case::new("post", paths::SDP_POST_DECLARATION).body(to_json(DeclarationMessage::sample())),
        Case::new("post", paths::SDP_POST_ACTIVITY).body(to_json(ActiveMessage::sample().metadata)),
        Case::new("post", paths::SDP_POST_WITHDRAWAL).body(json!(zero)),
        Case::new("post", paths::SDP_POST_SET_DECLARATION_ID).body(json!(zero)),
        Case::new("post", paths::SDP_POST_SET_DECLARATION_ID).body(Value::Null),
        // Leader
        Case::new("post", paths::LEADER_CLAIM),
        Case::new("get", paths::LEADER_CLAIM_VOUCHERS),
        Case::new("get", paths::LEADER_CLAIM_VOUCHERS)
            .uri(format!("{}?tip={zero}", paths::LEADER_CLAIM_VOUCHERS)),
        Case::new("get", paths::LEADER_AGED_NOTES),
        Case::new("get", paths::LEADER_AGED_NOTES)
            .uri(format!("{}?tip={zero}", paths::LEADER_AGED_NOTES)),
        // Proof of work
        Case::new("put", paths::POW_START_MINING),
        Case::new("put", paths::POW_STOP_MINING),
        Case::new("put", paths::POW_START_AUTO_CLAIM),
        Case::new("put", paths::POW_STOP_AUTO_CLAIM),
        Case::new("post", paths::POW_CLAIM),
        Case::new("post", paths::POW_CLAIM).body(json!({"claim_address": zero})),
        Case::new("get", paths::POW_CLAIMABLE_REWARDS),
        Case::new("get", paths::POW_STATUS),
        // Wallet
        Case::new("get", paths::wallet::BALANCE).uri(with_id(paths::wallet::BALANCE, &zero)),
        Case::new("get", paths::wallet::BALANCE).uri(format!(
            "{}?tip={zero}",
            with_id(paths::wallet::BALANCE, &zero)
        )),
        Case::new("post", paths::wallet::TRANSACTIONS_TRANSFER_FUNDS)
            .body(json!({
                "tip": null,
                "change_public_key": zero,
                "funding_public_keys": [],
                "recipient_public_key": zero,
                "amount": 1,
            }))
            .status(201),
        Case::new("post", paths::wallet::SIGN_TX_ED25519)
            .body(json!({"tx_hash": zero, "pk": ED25519_PUBLIC_KEY})),
        Case::new("post", paths::wallet::SIGN_TX_ZK).body(json!({"tx_hash": zero, "pks": [zero]})),
        Case::new("post", paths::wallet::FUND).body(json!({
            "tip": null,
            "tx_builder": to_json(MantleTxBuilder::new()),
            "change_public_key": zero,
            "funding_public_keys": [],
            "max_tx_fee": 0,
        })),
    ]
}
