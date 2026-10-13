//! Finding the serial device server the scale is wired to.
//!
//! Two ways, both used: the Moxa NPort answers a UDP broadcast on port 4800
//! (the protocol its own "NPort Search" utility speaks), and failing that the
//! LAN is scanned for the TCP ports an NPort serves serial data on - 950 in
//! Real COM mode, 4001 in TCP Server mode - with the device's web console
//! checked for the "NPort" name. Anything that answers is listed with the port
//! that carried data, so the Scale tab can offer it with one click.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::task::JoinSet;
use tokio::time::{timeout, Instant};

pub const DATA_PORTS: [u16; 2] = [950, 4001];
const MOXA_SEARCH_PORT: u16 = 4800;
const PORT_TIMEOUT: Duration = Duration::from_millis(700);
const WEB_TIMEOUT: Duration = Duration::from_millis(1500);
const BROADCAST_WAIT: Duration = Duration::from_millis(1500);
// Sixty at a time keeps the scan to a few seconds without flooding a small
// switch with hundreds of SYNs at once.
const SCAN_BATCH: usize = 60;

/// A /24 this machine sits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subnet {
    /// The first three octets.
    pub base: [u8; 3],
    /// This machine's own address on it, skipped by the scan.
    pub own: Ipv4Addr,
}

impl Subnet {
    pub fn host(&self, last: u8) -> Ipv4Addr {
        let [a, b, c] = self.base;
        Ipv4Addr::new(a, b, c, last)
    }

    pub fn broadcast(&self) -> Ipv4Addr {
        self.host(255)
    }
}

/// The /24s for a list of addresses, one each, loopback dropped. Larger
/// subnets are cut to the /24 the machine's own address is in: a yard LAN is
/// one of those.
pub fn subnets_of(addresses: impl IntoIterator<Item = Ipv4Addr>) -> Vec<Subnet> {
    let mut out: Vec<Subnet> = Vec::new();
    for own in addresses {
        if own.is_loopback() || own.is_unspecified() {
            continue;
        }
        let [a, b, c, _] = own.octets();
        if !out.iter().any(|s| s.base == [a, b, c]) {
            out.push(Subnet { base: [a, b, c], own });
        }
    }
    out
}

/// Every IPv4 /24 this machine sits on.
pub fn local_subnets() -> Vec<Subnet> {
    let addresses = if_addrs::get_if_addrs().unwrap_or_default();
    subnets_of(addresses.into_iter().filter_map(|iface| match iface.ip() {
        std::net::IpAddr::V4(ip) => Some(ip),
        std::net::IpAddr::V6(_) => None,
    }))
}

/// Whether something accepts a TCP connection there, within `wait`.
pub async fn tcp_open(addr: SocketAddr, wait: Duration) -> bool {
    matches!(timeout(wait, TcpStream::connect(addr)).await, Ok(Ok(_)))
}

/// The first of `ports` open on `host`, tried in order.
pub async fn first_open_port(host: Ipv4Addr, ports: &[u16], wait: Duration) -> Option<u16> {
    for &port in ports {
        if tcp_open(SocketAddr::from((host, port)), wait).await {
            return Some(port);
        }
    }
    None
}

static TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<title>([^<]*)</title>").unwrap());
static MODEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"NPort[\s-]?[0-9A-Za-z-]+").unwrap());

pub fn title_in(html: &str) -> String {
    TITLE.captures(html).map(|c| c[1].trim().to_string()).unwrap_or_default()
}

/// The model in a Moxa search reply, as far as it says.
pub fn model_in(reply: &[u8]) -> String {
    let text = super::frames::latin1(reply);
    MODEL.find(&text).map_or_else(|| "NPort".to_string(), |m| m.as_str().to_string())
}

/// The NPort's web console title, when it has one, says what the box is.
/// A bare HTTP/1.0 GET is all that takes; anything wrong is just no title.
pub async fn web_title(addr: SocketAddr) -> String {
    let fetch = async {
        let mut stream = TcpStream::connect(addr).await.ok()?;
        let request = format!("GET / HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n", addr.ip());
        stream.write_all(request.as_bytes()).await.ok()?;
        let mut page = Vec::new();
        // Headers and the start of the page; the title is near the top.
        stream.take(8192).read_to_end(&mut page).await.ok()?;
        Some(title_in(&String::from_utf8_lossy(&page)))
    };
    timeout(WEB_TIMEOUT, fetch).await.ok().flatten().unwrap_or_default()
}

/// Moxa's search protocol: a 24-byte broadcast, replies carry the model name.
/// The format is not published; what is parsed here is what an NPort 5110
/// answers with (an ASCII model string in the reply).
async fn moxa_broadcast(subnets: &[Subnet], wait: Duration) -> BTreeMap<Ipv4Addr, String> {
    let mut found = BTreeMap::new();
    let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await else { return found };
    if socket.set_broadcast(true).is_err() {
        return found;
    }
    let mut probe = [0u8; 24];
    probe[..4].copy_from_slice(&0x0100_0008u32.to_be_bytes());
    let targets = subnets.iter().map(Subnet::broadcast).chain([Ipv4Addr::BROADCAST]);
    for target in targets {
        let _ = socket.send_to(&probe, (target, MOXA_SEARCH_PORT)).await;
    }
    let deadline = Instant::now() + wait;
    let mut buf = [0u8; 1500];
    while let Ok(Ok((n, from))) = tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await {
        if let SocketAddr::V4(from) = from {
            found.insert(*from.ip(), model_in(&buf[..n]));
        }
    }
    found
}

/// Every host on the /24 with one of `ports` open, and which.
async fn scan_subnet(subnet: &Subnet, ports: &'static [u16], on_progress: &(dyn Fn(usize, usize) + Sync)) -> Vec<(Ipv4Addr, u16)> {
    let hosts: Vec<Ipv4Addr> = (1..=254).map(|last| subnet.host(last)).filter(|&h| h != subnet.own).collect();
    let mut hits = Vec::new();
    for (i, batch) in hosts.chunks(SCAN_BATCH).enumerate() {
        let mut probes = JoinSet::new();
        for &host in batch {
            probes.spawn(async move { first_open_port(host, ports, PORT_TIMEOUT).await.map(|port| (host, port)) });
        }
        while let Some(result) = probes.join_next().await {
            if let Ok(Some(hit)) = result {
                hits.push(hit);
            }
        }
        on_progress(((i + 1) * SCAN_BATCH).min(254), 254);
    }
    hits
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Found {
    pub host: String,
    pub port: u16,
    pub model: String,
    pub title: String,
    /// A Moxa search reply or an "NPort" web title: very probably the one.
    pub likely: bool,
    pub mode: &'static str,
}

/// What is known about one host before it is written up.
#[derive(Debug, Default, Clone)]
struct Candidate {
    model: Option<String>,
    port: Option<u16>,
    title: String,
}

fn write_up(host: Ipv4Addr, c: Candidate) -> Found {
    let from_search = c.model.is_some();
    let likely = from_search || c.title.to_lowercase().contains("nport");
    Found {
        host: host.to_string(),
        port: c.port.unwrap_or(DATA_PORTS[0]),
        model: c.model.unwrap_or_else(|| if likely { "NPort".into() } else { String::new() }),
        title: c.title,
        likely,
        // In Real COM mode the data port is 950; 4001 means TCP Server mode.
        mode: match c.port {
            Some(950) => "Real COM",
            Some(4001) => "TCP Server",
            _ => "",
        },
    }
}

/// Best guess first, then by address (numerically: .9 before .10).
fn rank(mut found: Vec<Found>) -> Vec<Found> {
    let key = |f: &Found| f.host.parse::<Ipv4Addr>().unwrap_or(Ipv4Addr::UNSPECIFIED);
    found.sort_by(|a, b| b.likely.cmp(&a.likely).then_with(|| key(a).cmp(&key(b))));
    found
}

/// Everything on the LAN that looks like a serial device server, best guess
/// first: a Moxa search reply or an "NPort" web title beats a bare open port.
pub async fn discover(on_progress: impl Fn(usize, usize) + Send + Sync) -> Vec<Found> {
    let subnets = local_subnets();
    // BTreeMap rather than HashMap: hosts come out in address order.
    let mut candidates: BTreeMap<Ipv4Addr, Candidate> = BTreeMap::new();
    for (host, model) in moxa_broadcast(&subnets, BROADCAST_WAIT).await {
        candidates.entry(host).or_default().model = Some(model);
    }
    for subnet in &subnets {
        for (host, port) in scan_subnet(subnet, &DATA_PORTS, &on_progress).await {
            candidates.entry(host).or_default().port = Some(port);
        }
    }

    // The web titles, and the data port of anything that answered the search
    // but not the scan, all at once: each is a second or so of waiting.
    let mut lookups = JoinSet::new();
    for (host, mut c) in candidates {
        lookups.spawn(async move {
            c.title = web_title(SocketAddr::from((host, 80))).await;
            if c.port.is_none() {
                // Found by broadcast but no data port answered: still worth listing.
                c.port = first_open_port(host, &DATA_PORTS, PORT_TIMEOUT).await;
            }
            write_up(host, c)
        });
    }
    let mut found = Vec::new();
    while let Some(result) = lookups.join_next().await {
        if let Ok(f) = result {
            found.push(f);
        }
    }
    rank(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn data_ports_are_real_com_and_tcp_server() {
        assert_eq!(DATA_PORTS, [950, 4001]);
    }

    #[test]
    fn subnets_are_one_per_slash_24_without_loopback() {
        let ips = ["127.0.0.1", "192.168.1.20", "192.168.1.21", "10.0.5.7"].map(|s| s.parse().unwrap());
        let subnets = subnets_of(ips);
        assert_eq!(subnets.len(), 2);
        assert_eq!(subnets[0].own, Ipv4Addr::new(192, 168, 1, 20));
        assert_eq!(subnets[0].broadcast(), Ipv4Addr::new(192, 168, 1, 255));
        assert_eq!(subnets[1].host(9), Ipv4Addr::new(10, 0, 5, 9));
    }

    #[test]
    fn this_machines_subnets_are_well_formed() {
        for s in local_subnets() {
            assert_eq!(s.own.octets()[..3], s.base);
        }
    }

    #[test]
    fn reads_the_model_from_a_search_reply() {
        let mut reply = vec![0x81, 0, 0, 0x18];
        reply.extend_from_slice(b"\0\0NPort 5110\0\0\xc0\xa8\x01\x09");
        assert_eq!(model_in(&reply), "NPort 5110");
        assert_eq!(model_in(b"\x81\0\0\x18 nothing readable"), "NPort");
    }

    #[test]
    fn reads_a_page_title() {
        assert_eq!(title_in("<html><HEAD><Title>\n NPort Web Console </Title>"), "NPort Web Console");
        assert_eq!(title_in("<html>no title</html>"), "");
    }

    #[test]
    fn the_likely_ones_come_first_then_by_address() {
        let found = |host: &str, likely| Found { host: host.into(), port: 950, model: String::new(), title: String::new(), likely, mode: "" };
        let ranked = rank(vec![found("192.168.1.10", false), found("192.168.1.9", false), found("192.168.1.200", true)]);
        assert_eq!(ranked.iter().map(|f| f.host.as_str()).collect::<Vec<_>>(), ["192.168.1.200", "192.168.1.9", "192.168.1.10"]);
    }

    #[test]
    fn a_device_is_written_up_by_how_it_was_found() {
        let host = Ipv4Addr::new(192, 168, 1, 9);
        let searched = write_up(host, Candidate { model: Some("NPort 5110".into()), port: Some(950), title: String::new() });
        assert_eq!((searched.likely, searched.mode, searched.model.as_str()), (true, "Real COM", "NPort 5110"));
        let titled = write_up(host, Candidate { model: None, port: Some(4001), title: "NPort Web Console".into() });
        assert_eq!((titled.likely, titled.mode, titled.model.as_str()), (true, "TCP Server", "NPort"));
        let bare = write_up(host, Candidate { model: None, port: Some(4001), title: "Printer".into() });
        assert_eq!((bare.likely, bare.model.as_str()), (false, ""));
        let silent = write_up(host, Candidate { model: Some("NPort".into()), port: None, title: String::new() });
        assert_eq!((silent.port, silent.mode), (950, ""));
    }

    // Against listeners on this machine: the real connects and the real GET.
    #[tokio::test]
    async fn finds_the_first_open_port_in_order() {
        let open = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = open.local_addr().unwrap().port();
        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        }; // dropped, so nothing listens there now
        let found = first_open_port(Ipv4Addr::LOCALHOST, &[closed, port], PORT_TIMEOUT).await;
        assert_eq!(found, Some(port));
        assert_eq!(first_open_port(Ipv4Addr::LOCALHOST, &[closed], PORT_TIMEOUT).await, None);
    }

    #[tokio::test]
    async fn fetches_a_web_console_title() {
        let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = server.accept().await.unwrap();
            let mut request = [0u8; 512];
            let _ = socket.read(&mut request).await;
            socket.write_all(b"HTTP/1.0 200 OK\r\nContent-Type: text/html\r\n\r\n<html><head><title>NPort 5110 Web Console</title>").await.unwrap();
        });
        assert_eq!(web_title(addr).await, "NPort 5110 Web Console");
    }

    #[tokio::test]
    async fn a_host_with_no_web_console_has_no_title() {
        let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr().unwrap();
        // Accepts and then says nothing: the fetch gives up at its timeout.
        tokio::spawn(async move {
            let (_socket, _) = server.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let started = Instant::now();
        assert_eq!(web_title(addr).await, "");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    // Scans the real LAN, so only on request:
    //   cargo test discovers_on_this_lan -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn discovers_on_this_lan() {
        let started = Instant::now();
        let found = discover(|done, total| println!("scanned {done}/{total}")).await;
        println!("{found:#?}\nin {:?}", started.elapsed());
    }
}
