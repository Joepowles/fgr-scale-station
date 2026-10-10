//! Finding IP cameras on the LAN and asking each for its RTSP streams.
//!
//! Two ways, both used on every network this PC sits on:
//!
//! WS-Discovery: a SOAP Probe multicast to 239.255.255.250:3702, which every
//! ONVIF camera answers with its device service address. Sent from each
//! interface in turn, since multicast leaves by one at a time. Quick, but it
//! needs the camera to have discovery turned on and Windows to let the
//! camera's reply back in - the reply comes from the camera's own address,
//! not the multicast one, and the firewall drops it on a "public" network.
//!
//! Sweep: the same Probe sent straight to every address in each /24, and a
//! knock on the ports ONVIF is usually served on, with an unauthenticated
//! GetSystemDateAndTime to whatever answers. Slower (a few seconds a
//! network) but it finds a camera whether or not multicast works.
//!
//! Each camera found is then asked, with the login the user gave, for its
//! make and model, and on request for its media profiles and the RTSP URI of
//! each.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::onvif::{self, Client, Error, Uri, NS_TDS};
use crate::scale::discovery::{local_subnets, tcp_open, Subnet};

const MULTICAST: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const WS_DISCOVERY_PORT: u16 = 3702;
const PROBE_WAIT: Duration = Duration::from_millis(3500);
const KNOCK_TIMEOUT: Duration = Duration::from_millis(700);
const SWEEP_BATCH: usize = 40;

/// Where cameras serve ONVIF: 80 by the spec, 8899 on Xiongmai boards, 8000
/// on Hikvision and Reolink, 8080 on a few others, 2020 on TP-Link.
pub const ONVIF_PORTS: [u16; 5] = [80, 8899, 8000, 8080, 2020];

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Camera {
    pub host: String,
    pub device_url: String,
    pub name: String,
    pub hardware: String,
    pub location: String,
    pub via: &'static str,
    /// From the sweep: false when it answered with a fault or a 401, which
    /// says ONVIF is there but wants a login.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmed: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct Found {
    pub cameras: Vec<Camera>,
    /// The networks searched, so the user can see the right one was.
    pub networks: Vec<String>,
}

fn probe_xml() -> String {
    let hex: String = (0..32).map(|_| char::from_digit(fastrand::u32(..16), 16).unwrap()).collect();
    let id = format!("urn:uuid:{}-{}-4{}-a{}-{}", &hex[..8], &hex[8..12], &hex[13..16], &hex[17..20], &hex[20..]);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <e:Envelope xmlns:e=\"http://www.w3.org/2003/05/soap-envelope\" xmlns:w=\"http://schemas.xmlsoap.org/ws/2004/08/addressing\" \
         xmlns:d=\"http://schemas.xmlsoap.org/ws/2005/04/discovery\" xmlns:dn=\"http://www.onvif.org/ver10/network/wsdl\">\
         <e:Header><w:MessageID>{id}</w:MessageID><w:To e:mustUnderstand=\"true\">urn:schemas-xmlsoap-org:ws:2005:04:discovery</w:To>\
         <w:Action a:mustUnderstand=\"true\" xmlns:a=\"http://schemas.xmlsoap.org/ws/2004/08/addressing\">http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe</w:Action></e:Header>\
         <e:Body><d:Probe><d:Types>dn:NetworkVideoTransmitter</d:Types></d:Probe></e:Body></e:Envelope>"
    )
}

/// "Gate+Cam" or "Gate%20Cam" in a scope, as "Gate Cam".
fn decode_scope(value: &str) -> String {
    let bytes = value.replace('+', " ").into_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What a ProbeMatch says about the camera that sent it.
pub fn parse_probe_match(xml: &str, from: Ipv4Addr) -> Camera {
    let from_text = from.to_string();
    let xaddrs = onvif::text_of(xml, "XAddrs");
    let scopes = onvif::text_of(xml, "Scopes");
    let scope = |key: &str| {
        let prefix = format!("onvif://www.onvif.org/{key}/");
        scopes.split_whitespace().find_map(|s| s.strip_prefix(&prefix)).map(decode_scope).unwrap_or_default()
    };
    // Prefer the address on the interface that answered; cameras sometimes
    // list an IPv6 or a stale address first.
    let listed: Vec<&str> = xaddrs.split_whitespace().collect();
    let device_url =
        listed.iter().find(|x| x.contains(&from_text)).or(listed.first()).map_or_else(|| format!("http://{from}/onvif/device_service"), |x| x.to_string());
    Camera { host: from_text, device_url, name: scope("name"), hardware: scope("hardware"), location: scope("location"), via: "ws-discovery", confirmed: None }
}

/// Cameras that answer the probe, by address. One socket per interface: the
/// multicast goes out of it, and the same probe is sent straight to every
/// address in that interface's /24, which a camera answers even when the
/// multicast never reached it.
async fn probe(subnets: &[Subnet], wait: Duration) -> BTreeMap<Ipv4Addr, Camera> {
    let mut sockets = Vec::new();
    let body = probe_xml();
    for subnet in subnets {
        let Ok(socket) = UdpSocket::bind((subnet.own, 0)).await else { continue };
        let _ = socket2::SockRef::from(&socket).set_multicast_if_v4(&subnet.own);
        let socket = std::sync::Arc::new(socket);
        let (s, subnet, body) = (socket.clone(), subnet.clone(), body.clone());
        tauri::async_runtime::spawn(async move {
            // Twice, a moment apart: one UDP probe on a busy switch is easy to lose.
            let _ = s.send_to(body.as_bytes(), (MULTICAST, WS_DISCOVERY_PORT)).await;
            tokio::time::sleep(Duration::from_millis(400)).await;
            let _ = s.send_to(body.as_bytes(), (MULTICAST, WS_DISCOVERY_PORT)).await;
            for last in 1..=254 {
                let host = subnet.host(last);
                if host != subnet.own {
                    let _ = s.send_to(body.as_bytes(), (host, WS_DISCOVERY_PORT)).await;
                }
            }
        });
        sockets.push(socket);
    }
    let deadline = Instant::now() + wait;
    let mut listening = JoinSet::new();
    for socket in sockets {
        listening.spawn(async move {
            let mut heard = Vec::new();
            let mut buf = vec![0u8; 65536];
            while let Ok(Ok((n, SocketAddr::V4(from)))) = tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await {
                let xml = String::from_utf8_lossy(&buf[..n]);
                if xml.to_lowercase().contains("probematch") {
                    heard.push(parse_probe_match(&xml, *from.ip()));
                }
            }
            heard
        });
    }
    let mut found = BTreeMap::new();
    while let Some(Ok(heard)) = listening.join_next().await {
        for camera in heard {
            if let Ok(ip) = camera.host.parse() {
                found.insert(ip, camera);
            }
        }
    }
    found
}

/// Does this host:port serve ONVIF? Asked for the time, which needs no login.
/// A SOAP answer of any kind - even a fault or a 401 - means an ONVIF
/// service is there; a plain web server gives 404 for the path.
pub async fn onvif_at(host: Ipv4Addr, port: u16) -> Option<(String, bool)> {
    let device_url = format!("http://{host}:{port}/onvif/device_service");
    let client = Client::new(&device_url, "", "");
    match client.request(&device_url, &format!("<tds:GetSystemDateAndTime xmlns:tds=\"{NS_TDS}\"/>")).await {
        Ok(xml) if xml.to_lowercase().contains("systemdateandtime") => Some((device_url, true)),
        Ok(_) => None,
        Err(Error::Fault(_) | Error::Http(401)) => Some((device_url, false)),
        Err(_) => None,
    }
}

/// The make and model, when the camera will say (many will without a login).
async fn describe(device_url: &str, username: &str, password: &str) -> (String, String) {
    let client = Client::new(device_url, username, password);
    let Ok(xml) = client.request(device_url, &format!("<tds:GetDeviceInformation xmlns:tds=\"{NS_TDS}\"/>")).await else {
        return (String::new(), String::new());
    };
    let field = |name| onvif::text_of(&xml, name);
    let hardware = [field("Manufacturer"), field("HardwareId")].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
    (field("Model"), hardware)
}

/// Every address in a /24, knocked on the ONVIF ports. Forty hosts at a
/// time, all their ports at once: a network with nothing on it takes about
/// five seconds.
async fn sweep(
    subnet: &Subnet,
    ports: &[u16],
    skip: &BTreeMap<Ipv4Addr, Camera>,
    on_progress: &(dyn Fn(usize, usize, String) + Sync),
) -> Vec<(Ipv4Addr, String, bool)> {
    let hosts: Vec<Ipv4Addr> = (1..=254).map(|last| subnet.host(last)).collect();
    let network = format!("{}.{}.{}", subnet.base[0], subnet.base[1], subnet.base[2]);
    let mut hits = Vec::new();
    for (i, batch) in hosts.chunks(SWEEP_BATCH).enumerate() {
        let mut knocks = JoinSet::new();
        for &host in batch.iter().filter(|&&h| h != subnet.own && !skip.contains_key(&h)) {
            let ports = ports.to_vec();
            knocks.spawn(async move {
                let mut open = JoinSet::new();
                for port in ports {
                    open.spawn(async move { tcp_open(SocketAddr::V4(SocketAddrV4::new(host, port)), KNOCK_TIMEOUT).await.then_some(port) });
                }
                let mut open_ports: Vec<u16> = open.join_all().await.into_iter().flatten().collect();
                open_ports.sort_by_key(|p| ONVIF_PORTS.iter().position(|q| q == p).unwrap_or(usize::MAX));
                for port in open_ports {
                    if let Some((url, confirmed)) = onvif_at(host, port).await {
                        return Some((host, url, confirmed));
                    }
                }
                None
            });
        }
        hits.extend(knocks.join_all().await.into_iter().flatten());
        on_progress(((i + 1) * SWEEP_BATCH).min(254), 254, network.clone());
    }
    hits
}

/// Everything that looks like a camera on every network this PC is on.
pub async fn discover(username: &str, password: &str, onvif_port: u16, on_progress: impl Fn(usize, usize, String) + Send + Sync) -> Found {
    let subnets = local_subnets();
    let mut ports = ONVIF_PORTS.to_vec();
    if !ports.contains(&onvif_port) {
        ports.push(onvif_port);
    }
    let mut cameras = probe(&subnets, PROBE_WAIT).await;
    for subnet in &subnets {
        for (host, device_url, confirmed) in sweep(subnet, &ports, &cameras, &on_progress).await {
            let camera = Camera {
                host: host.to_string(),
                device_url,
                name: String::new(),
                hardware: String::new(),
                location: String::new(),
                via: "scan",
                confirmed: Some(confirmed),
            };
            cameras.insert(host, camera);
        }
    }
    let mut described = JoinSet::new();
    for (_, mut camera) in cameras {
        let (username, password) = (username.to_string(), password.to_string());
        described.spawn(async move {
            if camera.name.is_empty() && camera.hardware.is_empty() {
                (camera.name, camera.hardware) = describe(&camera.device_url, &username, &password).await;
            }
            camera
        });
    }
    let mut cameras = described.join_all().await;
    cameras.sort_by_key(|c| c.host.parse::<Ipv4Addr>().unwrap_or(Ipv4Addr::UNSPECIFIED));
    let networks = subnets.iter().map(|s| format!("{}.{}.{}.0/24", s.base[0], s.base[1], s.base[2])).collect();
    Found { cameras, networks }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StreamsArgs {
    pub device_url: String,
    pub host: String,
    #[serde(deserialize_with = "crate::scale::frames::lenient_number")]
    pub port: f64,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stream {
    pub profile_token: String,
    pub profile_name: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub stream_uri: String,
    pub snapshot_uri: String,
}

/// The streams one camera offers: profile, RTSP URI, snapshot URI and
/// resolution. Needs the camera's login.
pub async fn streams_of(args: StreamsArgs) -> Result<Vec<Stream>, String> {
    let url = if args.device_url.trim().is_empty() {
        let port = if args.port >= 1.0 { args.port as u16 } else { 80 };
        format!("http://{}:{port}/onvif/device_service", args.host.trim())
    } else {
        args.device_url.trim().to_string()
    };
    let mut client = Client::new(&url, &args.username, &args.password);
    client.connect().await.map_err(|e| e.to_string())?;
    let profiles = client.profiles().await.map_err(|e| e.to_string())?;
    let mut streams = Vec::new();
    for profile in profiles {
        streams.push(Stream {
            stream_uri: client.uri(Uri::Stream, &profile.token).await,
            snapshot_uri: client.uri(Uri::Snapshot, &profile.token).await,
            profile_token: profile.token,
            profile_name: profile.name,
            width: profile.width,
            height: profile.height,
        });
    }
    Ok(streams)
}

// Ported from tests/cameraDiscovery.test.js.
#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn probe_match(xaddrs: &str, scopes: &str) -> String {
        format!("<?xml version=\"1.0\"?><SOAP-ENV:Envelope><SOAP-ENV:Body><d:ProbeMatches><d:ProbeMatch><d:Scopes>{scopes}</d:Scopes><d:XAddrs>{xaddrs}</d:XAddrs></d:ProbeMatch></d:ProbeMatches></SOAP-ENV:Body></SOAP-ENV:Envelope>")
    }

    const CAMERA: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 229);

    #[test]
    fn a_probe_match_gives_the_device_address_name_and_hardware() {
        let xml = probe_match(
            "http://192.168.1.229:8899/onvif/device_service",
            "onvif://www.onvif.org/name/Gate+Cam onvif://www.onvif.org/hardware/IPC-123 onvif://www.onvif.org/location/yard%20west",
        );
        let cam = parse_probe_match(&xml, CAMERA);
        assert_eq!(cam.host, "192.168.1.229");
        assert_eq!(cam.device_url, "http://192.168.1.229:8899/onvif/device_service");
        assert_eq!((cam.name.as_str(), cam.hardware.as_str(), cam.location.as_str()), ("Gate Cam", "IPC-123", "yard west"));
        assert_eq!(cam.via, "ws-discovery");
    }

    #[test]
    fn the_address_on_the_interface_that_answered_wins_over_a_stale_one() {
        let cam = parse_probe_match(&probe_match("http://10.0.0.5/onvif/device_service http://192.168.1.229/onvif/device_service", ""), CAMERA);
        assert_eq!(cam.device_url, "http://192.168.1.229/onvif/device_service");
    }

    #[test]
    fn a_probe_match_with_no_address_falls_back_to_the_sender_on_port_80() {
        let cam = parse_probe_match(&probe_match("", ""), CAMERA);
        assert_eq!(cam.device_url, "http://192.168.1.229/onvif/device_service");
        assert_eq!(cam.name, "");
    }

    #[test]
    fn the_sweep_knocks_on_the_usual_onvif_ports_80_first() {
        assert_eq!(ONVIF_PORTS[0], 80);
        for port in [8899, 8000, 8080] {
            assert!(ONVIF_PORTS.contains(&port));
        }
    }

    #[test]
    fn a_probe_is_well_formed() {
        let xml = probe_xml();
        assert!(xml.contains("<d:Types>dn:NetworkVideoTransmitter</d:Types>"));
        assert!(Regex::new(r"urn:uuid:[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-a[0-9a-f]{3}-[0-9a-f]{12}<").unwrap().is_match(&xml));
    }

    /// A stand-in camera: answers /onvif/device_service with `status` and
    /// `body`, anything else with 404.
    async fn serve(status: u16, body: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut request = vec![0u8; 8192];
                let n = socket.read(&mut request).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..n]);
                let (status, body) = if request.starts_with("POST /onvif/device_service ") { (status, body) } else { (404, "not here") };
                let reply =
                    format!("HTTP/1.1 {status} X\r\nContent-Type: application/soap+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });
        port
    }

    #[tokio::test]
    async fn a_host_that_answers_get_system_date_and_time_is_a_confirmed_camera() {
        let port = serve(
            200,
            "<s:Envelope><s:Body><tds:GetSystemDateAndTimeResponse><tds:SystemDateAndTime/></tds:GetSystemDateAndTimeResponse></s:Body></s:Envelope>",
        )
        .await;
        assert_eq!(onvif_at(Ipv4Addr::LOCALHOST, port).await, Some((format!("http://127.0.0.1:{port}/onvif/device_service"), true)));
    }

    #[tokio::test]
    async fn a_soap_fault_or_a_401_still_marks_an_onvif_service_unconfirmed() {
        let fault = serve(500, "<s:Envelope><s:Body><s:Fault><s:Reason><s:Text>NotAuthorized</s:Text></s:Reason></s:Fault></s:Body></s:Envelope>").await;
        let denied = serve(401, "").await;
        assert_eq!(onvif_at(Ipv4Addr::LOCALHOST, fault).await.map(|f| f.1), Some(false));
        assert_eq!(onvif_at(Ipv4Addr::LOCALHOST, denied).await.map(|f| f.1), Some(false));
    }

    #[tokio::test]
    async fn a_plain_web_server_is_not_a_camera() {
        let port = serve(200, "<html><title>router</title></html>").await;
        assert_eq!(onvif_at(Ipv4Addr::LOCALHOST, port).await, None);
    }

    // The real search, so only on request, with tools/fake-onvif.py running:
    //   cargo test finds_cameras_on_this_lan -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn finds_cameras_on_this_lan() {
        let started = Instant::now();
        let found = discover("admin", "secret", 80, |_, _, _| {}).await;
        println!("{found:#?}\nin {:?}", started.elapsed());
        for camera in &found.cameras {
            let args = StreamsArgs { device_url: camera.device_url.clone(), username: "admin".into(), password: "secret".into(), ..Default::default() };
            println!("{} streams: {:#?}", camera.host, streams_of(args).await);
            let wrong = StreamsArgs { device_url: camera.device_url.clone(), username: "admin".into(), password: "wrong".into(), ..Default::default() };
            println!("{} with a wrong password: {:?}", camera.host, streams_of(wrong).await.map(|s| s.len()));
        }
    }
}
