pub const MANTLE_METRICS: &str = "/mantle/metrics";
pub const MANTLE_STATUS: &str = "/mantle/status";
pub const MANTLE_SDP_DECLARATIONS: &str = "/mantle/sdp/declarations";
pub const CRYPTARCHIA_INFO: &str = "/cryptarchia/info";
pub const CRYPTARCHIA_HEADERS: &str = "/cryptarchia/headers";
pub const CRYPTARCHIA_LIB_STREAM: &str = "/cryptarchia/lib-stream";
pub const NETWORK_INFO: &str = "/network/info";
pub const STORAGE_BLOCK: &str = "/storage/block";
pub const MEMPOOL_ADD_TX: &str = "/mempool/add/tx";
pub const CHANNEL: &str = "/channel/:id";
pub const CHANNEL_DEPOSIT: &str = "/channel/deposit";
pub const SDP_POST_DECLARATION: &str = "/sdp/declaration";
pub const SDP_POST_ACTIVITY: &str = "/sdp/activity";
pub const SDP_POST_WITHDRAWAL: &str = "/sdp/withdrawal";
pub const LEADER_CLAIM: &str = "/leader/claim";

pub const BLOCKS: &str = "/cryptarchia/blocks";
pub const BLOCKS_STREAM: &str = "/cryptarchia/events/blocks/stream";

pub mod wallet {
    pub const BALANCE: &str = "/wallet/:public_key/balance";
    pub const TRANSACTIONS_TRANSFER_FUNDS: &str = "/wallet/transactions/transfer-funds";
}

// testing paths
pub const UPDATE_MEMBERSHIP: &str = "/test/membership/update";
