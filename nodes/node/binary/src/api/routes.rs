//! The node's HTTP API surface, declared once.
//!
//! Every documented endpoint appears exactly once in [`api_routes`]. The table
//! drives both the axum router ([`crate::api::backend`]) and the `OpenAPI` path
//! registration ([`crate::api::openapi`]), so an endpoint cannot be routed
//! without also being documented, nor documented without being routed.
//!
//! Each row is `METHOD PATH => DOC_FN, HANDLER;` where `PATH` is the shared
//! constant the handler's own `#[utoipa::path]` attribute also names, `DOC_FN`
//! is the bare handler path utoipa registers, and `HANDLER` is the fully
//! instantiated function axum routes to. `HANDLER` mentions type parameters
//! that only exist inside `AxumBackend::serve`, so that half of each row is
//! only ever expanded there.
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
            get lb_http_api_common::paths::NODE_VERSION => crate::api::handlers::version, version;
            get lb_http_api_common::paths::MANTLE_METRICS => crate::api::handlers::mantle_metrics, mantle_metrics::<MempoolStorageAdapter, RuntimeServiceId>;
            post lb_http_api_common::paths::MANTLE_STATUS => crate::api::handlers::mantle_status, mantle_status::<MempoolStorageAdapter, RuntimeServiceId>;
            get lb_http_api_common::paths::CRYPTARCHIA_INFO => crate::api::handlers::cryptarchia_info, cryptarchia_info::<RuntimeServiceId>;
            get lb_http_api_common::paths::TIME_INFO => crate::api::handlers::time_info, time_info::<RuntimeServiceId>;
            get lb_http_api_common::paths::CRYPTARCHIA_HEADERS => crate::api::handlers::cryptarchia_headers, cryptarchia_headers::<RuntimeServiceId>;
            get lb_http_api_common::paths::CRYPTARCHIA_LIB_STREAM => crate::api::handlers::cryptarchia_lib_stream, cryptarchia_lib_stream::<RuntimeServiceId>;
            get lb_http_api_common::paths::NETWORK_INFO => crate::api::handlers::libp2p_info, libp2p_info::<RuntimeServiceId>;
            post lb_http_api_common::paths::DIAL_PEER => crate::api::handlers::dial_peer, dial_peer::<RuntimeServiceId>;
            get lb_http_api_common::paths::BLEND_NETWORK_INFO => crate::api::handlers::blend_info, blend_info::<BlendService, RuntimeServiceId>;
            post lb_http_api_common::paths::BLEND_JOIN_NETWORK => crate::api::handlers::blend_join_network, blend_join_network::<BlendService, RuntimeServiceId>;
            get lb_http_api_common::paths::BLEND_PENDING_TRANSACTIONS => crate::api::handlers::blend_pending_transactions, blend_pending_transactions::<BlendService, RuntimeServiceId>;
            post lb_http_api_common::paths::MEMPOOL_ADD_TX => crate::api::handlers::add_tx, add_tx::<MempoolStorageAdapter, RuntimeServiceId>;
            post lb_http_api_common::paths::BLEND_DISPERSE_TRANSACTION => crate::api::handlers::blend_tx, blend_tx::<BlendService, RuntimeServiceId>;
            get lb_http_api_common::paths::MEMPOOL_VIEW => crate::api::handlers::mempool_view, mempool_view::<MempoolStorageAdapter, RuntimeServiceId>;
            get lb_http_api_common::paths::CHANNEL => crate::api::handlers::channel, channel::<RuntimeServiceId>;
            post lb_http_api_common::paths::CHANNEL_DEPOSIT => crate::api::handlers::channel_deposit, channel_deposit::<WalletService, MempoolStorageAdapter, RuntimeServiceId>;
            post lb_http_api_common::paths::SDP_POST_DECLARATION => crate::api::handlers::post_declaration, post_declaration::<SdpMempool, SdpWallet, Cryptarchia<RuntimeServiceId>, SdpStateStorage, RuntimeServiceId>;
            post lb_http_api_common::paths::SDP_POST_ACTIVITY => crate::api::handlers::post_activity, post_activity::<SdpMempool, SdpWallet, Cryptarchia<RuntimeServiceId>, SdpStateStorage, RuntimeServiceId>;
            post lb_http_api_common::paths::SDP_POST_WITHDRAWAL => crate::api::handlers::post_withdrawal, post_withdrawal::<SdpMempool, SdpWallet, Cryptarchia<RuntimeServiceId>, SdpStateStorage, RuntimeServiceId>;
            post lb_http_api_common::paths::SDP_POST_SET_DECLARATION_ID => crate::api::handlers::post_set_declaration_id, post_set_declaration_id::<SdpMempool, SdpWallet, Cryptarchia<RuntimeServiceId>, SdpStateStorage, RuntimeServiceId>;
            get lb_http_api_common::paths::MANTLE_SDP_DECLARATIONS => crate::api::handlers::get_sdp_declarations, get_sdp_declarations::<RuntimeServiceId>;
            get lb_http_api_common::paths::MANTLE_SDP_SNAPSHOT => crate::api::handlers::get_sdp_snapshot, get_sdp_snapshot::<RuntimeServiceId>;
            post lb_http_api_common::paths::LEADER_CLAIM => crate::api::handlers::leader_claim, leader_claim::<ChainLeader, RuntimeServiceId>;
            put lb_http_api_common::paths::POW_START_MINING => crate::api::handlers::pow_start_mining, pow_start_mining::<PoWService, RuntimeServiceId>;
            put lb_http_api_common::paths::POW_STOP_MINING => crate::api::handlers::pow_stop_mining, pow_stop_mining::<PoWService, RuntimeServiceId>;
            put lb_http_api_common::paths::POW_START_AUTO_CLAIM => crate::api::handlers::pow_start_auto_claim, pow_start_auto_claim::<PoWService, RuntimeServiceId>;
            put lb_http_api_common::paths::POW_STOP_AUTO_CLAIM => crate::api::handlers::pow_stop_auto_claim, pow_stop_auto_claim::<PoWService, RuntimeServiceId>;
            post lb_http_api_common::paths::POW_CLAIM => crate::api::handlers::pow_claim, pow_claim::<PoWService, RuntimeServiceId>;
            get lb_http_api_common::paths::POW_CLAIMABLE_REWARDS => crate::api::handlers::pow_claimable_rewards, pow_claimable_rewards::<PoWService, RuntimeServiceId>;
            get lb_http_api_common::paths::LEADER_CLAIM_VOUCHERS => crate::api::handlers::wallet::get_claimable_vouchers, wallet::get_claimable_vouchers::<WalletService, _>;
            get lb_http_api_common::paths::wallet::BALANCE => crate::api::handlers::wallet::get_balance, wallet::get_balance::<WalletService, _>;
            get lb_http_api_common::paths::MANTLE_GAS_PRICES => crate::api::handlers::get_gas_prices, get_gas_prices::<RuntimeServiceId>;
            post lb_http_api_common::paths::wallet::TRANSACTIONS_TRANSFER_FUNDS => crate::api::handlers::wallet::post_transactions_transfer_funds, wallet::post_transactions_transfer_funds::<WalletService, MempoolStorageAdapter, _>;
            post lb_http_api_common::paths::wallet::SIGN_TX_ED25519 => crate::api::handlers::wallet::sign_tx_ed25519, wallet::sign_tx_ed25519::<WalletService, MempoolStorageAdapter, _>;
            post lb_http_api_common::paths::wallet::SIGN_TX_ZK => crate::api::handlers::wallet::sign_tx_zk, wallet::sign_tx_zk::<WalletService, MempoolStorageAdapter, _>;
            post lb_http_api_common::paths::wallet::FUND => crate::api::handlers::wallet::fund, wallet::fund::<WalletService, MempoolStorageAdapter, _>;
            put lb_http_api_common::paths::admin::TRACING_FILTER => crate::api::tracing::reload_tracing_filter, reload_tracing_filter::<RuntimeServiceId>;
            get lb_http_api_common::paths::BLOCKS_STREAM => crate::api::handlers::blocks_stream, blocks_stream::<BlockStorageBackend, CryptarchiaConsensus<_, _, _, _>, RuntimeServiceId>;
            get lb_http_api_common::paths::BLOCKS_RANGE_STREAM => crate::api::handlers::blocks_range_stream, blocks_range_stream::<BlockStorageBackend, RuntimeServiceId>;
            get lb_http_api_common::paths::BLOCKS => crate::api::handlers::immutable_blocks, immutable_blocks::<BlockStorageBackend, RuntimeServiceId>;
            get lb_http_api_common::paths::BLOCKS_DETAIL => crate::api::handlers::block, block::<StorageAdapter, RuntimeServiceId>;
            get lb_http_api_common::paths::BLOCK_EVENTS => crate::api::handlers::block_events, block_events::<RuntimeServiceId>;
            get lb_http_api_common::paths::TRANSACTION => crate::api::handlers::transaction, transaction::<StorageAdapter, RuntimeServiceId>;
        }
    };
}

pub(crate) use api_routes;
