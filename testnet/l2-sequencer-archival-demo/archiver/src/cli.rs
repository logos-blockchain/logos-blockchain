use core::convert::Infallible;

use clap::Parser;
use nomos_core::mantle::ops::channel::ChannelId;
use url::Url;

#[derive(Parser, Debug)]
pub struct CliArgs {
    #[clap(short = 'e', env = "TESTNET_ENDPOINT")]
    pub nomos_node_http_endpoint: Url,
    #[clap(short = 'u', env = "TESTNET_USERNAME")]
    pub username: String,
    #[clap(short = 'p', env = "TESTNET_PASSWORD")]
    pub password: String,
    #[clap(short = 'c', env = "CHANNEL_ID", value_parser = parse_channel_id)]
    pub channel_id: ChannelId,
    #[clap(short = 't', env = "TOKEN_NAME")]
    pub token_name: String,
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "Clap requires a Result type for custom parsers"
)]
fn parse_channel_id(encoded_channel_id: &str) -> Result<ChannelId, Infallible> {
    Ok(
        <[u8; 32]>::try_from(hex::decode(encoded_channel_id).unwrap())
            .unwrap()
            .into(),
    )
}
