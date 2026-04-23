use std::net::SocketAddr;

use crate::unique::get_reserved_available_tcp_port;

const LOCAL_API_HOST: &str = "127.0.0.1";

#[derive(Clone)]
pub struct GeneralApiConfig {
    pub address: SocketAddr,
    pub testing_http_address: SocketAddr,
}

#[must_use]
pub fn create_api_configs(ids: &[[u8; 32]]) -> Vec<GeneralApiConfig> {
    ids.iter()
        .map(|_| GeneralApiConfig {
            address: format!(
                "{LOCAL_API_HOST}:{}",
                get_reserved_available_tcp_port().unwrap()
            )
            .parse()
            .unwrap(),
            testing_http_address: format!(
                "{LOCAL_API_HOST}:{}",
                get_reserved_available_tcp_port().unwrap()
            )
            .parse()
            .unwrap(),
        })
        .collect()
}
