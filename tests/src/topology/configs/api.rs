use std::net::SocketAddr;

use lb_utils::net::get_available_tcp_port;

#[derive(Clone)]
pub struct GeneralApiConfig {
    pub address: SocketAddr,
}

#[must_use]
pub fn create_api_configs(ids: &[[u8; 32]]) -> Vec<GeneralApiConfig> {
    ids.iter()
        .map(|_| GeneralApiConfig {
            address: format!("127.0.0.1:{}", get_available_tcp_port().unwrap())
                .parse()
                .unwrap(),
        })
        .collect()
}
