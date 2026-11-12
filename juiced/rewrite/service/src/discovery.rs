use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io, net::SocketAddr, time::Duration};
use tokio::{net::UdpSocket, time::Instant};

pub const BEACON_PORT: u16 = 13305;
const MAX_BEACON_BYTES: usize = 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Beacon {
    pub service: String,
    pub hostname: String,
    pub url: String,
}

impl Beacon {
    pub fn loopback(hostname: &str, port: u16) -> io::Result<Self> {
        if port == 0 || !hostname_valid(hostname) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid beacon hostname or HTTP port",
            ));
        }
        Ok(Self {
            service: "lemonade".to_owned(),
            hostname: hostname.to_owned(),
            url: format!("http://127.0.0.1:{port}/api/v1/"),
        })
    }

    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        if bytes.len() > MAX_BEACON_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Beacon exceeds native datagram limit",
            ));
        }
        Ok(bytes)
    }

    pub fn origin(&self) -> Option<Url> {
        let mut url = Url::parse(&self.url).ok()?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "/" | "/api/v1/")
            || url.port_or_known_default()? == 0
        {
            return None;
        }
        url.host_str()?
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .ok()?;
        url.set_path("/");
        Some(url)
    }

    pub fn from_datagram(bytes: &[u8], sender: SocketAddr, local_only: bool) -> Option<Self> {
        if bytes.len() > MAX_BEACON_BYTES || (local_only && !sender.ip().is_loopback()) {
            return None;
        }
        let beacon: Self = serde_json::from_slice(bytes).ok()?;
        if beacon.service != "lemonade" || !hostname_valid(&beacon.hostname) {
            return None;
        }
        let url = beacon.origin()?;
        let address = url
            .host_str()?
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .ok()?;
        if local_only {
            if !address.is_loopback() {
                return None;
            }
        } else if address != sender.ip() {
            return None;
        }
        Some(beacon)
    }
}

fn hostname_valid(value: &str) -> bool {
    !value.is_empty() && value.len() <= 255 && !value.chars().any(char::is_control)
}

fn listener(port: u16) -> io::Result<UdpSocket> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    socket.set_reuse_address(true)?;
    socket.set_broadcast(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, port)).into())?;
    UdpSocket::from_std(socket.into())
}

async fn receive(
    socket: UdpSocket,
    duration: Duration,
    local_only: bool,
) -> io::Result<Vec<Beacon>> {
    let deadline = Instant::now() + duration;
    let mut buffer = [0u8; MAX_BEACON_BYTES + 1];
    let mut found = BTreeMap::new();
    loop {
        let received = tokio::time::timeout_at(deadline, socket.recv_from(&mut buffer)).await;
        match received {
            Ok(Ok((bytes, sender))) => {
                if let Some(beacon) = Beacon::from_datagram(&buffer[..bytes], sender, local_only)
                    && (found.len() < 256 || found.contains_key(&beacon.url))
                {
                    found.insert(beacon.url.clone(), beacon);
                }
            }
            Ok(Err(error)) => return Err(error),
            Err(_) => return Ok(found.into_values().collect()),
        }
    }
}

pub async fn discover_local(duration: Duration) -> io::Result<Vec<Beacon>> {
    let socket = listener(BEACON_PORT)?;
    receive(socket, duration, true).await
}

pub async fn scan(duration: Duration) -> io::Result<Vec<Beacon>> {
    let socket = listener(BEACON_PORT)?;
    receive(socket, duration, false).await
}

pub async fn broadcast_local(port: u16, hostname: String, interval: Duration) -> io::Result<()> {
    if interval.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Beacon interval must be positive",
        ));
    }
    let payload = Beacon::loopback(&hostname, port)?.encode()?;
    let socket = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let mut timer = tokio::time::interval(interval);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        timer.tick().await;
        socket
            .send_to(&payload, (std::net::Ipv4Addr::LOCALHOST, BEACON_PORT))
            .await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_wire_preserves_service_hostname_and_api_path_but_connection_uses_origin() {
        let beacon = Beacon::loopback("test-host", 13306).unwrap();
        let decoded = Beacon::from_datagram(
            &beacon.encode().unwrap(),
            "127.0.0.1:40000".parse().unwrap(),
            true,
        )
        .unwrap();
        assert_eq!(decoded, beacon);
        assert_eq!(
            decoded.origin().unwrap().as_str(),
            "http://127.0.0.1:13306/"
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&beacon.encode().unwrap()).unwrap(),
            serde_json::json!({"service":"lemonade","hostname":"test-host","url":"http://127.0.0.1:13306/api/v1/"})
        );
    }

    #[test]
    fn local_discovery_rejects_remote_sources_credentials_redirect_paths_and_spoofed_service() {
        let source: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        let mut beacon = Beacon::loopback("host", 13306).unwrap();
        assert!(
            Beacon::from_datagram(
                &beacon.encode().unwrap(),
                "192.168.1.2:40000".parse().unwrap(),
                true
            )
            .is_none()
        );
        for url in [
            "http://192.168.1.2:13306/api/v1/",
            "http://user:secret@127.0.0.1:13306/api/v1/",
            "http://127.0.0.1:13306/private",
            "http://127.0.0.1:13306/?token=secret",
            "http://localhost:13306/api/v1/",
        ] {
            beacon.url = url.to_owned();
            assert!(Beacon::from_datagram(&beacon.encode().unwrap(), source, true).is_none());
        }
        beacon.url = "http://127.0.0.1:13306/api/v1/".to_owned();
        beacon.service = "other".to_owned();
        assert!(Beacon::from_datagram(&beacon.encode().unwrap(), source, true).is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn actual_loopback_datagrams_are_bounded_and_deduplicated() {
        let receiver = listener(0).unwrap();
        let address = SocketAddr::from((
            std::net::Ipv4Addr::LOCALHOST,
            receiver.local_addr().unwrap().port(),
        ));
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let beacon = Beacon::loopback("fixture", 13306).unwrap();
        let payload = beacon.encode().unwrap();
        sender.send_to(&payload, address).await.unwrap();
        sender.send_to(&payload, address).await.unwrap();
        sender.send_to(&vec![b'x'; 2048], address).await.unwrap();
        let found = receive(receiver, Duration::from_millis(30), true)
            .await
            .unwrap();
        assert_eq!(found, vec![beacon]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_udp_listeners_allow_address_reuse() {
        let first = listener(0).unwrap();
        let port = first.local_addr().unwrap().port();
        let second = listener(port).unwrap();
        assert_eq!(second.local_addr().unwrap().port(), port);
    }
}
