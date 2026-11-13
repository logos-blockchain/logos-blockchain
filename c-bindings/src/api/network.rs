use std::ffi::{CString, c_char};

use lb_c_macros::panic_to_error;
use lb_network_service::backends::libp2p::Libp2pInfo;

use crate::{
    LogosBlockchainNode,
    api::config::cstr_to_path,
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
/// [`info`](lb_node::api::calls::network::info) function.
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
    node.call(
        "Failed to get network info",
        lb_node::api::calls::network::info(node.get_overwatch_handle()),
    )
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
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_network_info(
    node: *const LogosBlockchainNode,
) -> FfiNetworkInfoResult {
    return_error_if_null_pointer!(node);

    let node = unsafe { &*node };
    let info = unwrap_or_return_error!(get_network_info_sync(node));

    FfiNetworkInfoResult::ok(NetworkInfo::from(info))
}

/// Result type for [`get_peer_id`]. On success, `value` is a pointer to a
/// NUL-terminated C string containing the base58-encoded libp2p `PeerId`.
pub type FfiGetPeerIdResult = FfiStatusResult<*mut c_char>;

/// Derives the libp2p `PeerId` from the node key in a user config file,
/// equivalent to the `get-peer-id` CLI command.
///
/// # Arguments
///
/// - `config_path`: Path to the user config YAML file.
///
/// # Returns
///
/// A [`FfiGetPeerIdResult`] containing a pointer to an allocated C string
/// (the base58-encoded `PeerId`) on success, or an [`OperationStatus`] error
/// on failure.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers. The caller
/// must ensure that `config_path` is a valid NUL-terminated C string.
///
/// # Memory Management
///
/// This function allocates memory for the output C string. The caller must
/// free this memory using the [`free_cstring`](super::free_cstring) function.
#[must_use]
#[panic_to_error]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_peer_id(config_path: *const c_char) -> FfiGetPeerIdResult {
    return_error_if_null_pointer!(config_path);

    let config_path = unsafe { cstr_to_path(config_path) };

    let peer_id = match lb_node::cli::get_peer_id::peer_id_from_config(&config_path) {
        Ok(peer_id) => peer_id,
        Err(error) => {
            return FfiGetPeerIdResult::err(OperationStatus::error(
                OperationStatusCode::ConfigurationError,
                format!("Error deriving peer id: {error:?}"),
            ));
        }
    };

    match CString::new(peer_id.to_string()) {
        Ok(peer_id) => FfiGetPeerIdResult::ok(peer_id.into_raw()),
        Err(error) => FfiGetPeerIdResult::err(OperationStatus::error(
            OperationStatusCode::RuntimeError,
            format!("Failed to create CString: {error}"),
        )),
    }
}
