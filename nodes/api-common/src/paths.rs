pub const MANTLE_METRICS: &str = "/mantle/metrics";
pub const MANTLE_STATUS: &str = "/mantle/status";
pub const MANTLE_SDP_DECLARATIONS: &str = "/mantle/sdp/declarations";
pub const CRYPTARCHIA_INFO: &str = "/cryptarchia/info";
pub const CRYPTARCHIA_HEADERS: &str = "/cryptarchia/headers";
pub const CRYPTARCHIA_LIB_STREAM: &str = "/cryptarchia/lib-stream";
pub const DA_ADD_SHARE: &str = "/da/add-share";
pub const DA_GET_SHARES_COMMITMENTS: &str = "/da/get-commitments";
pub const DA_GET_STORAGE_SHARES_COMMITMENTS: &str = "/da/get-storage-commitments";
pub const DA_GET_LIGHT_SHARE: &str = "/da/get-share";
pub const DA_GET_SHARES: &str = "/da/sampling/shares";
pub const DA_BLOCK_PEER: &str = "/da/block-peer";
pub const DA_UNBLOCK_PEER: &str = "/da/unblock-peer";
pub const DA_BLACKLISTED_PEERS: &str = "/da/blacklisted-peers";
pub const DA_BALANCER_STATS: &str = "/da/balancer-stats";
pub const DA_MONITOR_STATS: &str = "/da/monitor-stats";
pub const NETWORK_INFO: &str = "/network/info";
pub const STORAGE_BLOCK: &str = "/storage/block";
pub const MEMPOOL_ADD_TX: &str = "/mempool/add/tx";
pub const DISPERSE_DATA: &str = "/disperse-data";
pub const SDP_POST_DECLARATION: &str = "/sdp/declaration";
pub const SDP_POST_ACTIVITY: &str = "/sdp/activity";
pub const SDP_POST_WITHDRAWAL: &str = "/sdp/withdrawal";

pub const BLOCKS: &str = "/cryptarchia/blocks";
pub const BLOCKS_STREAM: &str = "/cryptarchia/blocks/stream";

pub mod wallet {
    pub const BALANCE: &str = "/wallet/:public_key/balance";
    pub const TRANSACTIONS_TRANSFER_FUNDS: &str = "/wallet/transactions/transfer-funds";
}

// testing paths
pub const UPDATE_MEMBERSHIP: &str = "/test/membership/update";
pub const DA_GET_MEMBERSHIP: &str = "/test/da/membership";
pub const DA_HISTORIC_SAMPLING: &str = "/test/da/historic-sampling";
