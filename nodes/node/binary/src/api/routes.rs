//! The node's HTTP API surface, declared once.
//!
//! Every documented endpoint appears exactly once in [`api_routes`]. The table
//! drives both the axum router ([`crate::api::backend`]) and the `OpenAPI` path
//! registration ([`crate::api::openapi`]), so an endpoint cannot be routed
//! without also being documented, nor documented without being routed.
//!
//! Each row is `METHOD PATH => HANDLER;` where `PATH` is the shared constant
//! the handler's own `#[utoipa::path]` attribute also names and `HANDLER` is
//! the function axum routes to and utoipa registers.
//!
//! To add an endpoint: annotate the handler with `#[utoipa::path]` and add one
//! row here. Nothing else needs touching.
//!
//! The row's method and the `#[utoipa::path]` method are still written
//! separately — utoipa reads the latter off the handler itself. They are
//! cross-checked by `crate::api::openapi` tests.

/// Expands `$consumer!` with the full route table.
///
/// See the module documentation for the row format.
macro_rules! api_routes {
    ($consumer:ident) => {
        $consumer! {
            get lb_api_common::paths::NODE_VERSION => crate::api::handlers::node::version;
            get lb_api_common::paths::MANTLE_METRICS => crate::api::handlers::mempool::mantle_metrics;
            post lb_api_common::paths::MANTLE_STATUS => crate::api::handlers::mempool::mantle_status;
            get lb_api_common::paths::CHAIN_ID => crate::api::handlers::node::chain_id;
            get lb_api_common::paths::CHAIN_FORK => crate::api::handlers::cryptarchia::chain_fork;
            get lb_api_common::paths::CRYPTARCHIA_INFO => crate::api::handlers::cryptarchia::cryptarchia_info;
            get lb_api_common::paths::TIME_INFO => crate::api::handlers::time::time_info;
            get lb_api_common::paths::CRYPTARCHIA_HEADERS => crate::api::handlers::blocks::cryptarchia_headers;
            get lb_api_common::paths::CRYPTARCHIA_LIB_STREAM => crate::api::handlers::blocks::cryptarchia_lib_stream;
            get lb_api_common::paths::NETWORK_INFO => crate::api::handlers::network::libp2p_info;
            post lb_api_common::paths::DIAL_PEER => crate::api::handlers::network::dial_peer;
            get lb_api_common::paths::BLEND_NETWORK_INFO => crate::api::handlers::blend::blend_info;
            post lb_api_common::paths::BLEND_JOIN_NETWORK => crate::api::handlers::blend::blend_join_network;
            get lb_api_common::paths::BLEND_PENDING_TRANSACTIONS => crate::api::handlers::blend::blend_pending_transactions;
            post lb_api_common::paths::MEMPOOL_ADD_TX => crate::api::handlers::mempool::add_tx;
            post lb_api_common::paths::BLEND_DISPERSE_TRANSACTION => crate::api::handlers::blend::blend_tx;
            get lb_api_common::paths::MEMPOOL_VIEW => crate::api::handlers::mempool::mempool_view;
            get lb_api_common::paths::CHANNEL => crate::api::handlers::channel::channel;
            post lb_api_common::paths::CHANNEL_DEPOSIT => crate::api::handlers::channel::channel_deposit;
            post lb_api_common::paths::SDP_POST_DECLARATION => crate::api::handlers::sdp::post_declaration;
            post lb_api_common::paths::SDP_POST_ACTIVITY => crate::api::handlers::sdp::post_activity;
            post lb_api_common::paths::SDP_POST_WITHDRAWAL => crate::api::handlers::sdp::post_withdrawal;
            post lb_api_common::paths::SDP_POST_SET_DECLARATION_ID => crate::api::handlers::sdp::post_set_declaration_id;
            get lb_api_common::paths::MANTLE_SDP_DECLARATIONS => crate::api::handlers::sdp::get_sdp_declarations;
            get lb_api_common::paths::MANTLE_SDP_SNAPSHOT => crate::api::handlers::sdp::get_sdp_snapshot;
            get lb_api_common::paths::MANTLE_SDP_FINALIZED_DECLARATION => crate::api::handlers::sdp::get_finalized_sdp_declaration;
            get lb_api_common::paths::MANTLE_SDP_FINALIZED_DECLARATIONS => crate::api::handlers::sdp::get_finalized_sdp_declarations;
            post lb_api_common::paths::LEADER_CLAIM => crate::api::handlers::leader::leader_claim;
            put lb_api_common::paths::POW_START_MINING => crate::api::handlers::pow::pow_start_mining;
            put lb_api_common::paths::POW_STOP_MINING => crate::api::handlers::pow::pow_stop_mining;
            put lb_api_common::paths::POW_START_AUTO_CLAIM => crate::api::handlers::pow::pow_start_auto_claim;
            put lb_api_common::paths::POW_STOP_AUTO_CLAIM => crate::api::handlers::pow::pow_stop_auto_claim;
            post lb_api_common::paths::POW_CLAIM => crate::api::handlers::pow::pow_claim;
            get lb_api_common::paths::POW_CLAIMABLE_REWARDS => crate::api::handlers::pow::pow_claimable_rewards;
            get lb_api_common::paths::POW_STATUS => crate::api::handlers::pow::pow_status;
            get lb_api_common::paths::LEADER_CLAIM_VOUCHERS => crate::api::handlers::wallet::get_claimable_vouchers;
            get lb_api_common::paths::LEADER_AGED_NOTES => crate::api::handlers::wallet::get_leader_aged_notes;
            get lb_api_common::paths::wallet::BALANCE => crate::api::handlers::wallet::get_balance;
            get lb_api_common::paths::MANTLE_GAS_PRICES => crate::api::handlers::cryptarchia::get_gas_prices;
            get lb_api_common::paths::MANTLE_LIMITS => crate::api::handlers::cryptarchia::get_limits;
            post lb_api_common::paths::wallet::TRANSACTIONS_TRANSFER_FUNDS => crate::api::handlers::wallet::post_transactions_transfer_funds;
            post lb_api_common::paths::wallet::SIGN_TX_ED25519 => crate::api::handlers::wallet::sign_tx_ed25519;
            post lb_api_common::paths::wallet::SIGN_TX_ZK => crate::api::handlers::wallet::sign_tx_zk;
            post lb_api_common::paths::wallet::FUND => crate::api::handlers::wallet::fund;
            put lb_api_common::paths::admin::TRACING_FILTER => crate::api::handlers::node::reload_tracing_filter;
            get lb_api_common::paths::BLOCKS_STREAM => crate::api::handlers::blocks::blocks_stream;
            get lb_api_common::paths::BLOCKS_RANGE_STREAM => crate::api::handlers::blocks::blocks_range_stream;
            get lb_api_common::paths::BLOCKS => crate::api::handlers::blocks::immutable_blocks;
            get lb_api_common::paths::BLOCKS_DETAIL => crate::api::handlers::blocks::block;
            get lb_api_common::paths::BLOCK_EVENTS => crate::api::handlers::blocks::block_events;
            get lb_api_common::paths::TRANSACTION => crate::api::handlers::blocks::transaction;
        }
    };
}

pub(crate) use api_routes;
