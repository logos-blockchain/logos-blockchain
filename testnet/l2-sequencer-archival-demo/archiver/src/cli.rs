use core::convert::Infallible;

use clap::Parser;
use nomos_core::mantle::ops::channel::ChannelId;
use url::Url;

#[derive(Parser, Debug)]
pub struct CliArgs {
    #[clap(short = 'e', env = "TESTNET_ENDPOINT")]
    pub nomos_node_http_endpoint: Url,
    #[clap(short = 'u', env = "TESTNET_USERNAME")]
    pub username: Option<String>,
    #[clap(short = 'p', env = "TESTNET_PASSWORD")]
    pub password: Option<String>,
    #[clap(short = 'c', env = "CHANNEL_ID", value_parser = parse_channel_id)]
    pub channel_id: ChannelId,
    #[clap(short = 't', env = "TOKEN_NAME")]
    pub token_name: String,
    #[clap(short = 'b', env = "INITIAL_BALANCE", default_value = "1000")]
    pub initial_balance: u64,
    #[clap(short = 'n', env = "PORT_NUMBER", default_value = "8090")]
    pub port_number: u16,
    #[clap(
        long,
        env = "ARCHIVER_BLOCKS_DB_PATH",
        default_value = "blocks.database"
    )]
    pub blocks_db_path: String,
    #[clap(
        long,
        env = "ARCHIVER_ACCOUNTS_DB_PATH",
        default_value = "accounts.database"
    )]
    pub accounts_db_path: String,
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
