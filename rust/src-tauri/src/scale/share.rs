//! One connection to the scale, shared by every copy of the app on the LAN.
//!
//! The device server takes only so many connections: the office's NPort has
//! four, and the ticketing software and the website hold two of them. So one
//! copy, the leader, reads the scale and passes the bytes on, just as they
//! came, to the others over TCP. Each follower parses them as if it had the
//! device server to itself, and keeps its own weighings, photos and history.
//!
//! Who leads is settled over UDP broadcast. Once a second, on every LAN it
//! sits on, each copy says who it is, which scale it reads and whether it
//! leads. A copy that hears a leader for its scale follows it. When there is
//! none, the oldest copy running takes over (oldest has the lowest id). Two
//! leaders (two copies started together, or a network split that healed) are
//! settled the same way: the younger one steps down. A leader that quits says
//! so. One that dies is missed when its relay drops, or after a few seconds
//! without its beacon.
//!
//! `Election` makes every decision and is handed the clock, so tests drive it
//! with made-up beacons and times. `Share` runs it: the sockets, the beacons,
//! and the relay the leader serves.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{broadcast, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{interval, timeout, MissedTickBehavior};

use super::discovery::local_subnets;
use super::reader::{now_ms, ScaleEvent, Sink};

pub const BEACON_PORT: u16 = 47950;
pub const RELAY_PORT: u16 = 47951;
const APP: &str = "falcon-scale-station";
const BEACON_EVERY: Duration = Duration::from_secs(1);
/// A copy not heard from for this long is gone.
const PEER_LOST_MS: i64 = 3_500;
/// A copy that has just started listens this long before it decides, to hear
/// a leader that is already there.
const LISTEN_FIRST_MS: i64 = 2_500;
/// No leader for this long, with an older copy around that should have taken
/// over: take over anyway. Two leaders are settled; none is no scale.
const LEADERLESS_MS: i64 = 8_000;
/// A leader this copy cannot connect to this many times running is passed
/// over for a while, and the scale read directly: its beacons get through
/// but its relay does not (a firewall letting one in and not the other).
const FAILS_BEFORE_PASSED_OVER: u32 = 3;
const PASSED_OVER_MS: i64 = 5 * 60_000;
/// A follower that takes longer than this to take a chunk is dropped.
const RELAY_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// This copy of the app, for as long as it runs: the time it started, so the
/// oldest copy is the lowest, and a random tail for two started together.
/// The same across scale reader restarts, so saving the settings does not
/// make the leader the youngest copy.
pub fn instance_id() -> u64 {
    static ID: OnceLock<u64> = OnceLock::new();
    *ID.get_or_init(|| ((now_ms().max(0) as u64) << 16) | fastrand::u16(..) as u64)
}

/// What the scale is called between copies: two copies share only when they
/// read the same device server, written the same way.
pub fn scale_key(host: &str, port: u16) -> String {
    format!("{}:{port}", host.trim().to_lowercase())
}

/// This PC's name, to say whose relay a follower reads.
fn computer_name() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Lead,
    Follow,
    /// Quitting, or no longer reading this scale.
    Bye,
}

/// What each copy broadcasts once a second.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Beacon {
    pub app: String,
    pub id: u64,
    pub scale: String,
    pub role: Role,
    /// The TCP port the relay is on.
    pub relay: u16,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Listening, or waiting for an older copy to take over.
    Wait,
    /// Read the scale and pass it on.
    Lead,
    /// Read it through the leader's relay.
    Follow { id: u64, addr: SocketAddr, name: String },
}

#[derive(Debug, Clone)]
struct Peer {
    ip: IpAddr,
    role: Role,
    relay: u16,
    name: String,
    seen_at: i64,
}

impl Peer {
    fn called(&self) -> String {
        if self.name.is_empty() {
            self.ip.to_string()
        } else {
            format!("{} ({})", self.name, self.ip)
        }
    }
}

pub struct Election {
    me: u64,
    scale: String,
    started_at: i64,
    leading: bool,
    peers: BTreeMap<u64, Peer>,
    leaderless_since: Option<i64>,
    /// Leaders this copy could not connect to: failures in a row, and when
    /// it may try again after being passed over.
    fails: HashMap<u64, (u32, Option<i64>)>,
}

impl Election {
    pub fn new(me: u64, scale: String, now: i64) -> Self {
        Self { me, scale, started_at: now, leading: false, peers: BTreeMap::new(), leaderless_since: None, fails: HashMap::new() }
    }

    pub fn leading(&self) -> bool {
        self.leading
    }

    pub fn hear(&mut self, beacon: &Beacon, from: IpAddr, now: i64) {
        if beacon.app != APP || beacon.id == self.me || beacon.scale != self.scale {
            return;
        }
        if beacon.role == Role::Bye {
            self.peers.remove(&beacon.id);
            return;
        }
        self.peers.insert(beacon.id, Peer { ip: from, role: beacon.role, relay: beacon.relay, name: beacon.name.clone(), seen_at: now });
    }

    /// Its relay dropped or would not connect: taken as gone until it is
    /// heard from again, so a dead leader is replaced without waiting out
    /// its beacons.
    pub fn forget(&mut self, id: u64) {
        self.peers.remove(&id);
    }

    /// Whether it was the last straw: the leader is passed over from now.
    pub fn failed(&mut self, id: u64, now: i64) -> bool {
        let entry = self.fails.entry(id).or_insert((0, None));
        entry.0 += 1;
        if entry.0 >= FAILS_BEFORE_PASSED_OVER && entry.1.is_none() {
            entry.1 = Some(now + PASSED_OVER_MS);
            return true;
        }
        false
    }

    pub fn reached(&mut self, id: u64) {
        self.fails.remove(&id);
    }

    fn passed_over(&self, id: u64, now: i64) -> bool {
        self.fails.get(&id).and_then(|f| f.1).is_some_and(|until| now < until)
    }

    pub fn decide(&mut self, now: i64) -> Decision {
        self.peers.retain(|_, p| now - p.seen_at < PEER_LOST_MS);
        self.fails.retain(|_, f| f.1.is_none_or(|until| now < until));
        let leader = self.peers.iter().find(|(&id, p)| p.role == Role::Lead && !self.passed_over(id, now));
        if let Some((&id, p)) = leader {
            self.leaderless_since = None;
            // An older leader stays; the younger one steps down to it.
            if !(self.leading && self.me < id) {
                self.leading = false;
                return Decision::Follow { id, addr: SocketAddr::new(p.ip, p.relay), name: p.called() };
            }
        }
        if self.leading {
            return Decision::Lead;
        }
        if now - self.started_at < LISTEN_FIRST_MS {
            return Decision::Wait;
        }
        let since = *self.leaderless_since.get_or_insert(now);
        let oldest = self.peers.keys().all(|&id| id > self.me || self.passed_over(id, now));
        if oldest || now - since >= LEADERLESS_MS {
            self.leading = true;
            return Decision::Lead;
        }
        Decision::Wait
    }
}

/// Where the beacons go. On the LAN: the broadcast address of every /24 this
/// PC sits on, and the all-ones one, on BEACON_PORT. In tests: copies on
/// this machine, each on its own port.
#[derive(Debug, Clone)]
pub struct Net {
    pub beacon_port: u16,
    pub relay_port: u16,
    pub targets: Option<Vec<SocketAddr>>,
}

impl Default for Net {
    fn default() -> Self {
        Self { beacon_port: BEACON_PORT, relay_port: RELAY_PORT, targets: None }
    }
}

impl Net {
    fn targets(&self) -> Vec<SocketAddr> {
        if let Some(targets) = &self.targets {
            return targets.clone();
        }
        local_subnets().iter().map(|s| s.broadcast()).chain([Ipv4Addr::BROADCAST]).map(|ip| SocketAddr::from((ip, self.beacon_port))).collect()
    }
}

/// The running side of the election, for one scale reader.
pub struct Share {
    election: Mutex<Election>,
    decision: watch::Sender<Decision>,
    /// While leading: where the scale's bytes go to be passed on.
    relay: Mutex<Option<broadcast::Sender<Bytes>>>,
    relay_port: Mutex<u16>,
    net: Net,
    name: String,
    sink: Sink,
}

impl Share {
    pub fn new(scale: String, net: Net, sink: Sink) -> Arc<Self> {
        Arc::new(Self {
            election: Mutex::new(Election::new(instance_id(), scale, now_ms())),
            decision: watch::channel(Decision::Wait).0,
            relay: Mutex::new(None),
            relay_port: Mutex::new(net.relay_port),
            net,
            name: computer_name(),
            sink,
        })
    }

    #[cfg(test)]
    fn with_id(id: u64, scale: String, net: Net, sink: Sink) -> Arc<Self> {
        let share = Self::new(scale, net, sink);
        share.election().me = id;
        share
    }

    fn election(&self) -> MutexGuard<'_, Election> {
        self.election.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn note(&self, message: String) {
        (self.sink)(ScaleEvent::Note(message));
    }

    pub fn decisions(&self) -> watch::Receiver<Decision> {
        self.decision.subscribe()
    }

    fn decide(&self) {
        let decision = self.election().decide(now_ms());
        self.decision.send_if_modified(|current| {
            let changed = *current != decision;
            *current = decision;
            changed
        });
    }

    /// The relay to this leader dropped or would not connect.
    pub fn lost(&self, id: u64, connected: bool) {
        let passed_over = {
            let mut election = self.election();
            election.forget(id);
            !connected && election.failed(id, now_ms())
        };
        if passed_over {
            self.note(format!(
                "could not reach the PC sharing the scale {FAILS_BEFORE_PASSED_OVER} times running, though its beacons come through \
                 (a firewall?): reading the scale directly for the next {} minutes",
                PASSED_OVER_MS / 60_000
            ));
        }
        self.decide();
    }

    pub fn reached(&self, id: u64) {
        self.election().reached(id);
    }

    /// Starts passing the scale on; it stops when the guard is dropped, and
    /// every follower's connection with it.
    pub fn lead(self: &Arc<Self>) -> Relaying {
        *self.relay.lock().unwrap() = Some(broadcast::channel(256).0);
        Relaying(self.clone())
    }

    fn pass_on(&self, chunk: &[u8]) {
        if let Some(tx) = self.relay.lock().unwrap().as_ref() {
            let _ = tx.send(Bytes::copy_from_slice(chunk));
        }
    }

    fn beacon(&self, role: Role) -> Vec<u8> {
        let election = self.election();
        let beacon =
            Beacon { app: APP.into(), id: election.me, scale: election.scale.clone(), role, relay: *self.relay_port.lock().unwrap(), name: self.name.clone() };
        serde_json::to_vec(&beacon).unwrap_or_default()
    }

    /// Said on the way out, so a follower takes over now rather than in a
    /// few seconds. Blocking, but a datagram or two is nothing.
    pub fn bye(&self) {
        let Ok(socket) = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else { return };
        let _ = socket.set_broadcast(true);
        let bye = self.beacon(Role::Bye);
        for target in self.net.targets() {
            let _ = socket.send_to(&bye, target);
        }
    }

    /// The sockets and the tasks behind them, until the guard is dropped.
    /// Without a socket to hear the others on, this copy just leads.
    pub async fn start(self: &Arc<Self>) -> Tasks {
        let mut tasks = Tasks(Vec::new());
        let relay = match TcpListener::bind((Ipv4Addr::UNSPECIFIED, self.net.relay_port)).await {
            Ok(l) => Ok(l),
            // Something else has the port: any port will do, the beacon says which.
            Err(_) => TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await,
        };
        let beacons = beacon_socket(self.net.beacon_port);
        match (relay, beacons) {
            (Ok(relay), Ok(beacons)) => {
                *self.relay_port.lock().unwrap() = relay.local_addr().map(|a| a.port()).unwrap_or(self.net.relay_port);
                tasks.0.push(tokio::spawn(self.clone().accept(relay)));
                tasks.0.push(tokio::spawn(self.clone().beacons(beacons)));
            }
            (relay, beacons) => {
                let err = relay.err().or(beacons.err()).map(|e| e.to_string()).unwrap_or_default();
                self.note(format!("cannot share the scale with other PCs ({err}): reading it directly"));
                self.election().leading = true;
                self.decision.send_replace(Decision::Lead);
            }
        }
        tasks
    }

    /// Says who this copy is once a second, hears the others, and decides
    /// again on each.
    async fn beacons(self: Arc<Self>, socket: UdpSocket) {
        let mut tick = interval(BEACON_EVERY);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut buf = [0u8; 1500];
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    let beacon = self.beacon(if self.election().leading() { Role::Lead } else { Role::Follow });
                    for target in self.net.targets() {
                        let _ = socket.send_to(&beacon, target).await;
                    }
                }
                received = socket.recv_from(&mut buf) => {
                    if let (Ok((n, from)), now) = (received, now_ms()) {
                        if let Ok(beacon) = serde_json::from_slice::<Beacon>(&buf[..n]) {
                            self.election().hear(&beacon, from.ip(), now);
                        }
                    }
                }
            }
            self.decide();
        }
    }

    /// Hands each follower that connects the scale's bytes from then on,
    /// while this copy leads. Others are hung up on.
    async fn accept(self: Arc<Self>, listener: TcpListener) {
        let mut followers = JoinSet::new();
        loop {
            let Ok((socket, from)) = listener.accept().await else { continue };
            while followers.try_join_next().is_some() {}
            let Some(chunks) = self.relay.lock().unwrap().as_ref().map(|tx| tx.subscribe()) else { continue };
            let share = self.clone();
            followers.spawn(async move {
                share.note(format!("{} is reading the scale through this PC", from.ip()));
                serve(socket, chunks).await;
                share.note(format!("{} stopped reading the scale through this PC", from.ip()));
            });
        }
    }
}

/// Bound with the address reusable, so a reader started while the last one
/// is still shutting down can have the port too.
fn beacon_socket(port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_broadcast(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())?;
    UdpSocket::from_std(socket.into())
}

async fn serve(mut socket: TcpStream, mut chunks: broadcast::Receiver<Bytes>) {
    let _ = socket.set_nodelay(true);
    loop {
        match chunks.recv().await {
            Ok(chunk) => {
                if !matches!(timeout(RELAY_WRITE_TIMEOUT, socket.write_all(&chunk)).await, Ok(Ok(()))) {
                    return;
                }
            }
            // Fell behind: the frames it missed are gone, and the reader
            // finds the next one.
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// While leading; dropping it hangs up on every follower.
pub struct Relaying(Arc<Share>);

impl Relaying {
    pub fn pass_on(&self, chunk: &[u8]) {
        self.0.pass_on(chunk);
    }
}

impl Drop for Relaying {
    fn drop(&mut self) {
        *self.0.relay.lock().unwrap() = None;
    }
}

/// The tasks a Share runs, stopped when this is dropped.
pub struct Tasks(Vec<JoinHandle<()>>);

impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

/// The first decision that is not to wait.
pub async fn next(decisions: &mut watch::Receiver<Decision>) -> Decision {
    loop {
        let decision = decisions.borrow_and_update().clone();
        if decision != Decision::Wait {
            return decision;
        }
        if decisions.changed().await.is_err() {
            return Decision::Lead;
        }
    }
}

/// Returns once the decision is no longer `from`; never without sharing.
pub async fn moved(decisions: &mut Option<watch::Receiver<Decision>>, from: &Decision) {
    let Some(decisions) = decisions else { return std::future::pending().await };
    loop {
        if *decisions.borrow_and_update() != *from {
            return;
        }
        if decisions.changed().await.is_err() {
            return std::future::pending().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCALE: &str = "192.168.1.9:950";

    fn beacon(id: u64, role: Role) -> Beacon {
        Beacon { app: APP.into(), id, scale: SCALE.into(), role, relay: RELAY_PORT, name: String::new() }
    }

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, last))
    }

    fn follows(d: &Decision) -> Option<u64> {
        if let Decision::Follow { id, .. } = d {
            Some(*id)
        } else {
            None
        }
    }

    #[test]
    fn beacons_go_out_as_json_and_back() {
        let b = beacon(7, Role::Lead);
        let text = serde_json::to_string(&b).unwrap();
        assert!(text.contains(r#""role":"lead""#), "{text}");
        assert_eq!(serde_json::from_str::<Beacon>(&text).unwrap(), b);
    }

    #[test]
    fn listens_first_then_leads_when_no_one_else_does() {
        let mut e = Election::new(10, SCALE.into(), 0);
        assert_eq!(e.decide(1_000), Decision::Wait);
        assert_eq!(e.decide(2_500), Decision::Lead);
        assert!(e.leading());
    }

    #[test]
    fn follows_a_leader_already_there_even_a_younger_one() {
        let mut e = Election::new(10, SCALE.into(), 0);
        e.hear(&beacon(99, Role::Lead), ip(23), 500);
        let d = e.decide(600);
        assert_eq!(d, Decision::Follow { id: 99, addr: SocketAddr::new(ip(23), RELAY_PORT), name: "192.168.1.23".into() });
        assert_eq!(follows(&e.decide(3_900)), Some(99), "not heard from for 3.4 s: still there");
        assert_eq!(e.decide(4_000), Decision::Lead, "3.5 s: gone");
    }

    #[test]
    fn the_oldest_follower_takes_over_from_a_leader_that_is_gone() {
        let (mut old, mut young) = (Election::new(10, SCALE.into(), 0), Election::new(20, SCALE.into(), 0));
        for e in [&mut old, &mut young] {
            e.hear(&beacon(5, Role::Lead), ip(5), 3_000);
        }
        old.hear(&beacon(20, Role::Follow), ip(20), 3_000);
        young.hear(&beacon(10, Role::Follow), ip(10), 3_000);
        assert_eq!((follows(&old.decide(3_100)), follows(&young.decide(3_100))), (Some(5), Some(5)));
        // The leader's relay drops.
        old.forget(5);
        young.forget(5);
        assert_eq!(old.decide(3_200), Decision::Lead);
        assert_eq!(young.decide(3_200), Decision::Wait, "the older one takes over");
        young.hear(&beacon(10, Role::Lead), ip(10), 4_000);
        assert_eq!(follows(&young.decide(4_000)), Some(10));
    }

    #[test]
    fn a_leader_that_stops_beaconing_is_missed() {
        let mut e = Election::new(10, SCALE.into(), 0);
        e.hear(&beacon(5, Role::Lead), ip(5), 3_000);
        assert_eq!(follows(&e.decide(6_000)), Some(5));
        assert_eq!(e.decide(6_500), Decision::Lead, "3.5 s without a word");
    }

    #[test]
    fn of_two_leaders_the_younger_steps_down() {
        let mut e = Election::new(10, SCALE.into(), 0);
        assert_eq!(e.decide(3_000), Decision::Lead);
        e.hear(&beacon(20, Role::Lead), ip(20), 3_100);
        assert_eq!(e.decide(3_100), Decision::Lead, "older: stays");
        e.hear(&beacon(5, Role::Lead), ip(5), 3_200);
        assert_eq!(follows(&e.decide(3_200)), Some(5), "younger: steps down");
        assert!(!e.leading());
    }

    #[test]
    fn a_leader_that_says_bye_is_gone_at_once() {
        let mut e = Election::new(10, SCALE.into(), 0);
        e.hear(&beacon(5, Role::Lead), ip(5), 3_000);
        e.hear(&beacon(5, Role::Bye), ip(5), 3_100);
        assert_eq!(e.decide(3_100), Decision::Lead);
    }

    #[test]
    fn copies_reading_another_scale_and_its_own_beacons_are_not_heard() {
        let mut e = Election::new(10, SCALE.into(), 0);
        e.hear(&Beacon { scale: "192.168.1.10:950".into(), ..beacon(5, Role::Lead) }, ip(5), 3_000);
        e.hear(&Beacon { app: "something-else".into(), ..beacon(6, Role::Lead) }, ip(6), 3_000);
        e.hear(&beacon(10, Role::Lead), ip(10), 3_000);
        assert_eq!(e.decide(3_000), Decision::Lead);
    }

    #[test]
    fn leads_anyway_when_an_older_copy_never_does() {
        let mut e = Election::new(10, SCALE.into(), 0);
        for t in (0..=10_000).step_by(1_000) {
            e.hear(&beacon(5, Role::Follow), ip(5), t);
            let d = e.decide(t);
            assert_eq!(d == Decision::Lead, t >= 2_500 + LEADERLESS_MS, "at {t}: {d:?}");
        }
    }

    #[test]
    fn a_leader_whose_relay_never_connects_is_passed_over_for_a_while() {
        let mut e = Election::new(10, SCALE.into(), 0);
        let mut t = 3_000;
        for round in 1..=FAILS_BEFORE_PASSED_OVER {
            e.hear(&beacon(5, Role::Lead), ip(5), t);
            assert_eq!(follows(&e.decide(t)), Some(5));
            assert_eq!(e.failed(5, t), round == FAILS_BEFORE_PASSED_OVER);
            t += 1_000;
        }
        e.hear(&beacon(5, Role::Lead), ip(5), t);
        assert_eq!(e.decide(t), Decision::Lead);
        // Leading beside an older leader it cannot reach: it does not step down.
        assert_eq!(e.decide(t + 1), Decision::Lead);
        let later = t + PASSED_OVER_MS;
        e.hear(&beacon(5, Role::Lead), ip(5), later);
        assert_eq!(follows(&e.decide(later)), Some(5), "tried again after the wait");
    }

    #[test]
    fn a_reached_leader_starts_its_count_over() {
        let mut e = Election::new(10, SCALE.into(), 0);
        assert!(!e.failed(5, 0));
        assert!(!e.failed(5, 0));
        e.reached(5);
        assert!(!e.failed(5, 0));
        assert!(!e.failed(5, 0));
        assert!(e.failed(5, 0));
    }

    #[test]
    fn the_scale_key_ignores_case_and_spaces() {
        assert_eq!(scale_key(" NPort.local ", 950), "nport.local:950");
    }

    #[test]
    fn the_id_stays_put_and_puts_older_copies_first() {
        assert_eq!(instance_id(), instance_id());
        assert!(instance_id() >> 16 <= now_ms() as u64);
    }

    // Several copies on this machine, each its own Share, against a fake
    // indicator: the real sockets, beacons, relay and failover.
    mod live {
        use super::super::super::reader::{ScaleReader, ScaleSettings, ScaleStatus};
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::time::{sleep, Instant};

        fn free_udp() -> u16 {
            std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
        }

        fn free_tcp() -> u16 {
            std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
        }

        /// A scale indicator sending a frame every 50 ms to each connection,
        /// counting the connections it has open.
        async fn indicator() -> (u16, Arc<AtomicUsize>) {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let open = Arc::new(AtomicUsize::new(0));
            let counted = open.clone();
            tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let open = counted.clone();
                    open.fetch_add(1, Ordering::SeqCst);
                    tokio::spawn(async move {
                        while socket.write_all(b"\x02  12340LG \r\n").await.is_ok() {
                            sleep(Duration::from_millis(50)).await;
                        }
                        open.fetch_sub(1, Ordering::SeqCst);
                    });
                }
            });
            (port, open)
        }

        struct Copy {
            reader: ScaleReader,
            notes: Arc<Mutex<Vec<String>>>,
        }

        impl Copy {
            fn status(&self) -> ScaleStatus {
                self.reader.status()
            }
            fn reading(&self) -> bool {
                let s = self.status();
                s.connected && s.weight == Some(12340.0) && s.last_frame_at.is_some_and(|at| now_ms() - at < 500)
            }
        }

        fn copies(n: usize, scale_port: u16) -> Vec<Copy> {
            let beacon_ports: Vec<u16> = (0..n).map(|_| free_udp()).collect();
            let targets: Vec<SocketAddr> = beacon_ports.iter().map(|&p| SocketAddr::from((Ipv4Addr::LOCALHOST, p))).collect();
            let settings: ScaleSettings = serde_json::from_value(serde_json::json!({ "enabled": true, "host": "127.0.0.1", "port": scale_port })).unwrap();
            beacon_ports
                .iter()
                .enumerate()
                .map(|(i, &beacon_port)| {
                    let notes = Arc::new(Mutex::new(Vec::new()));
                    let kept = notes.clone();
                    let sink: Sink = Arc::new(move |e| {
                        if let ScaleEvent::Note(n) = e {
                            kept.lock().unwrap().push(n);
                        }
                    });
                    let net = Net { beacon_port, relay_port: free_tcp(), targets: Some(targets.clone()) };
                    // Started in order: the first is the oldest.
                    let share = Share::with_id(100 + i as u64, scale_key("127.0.0.1", scale_port), net, sink.clone());
                    Copy { reader: ScaleReader::start_shared(&settings, None, sink, Some(share)), notes }
                })
                .collect()
        }

        async fn until(what: &str, seconds: u64, done: impl Fn() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(seconds);
            while !done() {
                assert!(Instant::now() < deadline, "timed out waiting for: {what}");
                sleep(Duration::from_millis(100)).await;
            }
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn one_copy_reads_the_scale_and_the_rest_read_it_through_that_one() {
            let (port, open) = indicator().await;
            let mut copies = copies(3, port);
            until("every copy reading", 10, || copies.iter().all(Copy::reading)).await;
            assert_eq!(open.load(Ordering::SeqCst), 1, "one connection to the scale");
            assert_eq!(copies[0].status().via, None, "the oldest leads");
            assert!(copies[1].status().via.is_some() && copies[2].status().via.is_some());
            assert_eq!(copies[0].notes.lock().unwrap().iter().filter(|n| n.contains("is reading the scale through this PC")).count(), 2);

            // The leader quits: the next oldest takes over, and the last one
            // reads through it.
            let leader = copies.remove(0);
            leader.reader.stop();
            drop(leader);
            until("the next oldest reading the scale itself", 10, || copies[0].status().via.is_none() && copies[0].reading()).await;
            until("the youngest reading through it", 10, || copies[1].status().via.is_some() && copies[1].reading()).await;
            until("one connection to the scale again", 5, || open.load(Ordering::SeqCst) == 1).await;
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn a_leader_that_dies_without_a_word_is_replaced() {
            let (port, open) = indicator().await;
            let mut copies = copies(2, port);
            until("both reading", 10, || copies.iter().all(Copy::reading)).await;
            // Dropped without stop(): no bye, as when the PC loses power.
            drop(copies.remove(0));
            let started = Instant::now();
            until("the other reading the scale itself", 10, || copies[0].status().via.is_none() && copies[0].reading()).await;
            assert!(started.elapsed() < Duration::from_secs(8), "{:?}", started.elapsed());
            until("one connection to the scale", 5, || open.load(Ordering::SeqCst) == 1).await;
        }
    }
}
