use std::{
    net::SocketAddr,
    sync::LazyLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use log::info;
use sntpc::NtpResult;
use time::{Date, Month, OffsetDateTime, Time, error::ComponentRange};
use tokio::net::UdpSocket;

// NTP epoch starts on Jan 1, 1900; Unix starts on Jan 1, 1970.
static NTP_EPOCH_OFFSET: LazyLock<Duration> = LazyLock::new(|| {
    let ntp_date =
        Date::from_calendar_date(1900, Month::January, 1).expect("This date should be valid.");
    let ntp_system_time = SystemTime::from(OffsetDateTime::new_utc(ntp_date, Time::MIDNIGHT));
    UNIX_EPOCH
        .duration_since(ntp_system_time)
        .expect("This duration should be valid.")
});
pub const SNTP_PACKET_SIZE: usize = 48;

pub struct NtpTimestamp {
    pub seconds: u32,
    pub fraction: u32,
}

impl NtpTimestamp {
    #[must_use]
    pub fn from_unix(unix_nanos: u128) -> Self {
        let ntp_seconds = (unix_nanos / 1_000_000_000) as u32 + NTP_EPOCH_OFFSET.as_secs() as u32;
        let ntp_nanos = (unix_nanos % 1_000_000_000) as u32;
        Self {
            seconds: ntp_seconds,
            fraction: ntp_nanos,
        }
    }
}

impl From<NtpResult> for NtpTimestamp {
    fn from(value: NtpResult) -> Self {
        Self {
            seconds: value.sec(),
            fraction: value.sec_fraction(),
        }
    }
}

impl TryFrom<NtpTimestamp> for OffsetDateTime {
    type Error = ComponentRange;

    fn try_from(value: NtpTimestamp) -> Result<Self, Self::Error> {
        Self::from_unix_timestamp(i64::from(value.seconds))?.replace_nanosecond(value.fraction)
    }
}

pub trait TimestampGeneratorTrait {
    fn generate_timestamp(&self) -> NtpTimestamp;
}

#[derive(Default)]
pub struct NowTimestampGenerator;

impl TimestampGeneratorTrait for NowTimestampGenerator {
    fn generate_timestamp(&self) -> NtpTimestamp {
        let unix_now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();
        NtpTimestamp::from_unix(unix_now.as_nanos())
    }
}

pub struct FakeNTPServer<TimestampGenerator> {
    socket_address: SocketAddr,
    leap_indicator: u8, // 2 bits
    version_number: u8, // 3 bits
    mode: u8,           // 3 bits
    stratum: u8,
    poll_interval: u8,
    precision: u8,
    root_delay: [u8; 4],
    root_dispersion: [u8; 4],
    reference_identifier: [u8; 4],
    timestamp_generator: TimestampGenerator,
}

impl<TimestampGenerator: TimestampGeneratorTrait + Send + Sync> FakeNTPServer<TimestampGenerator> {
    pub const fn from_address(
        socket_address: SocketAddr,
        timestamp_generator: TimestampGenerator,
    ) -> Self {
        Self {
            socket_address,
            leap_indicator: 0, // no warning
            version_number: 4, // NTPv4
            mode: 4,           // server
            stratum: 1,        // primary server
            poll_interval: 0,  // 1 second
            precision: 0xFA,
            root_delay: [0u8; 4],
            root_dispersion: [0u8; 4],
            reference_identifier: [127u8, 0u8, 0u8, 1u8],
            timestamp_generator,
        }
    }

    pub async fn run(&self) {
        let socket = UdpSocket::bind(self.socket_address)
            .await
            .expect("Failed to bind socket");
        info!("NTP server listening at: {:?}", self.socket_address);

        loop {
            let mut buffer = [0u8; 48];
            let response = socket.recv_from(&mut buffer).await;

            if let Ok((size, client_socket)) = response {
                assert_eq!(size, 48, "Package size different than expected (48 bytes).");
                let response = self
                    .create_response(&buffer)
                    .expect("Failed to create response");
                let _response_size = socket
                    .send_to(&response, client_socket)
                    .await
                    .expect("Failed to send data");
            } else {
                info!("Failed to receive data");
            }
        }
    }

    fn create_response(&self, request: &[u8]) -> Result<[u8; 48], String> {
        if request.len() < SNTP_PACKET_SIZE {
            let actual_size = request.len();
            return Err(format!(
                "Invalid request size. Expected {SNTP_PACKET_SIZE} bytes but got {actual_size}"
            ));
        }

        let origin_timestamp = &request[40..48];
        let ntp_timestamp = self.timestamp_generator.generate_timestamp();

        Ok(self.build_response_packet(&ntp_timestamp, origin_timestamp))
    }

    fn build_response_packet(
        &self,
        ntp_timestamp: &NtpTimestamp,
        origin_timestamp: &[u8],
    ) -> [u8; 48] {
        let mut packet = [0u8; 48];

        let ntp_seconds = ntp_timestamp.seconds.to_be_bytes();
        let ntp_fraction = ntp_timestamp.fraction.to_be_bytes();

        let li_vn_mode = (self.leap_indicator << 6) | (self.version_number << 3) | self.mode;
        packet[0] = li_vn_mode;
        packet[1] = self.stratum;
        packet[2] = self.poll_interval;
        packet[3] = self.precision;
        packet[4..8].copy_from_slice(&self.root_delay);
        packet[8..12].copy_from_slice(&self.root_dispersion);
        packet[12..16].copy_from_slice(&self.reference_identifier);

        // Reference Timestamp (same as transmit timestamp)
        packet[16..20].copy_from_slice(&ntp_seconds);
        packet[20..24].copy_from_slice(&ntp_fraction);

        // Origin Timestamp (copied from client)
        packet[24..32].copy_from_slice(origin_timestamp);

        // Receive Timestamp (same as transmit timestamp)
        packet[32..36].copy_from_slice(&ntp_seconds);
        packet[36..40].copy_from_slice(&ntp_fraction);

        // Transmit Timestamp (current time)
        packet[40..44].copy_from_slice(&ntp_seconds);
        packet[44..48].copy_from_slice(&ntp_fraction);

        packet
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use lb_utils::net::get_available_tcp_port;
    use log::trace;

    use super::*;
    use crate::backends::ntp::async_client::{AsyncNTPClient, NTPClientSettings};

    #[tokio::test]
    async fn test_fake_ntp_server() {
        let server_ip_address = "127.0.0.1";
        let server_port = get_available_tcp_port().unwrap();
        let parsed_server_ip_address: Ipv4Addr = server_ip_address.parse().unwrap();
        let server_socket_address =
            SocketAddr::new(IpAddr::from(parsed_server_ip_address), server_port);

        let server = FakeNTPServer::from_address(server_socket_address, NowTimestampGenerator);
        let server_handle = tokio::spawn(async move { server.run().await });

        let client = AsyncNTPClient::new(NTPClientSettings {
            timeout: Duration::from_secs(1),
            listening_interface: IpAddr::from([127, 0, 0, 1]),
        });

        let formatted_server_socket_address = format!("{server_ip_address}:{server_port}");
        let ntp_result = client
            .request_timestamp(formatted_server_socket_address)
            .await
            .expect("Failed to request timestamp");

        let response_datetime: OffsetDateTime = NtpTimestamp::from(ntp_result)
            .try_into()
            .expect("Failed to convert response to OffsetDateTime");

        trace!("Received timestamp from NTP server: {response_datetime}");

        server_handle.abort();
    }
}
