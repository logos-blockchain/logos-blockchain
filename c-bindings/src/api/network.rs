use lb_api_service::http::libp2p::libp2p_info;
use lb_network_service::backends::libp2p::Libp2pInfo;

use crate::{
    LogosBlockchainNode,
    errors::{OperationStatus, OperationStatusCode},
    result::{FfiStatusResult, StatusResult},
    return_error_if_null_pointer, unwrap_or_return_error,
};

/// Connectivity counters for the node's libp2p swarm.
///
/// All fields are plain counters, so the value is self-contained and needs no
/// accompanying `free` call. The peer and address lists behind these counts are
/// available over HTTP at `/network/info`.
#[repr(C)]
#[derive(Default)]
pub struct NetworkInfo {
    /// Peers the swarm is connected to.
    pub n_peers: usize,
    /// Established connections. A peer can account for more than one.
    pub n_connections: u32,
    /// Connections still being established.
    pub n_pending_connections: u32,
    /// Peers found through Kademlia discovery, connected or not.
    pub n_discovered_peers: usize,
}

impl From<Libp2pInfo> for NetworkInfo {
    fn from(value: Libp2pInfo) -> Self {
        Self {
            n_peers: value.n_peers,
            n_connections: value.n_connections,
            n_pending_connections: value.n_pending_connections,
            n_discovered_peers: value.n_discovered_peers,
        }
    }
}

/// Gets the node's libp2p connectivity information.
///
/// This is a synchronous wrapper around the asynchronous
/// [`libp2p_info`](lb_api_service::http::libp2p::libp2p_info) function.
///
/// # Arguments
///
/// - `node`: A [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A [`Result`] containing the swarm information on success, or an
/// [`OperationStatus`] error on failure.
pub(crate) fn get_network_info_sync(node: &LogosBlockchainNode) -> StatusResult<Libp2pInfo> {
    node.get_runtime_handle()
        .block_on(libp2p_info(node.get_overwatch_handle()))
        .map_err(|error| {
            OperationStatus::error(
                OperationStatusCode::RelayError,
                format!("Failed to get network info: {error}"),
            )
        })
}

pub type FfiNetworkInfoResult = FfiStatusResult<NetworkInfo>;

/// Reports how many peers the node is connected to, along with the related
/// connection counters.
///
/// # Arguments
///
/// - `node`: A non-null pointer to a running [`LogosBlockchainNode`] instance.
///
/// # Returns
///
/// A [`FfiNetworkInfoResult`] containing the [`NetworkInfo`] counters on
/// success, or an [`OperationStatus`] error on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences a raw pointer. The caller
/// must ensure that `node` is non-null and points to a valid
/// [`LogosBlockchainNode`] instance.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_network_info(
    node: *const LogosBlockchainNode,
) -> FfiNetworkInfoResult {
    return_error_if_null_pointer!(node);

    let node = unsafe { &*node };
    let info = unwrap_or_return_error!(get_network_info_sync(node));

    FfiNetworkInfoResult::ok(NetworkInfo::from(info))
}
