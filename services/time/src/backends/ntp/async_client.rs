use std::{
    fmt::{Debug, Formatter},
    future::ready,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use futures::{StreamExt as _, stream::FuturesUnordered};
use lb_utils::bounded_duration::{MinimalBoundedDuration, NANO};
use sntpc::{NtpContext, NtpResult, StdTimestampGen, get_time};
use tokio::{
    net::{ToSocketAddrs, UdpSocket, lookup_host},
    time::sleep,
};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Io(std::io::Error),
    #[error(
        "{replies} of the {addresses} NTP server addresses of the listening interface's IP \
         version replied, not a majority"
    )]
    TooFewReplies { replies: usize, addresses: usize },
}

#[serde_with::serde_as]
#[derive(Debug, Copy, Clone, serde::Serialize, serde::Deserialize)]
pub struct NTPClientSettings {
    /// How long a round of requests waits for the servers' replies
    #[serde_as(as = "MinimalBoundedDuration<1, NANO>")]
    pub timeout: Duration,
    pub listening_interface: IpAddr,
}

#[derive(Clone)]
pub struct AsyncNTPClient {
    settings: NTPClientSettings,
    ntp_context: NtpContext<StdTimestampGen>,
}

impl Debug for AsyncNTPClient {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncNTPClient")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl AsyncNTPClient {
    #[must_use]
    pub fn new(settings: NTPClientSettings) -> Self {
        let ntp_context = NtpContext::new(StdTimestampGen::default());
        Self {
            settings,
            ntp_context,
        }
    }

    async fn get_time(&self, host: SocketAddr) -> sntpc::Result<NtpResult> {
        let socket = UdpSocket::bind(SocketAddr::new(self.settings.listening_interface, 0))
            .await
            .map_err(|_| sntpc::Error::Network)?; // same error that get_time returns for io
        get_time(host, &socket, self.ntp_context).await
    }

    /// How far ahead of the system clock the time of the NTP servers at `pool`
    /// is: the median of the offsets they reply within the timeout, if most of
    /// them reply.
    ///
    /// Only the addresses of the listening interface's IP version are asked,
    /// as the others are out of its reach.
    pub async fn request_offset<Addresses>(&self, pool: Addresses) -> Result<time::Duration, Error>
    where
        Addresses: ToSocketAddrs + Sync,
    {
        let is_ipv4 = self.settings.listening_interface.is_ipv4();
        let hosts: Vec<_> = lookup_host(&pool)
            .await
            .map_err(Error::Io)?
            .filter(|host| host.is_ipv4() == is_ipv4)
            .collect();
        let offsets: Vec<_> = hosts
            .iter()
            .map(|host| self.get_time(*host))
            .collect::<FuturesUnordered<_>>()
            .filter_map(|reply| ready(reply.ok().map(|reply| reply.offset())))
            .take_until(sleep(self.settings.timeout))
            .collect()
            .await;
        let replies = offsets.len();
        median_of_majority(offsets, hosts.len())
            .map(time::Duration::microseconds)
            .ok_or(Error::TooFewReplies {
                replies,
                addresses: hosts.len(),
            })
    }
}

/// The median of `offsets`, if more than half of the `addresses` asked replied
/// with them.
fn median_of_majority(mut offsets: Vec<i64>, addresses: usize) -> Option<i64> {
    if offsets.len() <= addresses / 2 {
        return None;
    }
    offsets.sort_unstable();
    let middle = offsets.len() / 2;
    Some(if offsets.len().is_multiple_of(2) {
        offsets[middle - 1].midpoint(offsets[middle])
    } else {
        offsets[middle]
    })
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    #[tokio::test]
    async fn real_ntp_request() -> Result<(), Error> {
        let ntp_server_ip = "pool.ntp.org";
        let ntp_server_address = format!("{ntp_server_ip}:123");

        let settings = NTPClientSettings {
            timeout: Duration::from_secs(3),
            listening_interface: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        };
        let client = AsyncNTPClient::new(settings);

        let _offset = client.request_offset(ntp_server_address).await?;

        Ok(())
    }

    #[test]
    fn a_minority_of_replies_does_not_move_the_median() {
        // 50 years, in microseconds.
        let far_ahead = 50 * 365 * 24 * 3_600 * 1_000_000;
        assert_eq!(median_of_majority(vec![far_ahead, 2, 1], 4), Some(2));
        assert_eq!(median_of_majority(vec![1, far_ahead, 4, 2], 4), Some(3));
    }

    #[test]
    fn fewer_replies_than_a_majority_give_no_median() {
        assert_eq!(median_of_majority(vec![1, 2], 4), None);
        assert_eq!(median_of_majority(vec![], 0), None);
    }
}
