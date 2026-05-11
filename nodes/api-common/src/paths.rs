pub const MANTLE_METRICS: &str = "/mantle/metrics";
pub const MANTLE_STATUS: &str = "/mantle/status";
pub const MANTLE_SDP_DECLARATIONS: &str = "/mantle/sdp/declarations";
pub const CRYPTARCHIA_INFO: &str = "/cryptarchia/info";
pub const CRYPTARCHIA_HEADERS: &str = "/cryptarchia/headers";
pub const CRYPTARCHIA_LIB_STREAM: &str = "/cryptarchia/lib-stream";
pub const NETWORK_INFO: &str = "/network/info";
pub const BLEND_NETWORK_INFO: &str = "/blend/info";
pub const MEMPOOL_ADD_TX: &str = "/mempool/add/tx";
pub const CHANNEL: &str = "/channel/:id";
pub const CHANNEL_DEPOSIT: &str = "/channel/deposit";
pub const SDP_POST_DECLARATION: &str = "/sdp/declaration";
pub const SDP_POST_ACTIVITY: &str = "/sdp/activity";
pub const SDP_POST_WITHDRAWAL: &str = "/sdp/withdrawal";
pub const SDP_POST_SET_DECLARATION_ID: &str = "/sdp/set-declaration-id";
pub const LEADER_CLAIM: &str = "/leader/claim";

pub const BLOCKS: &str = "/cryptarchia/blocks";
pub const BLOCKS_RANGE_STREAM: &str = "/cryptarchia/blocks_range";
pub const BLOCKS_DETAIL: &str = "/cryptarchia/blocks/:id";
pub const BLOCKS_STREAM: &str = "/cryptarchia/events/blocks/stream";

pub const TRANSACTION: &str = "/cryptarchia/transaction/:id";

pub mod wallet {
    pub const BALANCE: &str = "/wallet/:public_key/balance";
    pub const TRANSACTIONS_TRANSFER_FUNDS: &str = "/wallet/transactions/transfer-funds";
    pub const SIGN_TX_ED25519: &str = "/wallet/sign/ed25519";
    pub const SIGN_TX_ZK: &str = "/wallet/sign/zk";
}

// testing paths
pub const UPDATE_MEMBERSHIP: &str = "/test/membership/update";
pub const DIAL_PEER: &str = "/test/network/dial_peer";
