use std::{
    net::{IpAddr, Ipv4Addr},
    time::Duration,
};

use lb_node::config::time::serde as time;

pub const DEFAULT_SLOT_TIME_IN_SECS: u64 = 1;
pub const CONSENSUS_SLOT_TIME_VAR: &str = "CONSENSUS_SLOT_TIME";
pub const USE_LOCAL_HOST_NTP_TIME_CONFIG: &str = "USE_LOCAL_HOST_NTP_TIME_CONFIG";

const PUBLIC_NTP_SERVER: &str = "pool.ntp.org:123";
const LOCAL_NTP_SERVER: &str = "127.0.0.1:123";

const NTP_SERVER_HOST: &str = "127.0.0.1";
const NTP_SERVER_PORT: u16 = 123;

const PUBLIC_NTP_TIMEOUT: Duration = Duration::from_secs(5);
const PUBLIC_NTP_UPDATE_INTERVAL: Duration = Duration::from_secs(16);
const LOCAL_NTP_TIMEOUT: Duration = Duration::from_secs(1);
const LOCAL_NTP_UPDATE_INTERVAL: Duration = Duration::from_secs(1);
const LOCAL_NTP_HEALTHCHECK_TIMEOUT: Duration = Duration::from_millis(500);

pub type GeneralTimeConfig = time::Config;

#[must_use]
pub fn set_time_config() -> GeneralTimeConfig {
    if is_truthy_env(USE_LOCAL_HOST_NTP_TIME_CONFIG) {
        local_host_ntp_time_config()
    } else {
        default_public_time_config()
    }
}

fn is_truthy_env(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

#[must_use]
fn default_public_time_config() -> GeneralTimeConfig {
    GeneralTimeConfig {
        backend: time::NtpSettings {
            client: time::NtpClientSettings {
                timeout: PUBLIC_NTP_TIMEOUT,
                listening_interface: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            },
            server: PUBLIC_NTP_SERVER.to_owned(),
            update_interval: PUBLIC_NTP_UPDATE_INTERVAL,
        },
    }
}

#[must_use]
fn local_host_ntp_time_config() -> GeneralTimeConfig {
    assert!(
        is_local_ntp_server_running(),
        "Ensure a local NTP server is properly installed and configured\n\
        \n\
        Linux (Ubuntu/WSL Ubuntu):\n\
          - Install a simple NTP service like `chrony`:\n  \
            `sudo apt-get update && sudo apt-get install -y chrony`\n\
          - With `/etc/chrony/chrony.conf`:\n  \
            - Add a real public NTP server, e.g. `server time.google.com iburst`\n  \
            - Ensure only one 'pool' or 'server' line exit to avoid duplicates\n  \
            - Add 'bindaddress 127.0.0.1'\n  \
            - Add 'allow 127.0.0.1'\n\
          - Start/restart the NTP service\n  \
            - `sudo systemctl restart chrony`\n\
          - Check the NTP service status and sources:\n  \
            - `sudo systemctl status chrony`\n  \
            - `chronyc sources`\n\
        "
    );
    GeneralTimeConfig {
        backend: time::NtpSettings {
            client: time::NtpClientSettings {
                timeout: LOCAL_NTP_TIMEOUT,
                listening_interface: IpAddr::V4(Ipv4Addr::LOCALHOST),
            },
            server: LOCAL_NTP_SERVER.to_owned(),
            update_interval: LOCAL_NTP_UPDATE_INTERVAL,
        },
    }
}

fn is_local_ntp_server_running() -> bool {
    use std::net::UdpSocket;

    let addr = (NTP_SERVER_HOST, NTP_SERVER_PORT);
    let socket = UdpSocket::bind((NTP_SERVER_HOST, 0)).expect("Failed to bind UDP socket");
    drop(socket.set_read_timeout(Some(LOCAL_NTP_HEALTHCHECK_TIMEOUT)));

    let mut req = [0u8; 48];
    req[0] = 0x1B;
    drop(socket.send_to(&req, addr));

    let mut buf = [0u8; 48];
    match socket.recv_from(&mut buf) {
        Ok((len, _)) if len >= 48 => {
            let stratum = buf[1];
            if (1..=15).contains(&stratum) {
                true
            } else {
                eprintln!(
                    "NTP server on 127.0.0.1:123 responded with invalid stratum: {stratum} \
                     (expected 1-15).\n\
                     This usually means the NTP service is not synchronized to a real or manual \
                     time source.",
                );
                false
            }
        }
        _ => {
            eprintln!("No NTP server found on 127.0.0.1:123");
            false
        }
    }
}
