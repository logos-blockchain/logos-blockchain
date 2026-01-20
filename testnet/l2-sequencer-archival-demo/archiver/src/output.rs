use core::net::SocketAddr;

use lb_core::mantle::ops::channel::ChannelId;
use owo_colors::OwoColorize as _;
use url::Url;

const BANNER: &str = r"
    _             _     _                 ____                       
   / \   _ __ ___| |__ (_)_   _____ _ __ |  _ \  ___ _ __ ___   ___  
  / _ \ | '__/ __| '_ \| \ \ / / _ \ '__|| | | |/ _ \ '_ ` _ \ / _ \ 
 / ___ \| | | (__| | | | |\ V /  __/ |   | |_| |  __/ | | | | | (_) |
/_/   \_\_|  \___|_| |_|_| \_/ \___|_|   |____/ \___|_| |_| |_|\___/ 
";

pub fn print_startup_banner(endpoint: &Url, channel_id: &ChannelId, listen_addr: &SocketAddr) {
    println!("{}", BANNER.cyan().bold());
    println!("{}", "═".repeat(70).dimmed());
    println!(
        "  {} {}",
        "📡 Logos Blockchain Node:".bright_blue().bold(),
        endpoint.white()
    );
    println!(
        "  {} {}",
        "📺 Channel ID:".bright_blue().bold(),
        hex::encode(channel_id.as_ref()).white()
    );
    println!(
        "  {} {}",
        "🌐 HTTP Server:".bright_blue().bold(),
        format!("http://{listen_addr}/blocks").green()
    );
    println!("{}", "═".repeat(70).dimmed());
    println!("  {} Waiting for blocks...\n", "⏳".yellow());
}
