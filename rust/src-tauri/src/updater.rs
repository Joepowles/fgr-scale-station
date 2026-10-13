//! Updates, from the same GitHub release feed as the Electron app.
//!
//! Two channels. Stable follows the newest release not marked pre-release
//! (its latest.yml). Beta follows the newest release of either kind: a
//! pre-release tagged v0.2.0-beta.1 carries beta.yml, and a stable release
//! is taken too, so a beta PC is never behind. Going back to stable from a
//! beta version may step down, so the switch actually happens - but never to
//! an Electron build (below 0.2.0): that one would be installed beside this
//! app rather than over it, and both would read the scale. Going back to the
//! Electron app is done by hand from the releases page.
//!
//! The feed files are electron-builder's (version, path, sha512, size), so
//! the Electron app's updater can move a PC over to this app, and this one
//! reads what the release workflow writes for it in the same form.
//!
//! A yard PC is often on a poor link. The installer is downloaded into a
//! .part file that a dropped connection leaves in place, and the next try
//! asks only for the rest (an HTTP range). Failed checks are tried again
//! soon, backing off up to an hour, rather than waiting six hours. Nothing is
//! run until the whole file matches the feed's SHA-512.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha512};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;

pub const REPO: &str = "Joepowles/fgr-scale-station";
/// The first Rust build, betas included. Anything older is the Electron app.
const FIRST_RUST_VERSION: [u64; 3] = [0, 2, 0];
const FIRST_CHECK: Duration = Duration::from_secs(30);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const RETRY_MINUTES: [u64; 5] = [2, 5, 15, 30, 60];
const FEED_TIMEOUT: Duration = Duration::from_secs(30);
/// No bytes for this long and the download is given up, to resume later.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);

// ── Versions ───────────────────────────────────────────────────────────────

/// A semver version: 0.2.0, 0.2.0-beta.1. Build metadata is ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    core: [u64; 3],
    pre: Vec<String>,
}

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().trim_start_matches('v');
        let text = text.split('+').next()?;
        let (core, pre) = text.split_once('-').map_or((text, None), |(c, p)| (c, Some(p)));
        let parts: Vec<u64> = core.split('.').map(|p| p.parse().ok()).collect::<Option<_>>()?;
        let [major, minor, patch] = parts[..] else { return None };
        let pre = pre.map(|p| p.split('.').map(String::from).collect()).unwrap_or_default();
        Some(Self { core: [major, minor, patch], pre })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }

    /// The channel a pre-release names ("beta" in 0.2.0-beta.1).
    pub fn channel(&self) -> Option<&str> {
        self.pre.first().map(String::as_str)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering::*;
        self.core.cmp(&other.core).then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Equal,
            (true, false) => Greater, // 0.2.0 is after 0.2.0-beta.9
            (false, true) => Less,
            (false, false) => {
                for (a, b) in self.pre.iter().zip(&other.pre) {
                    let order = match (a.parse::<u64>(), b.parse::<u64>()) {
                        (Ok(x), Ok(y)) => x.cmp(&y),
                        (Ok(_), Err(_)) => Less,
                        (Err(_), Ok(_)) => Greater,
                        (Err(_), Err(_)) => a.cmp(b),
                    };
                    if order != Equal {
                        return order;
                    }
                }
                self.pre.len().cmp(&other.pre.len())
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [a, b, c] = self.core;
        write!(f, "{a}.{b}.{c}")?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Channel {
    Stable,
    Beta,
}

impl Channel {
    /// Anything unknown is stable.
    pub fn named(name: &str) -> Self {
        if name == "beta" {
            Self::Beta
        } else {
            Self::Stable
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
        }
    }
}

/// Whether to move from `current` to what the feed offers, and if not, why
/// not (None: simply nothing newer).
pub fn decide(channel: Channel, current: &Version, offered: &Version) -> Result<bool, Option<String>> {
    if offered == current {
        return Err(None);
    }
    if offered.core < FIRST_RUST_VERSION {
        let reason = format!("{offered} is the Electron app; going back to it is done by hand, from the releases page");
        // Only worth saying when it would otherwise have been taken.
        let would_take = offered > current || (channel == Channel::Stable && current.is_prerelease());
        return Err(would_take.then_some(reason));
    }
    let take = offered > current || (channel == Channel::Stable && current.is_prerelease());
    if take {
        Ok(true)
    } else {
        Err(None)
    }
}

// ── The feed ───────────────────────────────────────────────────────────────

/// One release as its feed file describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: Version,
    /// The installer's file name, on the release.
    pub file: String,
    pub sha512: String,
    pub size: Option<u64>,
}

impl Release {
    pub fn url(&self) -> String {
        format!("https://github.com/{REPO}/releases/download/v{}/{}", self.version, self.file)
    }
}

/// Reads electron-builder's latest.yml / beta.yml: the top-level version,
/// path and sha512, and the size from the files entry for that path.
pub fn parse_feed(text: &str) -> Result<Release, String> {
    let value = |line: &str, key: &str| -> Option<String> {
        let rest = line.strip_prefix(key)?.strip_prefix(':')?;
        Some(rest.trim().trim_matches(|c| c == '\'' || c == '"').to_string())
    };
    let (mut version, mut path, mut sha512) = (None, None, None);
    let mut files: Vec<(String, Option<u64>)> = Vec::new();
    for line in text.lines() {
        let indented = line.starts_with(' ');
        let trimmed = line.trim().trim_start_matches("- ");
        if !indented {
            version = version.or_else(|| value(trimmed, "version"));
            path = path.or_else(|| value(trimmed, "path"));
            sha512 = sha512.or_else(|| value(trimmed, "sha512"));
        } else if let Some(url) = value(trimmed, "url") {
            files.push((url, None));
        } else if let (Some(size), Some(last)) = (value(trimmed, "size"), files.last_mut()) {
            last.1 = size.parse().ok();
        }
    }
    let version = version.and_then(|v| Version::parse(&v)).ok_or("the update file has no version")?;
    let file = path.or_else(|| files.first().map(|f| f.0.clone())).ok_or("the update file names no installer")?;
    let sha512 = sha512.ok_or("the update file has no checksum")?;
    let size = files.iter().find(|f| f.0 == file).and_then(|f| f.1);
    Ok(Release { version, file, sha512, size })
}

/// The newest release tag a beta install follows, from the releases' Atom
/// feed (newest first): a stable release or a beta, not an alpha or other.
pub fn newest_beta_tag(atom: &str) -> Option<String> {
    atom.split("/releases/tag/").skip(1).filter_map(|rest| rest.split(['"', '<']).next()).find_map(|tag| {
        let channel = Version::parse(tag)?.channel().map(String::from);
        matches!(channel.as_deref(), None | Some("beta")).then(|| tag.to_string())
    })
}

/// A GET that answers its text, 404 as None.
async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<Option<String>, String> {
    let response = client.get(url).send().await.map_err(|e| describe(&e))?;
    match response.status().as_u16() {
        404 => Ok(None),
        200..=299 => response.text().await.map(Some).map_err(|e| describe(&e)),
        status => Err(format!("GitHub answered {status} for {url}")),
    }
}

fn describe(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        "GitHub did not answer in time".into()
    } else if err.is_connect() {
        format!("could not reach GitHub ({err})")
    } else {
        err.to_string()
    }
}

const NOT_PUBLISHED_YET: &str = "the newest release has no update file yet. It is probably still being published";

/// What the channel's feed offers now.
async fn offered(client: &reqwest::Client, channel: Channel) -> Result<Release, String> {
    let base = format!("https://github.com/{REPO}/releases");
    let text = match channel {
        // GitHub sends /latest/download/ to the newest release not marked pre-release.
        Channel::Stable => fetch_text(client, &format!("{base}/latest/download/latest.yml")).await?,
        Channel::Beta => {
            let atom = fetch_text(client, &format!("{base}.atom")).await?.ok_or("the release feed is missing")?;
            let tag = newest_beta_tag(&atom).ok_or("the release feed lists no releases")?;
            match fetch_text(client, &format!("{base}/download/{tag}/beta.yml")).await? {
                Some(text) => Some(text),
                None => fetch_text(client, &format!("{base}/download/{tag}/latest.yml")).await?,
            }
        }
    };
    parse_feed(&text.ok_or(NOT_PUBLISHED_YET)?)
}

// ── The download ───────────────────────────────────────────────────────────

pub fn sha512_base64(file: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut hasher = Sha512::new();
    let mut f = std::fs::File::open(file)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(hasher.finalize()))
}

/// Downloads `url` to `target`, resuming a .part file a dropped connection
/// left, and checks it against `sha512`. `progress` hears the percentage.
pub async fn download(client: &reqwest::Client, url: &str, target: &Path, sha512: &str, size: Option<u64>, progress: impl Fn(u8)) -> Result<(), String> {
    if target.is_file() && sha512_base64(target).ok().as_deref() == Some(sha512) {
        return Ok(());
    }
    let part = target.with_extension("part");
    let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    let mut request = client.get(url);
    if have > 0 {
        request = request.header("Range", format!("bytes={have}-"));
    }
    let mut response = request.send().await.map_err(|e| describe(&e))?;
    let status = response.status().as_u16();
    let (mut file, mut done) = match status {
        206 => (tokio::fs::OpenOptions::new().append(true).open(&part).await.map_err(|e| e.to_string())?, have),
        // 416: the .part already has every byte; it is checked below.
        416 => (tokio::fs::OpenOptions::new().append(true).open(&part).await.map_err(|e| e.to_string())?, have),
        200 => (tokio::fs::File::create(&part).await.map_err(|e| e.to_string())?, 0),
        _ => return Err(format!("the installer download answered {status}")),
    };
    let total = size.or_else(|| response.content_length().map(|n| n + done));
    let mut last_percent = None;
    if status != 416 {
        loop {
            let chunk = tokio::time::timeout(STALL_TIMEOUT, response.chunk())
                .await
                .map_err(|_| "the download stalled; it carries on from here next time".to_string())?
                .map_err(|e| format!("the download dropped ({}); it carries on from here next time", describe(&e)))?;
            let Some(chunk) = chunk else { break };
            file.write_all(&chunk).await.map_err(|e| e.to_string())?;
            done += chunk.len() as u64;
            if let Some(total) = total.filter(|&t| t > 0) {
                let percent = (done * 100 / total).min(100) as u8;
                if last_percent != Some(percent) {
                    last_percent = Some(percent);
                    progress(percent);
                }
            }
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);
    let got = sha512_base64(&part).map_err(|e| e.to_string())?;
    if got != sha512 {
        let _ = std::fs::remove_file(&part);
        return Err("the downloaded installer does not match the release's checksum; it will be downloaded again".into());
    }
    std::fs::rename(&part, target).map_err(|e| e.to_string())
}

// ── The updater in the app ─────────────────────────────────────────────────

#[derive(Default)]
struct Shared {
    status: Option<Value>,
    /// The installer downloaded and checked, and its version.
    ready: Option<(PathBuf, Version)>,
    channel: Option<Channel>,
    installing: bool,
}

pub struct Updater {
    app: AppHandle,
    folder: PathBuf,
    current: Version,
    /// Only an installed build updates itself; STATION_UPDATER=1 tries it in development.
    enabled: bool,
    shared: Mutex<Shared>,
    wake: Notify,
    /// One check or download at a time.
    busy: tokio::sync::Mutex<()>,
}

impl Updater {
    pub fn new(app: &AppHandle, folder: PathBuf) -> Arc<Self> {
        let current = Version::parse(&app.package_info().version.to_string()).expect("the app's own version");
        let enabled = !cfg!(debug_assertions) || std::env::var("STATION_UPDATER").as_deref() == Ok("1");
        Arc::new(Self { app: app.clone(), folder, current, enabled, shared: Mutex::default(), wake: Notify::new(), busy: Default::default() })
    }

    fn log(&self, message: impl AsRef<str>) {
        crate::log(&self.app, message);
    }

    fn channel(&self) -> Channel {
        let name = self.app.state::<crate::AppState>().settings.lock().unwrap().get()["updates"]["channel"].as_str().unwrap_or("stable").to_string();
        Channel::named(&name)
    }

    fn send(&self, status: Value) {
        self.shared.lock().unwrap().status = Some(status.clone());
        let _ = self.app.emit("update:status", status);
    }

    pub fn status(&self) -> Option<Value> {
        self.shared.lock().unwrap().status.clone()
    }

    fn client() -> Result<reqwest::Client, String> {
        reqwest::Client::builder().connect_timeout(FEED_TIMEOUT).user_agent("fgr-scale-station").build().map_err(|e| e.to_string())
    }

    /// Checks now and then every six hours, sooner after a failure.
    pub fn start(self: &Arc<Self>) {
        if !self.enabled {
            return;
        }
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(FIRST_CHECK).await;
            let mut failures = 0usize;
            loop {
                let wait = match me.check_and_download().await {
                    Ok(()) => {
                        failures = 0;
                        CHECK_EVERY
                    }
                    Err(err) => {
                        let minutes = RETRY_MINUTES[failures.min(RETRY_MINUTES.len() - 1)];
                        failures += 1;
                        me.log(format!("updater: {err}; trying again in {minutes} min"));
                        me.send(json!({ "state": "error", "message": err, "detail": err, "retryMinutes": minutes }));
                        Duration::from_secs(minutes * 60)
                    }
                };
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = me.wake.notified() => {}
                }
            }
        });
    }

    /// The settings were saved: a new channel is checked straight away.
    pub fn settings_changed(&self) {
        let channel = self.channel();
        let previous = self.shared.lock().unwrap().channel.replace(channel);
        if previous.is_some_and(|p| p != channel) {
            self.log(format!("updates: {} channel", channel.name()));
            self.wake.notify_one();
        } else if previous.is_none() {
            self.log(format!("updates: {} channel", channel.name()));
        }
    }

    async fn check_and_download(&self) -> Result<(), String> {
        let _one_at_a_time = self.busy.lock().await;
        let channel = self.channel();
        self.send(json!({ "state": "checking" }));
        let client = Self::client()?;
        let release = offered(&client, channel).await?;
        if let Err(reason) = decide(channel, &self.current, &release.version) {
            if let Some(reason) = reason {
                self.log(format!("updater: {reason}"));
            }
            self.send(json!({ "state": "none", "version": self.current.to_string() }));
            return Ok(());
        }
        if self.shared.lock().unwrap().ready.as_ref().is_some_and(|(_, v)| v == &release.version) {
            self.send(json!({ "state": "ready", "version": release.version.to_string() }));
            return Ok(());
        }
        self.log(format!("update {} found, downloading", release.version));
        self.send(json!({ "state": "downloading", "version": release.version.to_string() }));
        std::fs::create_dir_all(&self.folder).map_err(|e| e.to_string())?;
        let target = self.folder.join(&release.file);
        // Anything else in the folder is an older download.
        for old in std::fs::read_dir(&self.folder).into_iter().flatten().flatten() {
            let name = old.file_name().to_string_lossy().into_owned();
            if name != release.file && !name.starts_with(release.file.trim_end_matches(".exe")) {
                let _ = std::fs::remove_file(old.path());
            }
        }
        let version = release.version.to_string();
        download(&client, &release.url(), &target, &release.sha512, release.size, |percent| {
            let _ = self.app.emit("update:status", json!({ "state": "downloading", "version": version, "percent": percent }));
        })
        .await?;
        self.shared.lock().unwrap().ready = Some((target, release.version.clone()));
        self.log(format!("update {} ready; installs on next restart", release.version));
        let _ = self.app.emit("update:ready", release.version.to_string());
        self.send(json!({ "state": "ready", "version": release.version.to_string() }));
        Ok(())
    }

    /// The Updates tab's "Check for updates": what the feed says, at once.
    /// A newer version is then downloaded in the background.
    pub async fn check_now(&self) -> Value {
        if !self.enabled {
            return json!({ "state": "dev", "message": "Updates only apply to the installed app" });
        }
        if let Some((_, version)) = self.shared.lock().unwrap().ready.clone() {
            return json!({ "state": "ready", "version": version.to_string() });
        }
        let found = match Self::client() {
            Ok(client) => offered(&client, self.channel()).await,
            Err(err) => Err(err),
        };
        match found {
            Err(err) => json!({ "state": "error", "message": err, "detail": err }),
            Ok(release) => match decide(self.channel(), &self.current, &release.version) {
                Ok(_) => {
                    self.wake.notify_one();
                    json!({ "state": "downloading", "version": release.version.to_string() })
                }
                Err(reason) => json!({ "state": "none", "version": self.current.to_string(), "message": reason }),
            },
        }
    }

    /// Runs the downloaded installer silently. `restart`: start the app again
    /// when it is done (the Restart to update button); otherwise the update
    /// goes in as the app closes. True when an installer was started.
    pub fn install(&self, restart: bool) -> bool {
        let mut shared = self.shared.lock().unwrap();
        let Some((file, version)) = shared.ready.clone() else { return false };
        if shared.installing || !file.is_file() {
            return false;
        }
        let mut cmd = std::process::Command::new(&file);
        cmd.arg("/S");
        if restart {
            cmd.arg("/R");
        }
        match cmd.spawn() {
            Ok(_) => {
                shared.installing = true;
                drop(shared);
                self.log(format!("installing {version}{}", if restart { " and restarting" } else { "" }));
                true
            }
            Err(err) => {
                drop(shared);
                self.log(format!("could not start the {version} installer: {err}"));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn orders_versions_as_semver_does() {
        assert!(v("0.2.0") > v("0.2.0-beta.9"));
        assert!(v("0.2.0-beta.10") > v("0.2.0-beta.9"));
        assert!(v("0.2.0-beta.1") > v("0.1.15"));
        assert!(v("0.1.15") > v("0.1.14-beta.12"));
        assert!(v("v1.0.0") > v("0.99.99"));
        assert!(v("0.2.0-beta") < v("0.2.0-beta.1"));
        assert_eq!(v("0.2.0+7"), v("0.2.0"));
        assert_eq!(v("0.2.0-beta.1").to_string(), "0.2.0-beta.1");
        assert_eq!(v("0.2.0-beta.1").channel(), Some("beta"));
        assert!(Version::parse("0.2").is_none());
        assert!(Version::parse("latest").is_none());
    }

    #[test]
    fn beta_takes_anything_newer_stable_steps_down_from_a_beta_but_never_to_electron() {
        use Channel::*;
        assert_eq!(decide(Beta, &v("0.2.0-beta.1"), &v("0.2.0-beta.2")), Ok(true));
        assert_eq!(decide(Beta, &v("0.2.0-beta.2"), &v("0.2.0")), Ok(true)); // the stable release after the betas
        assert_eq!(decide(Beta, &v("0.2.0-beta.2"), &v("0.2.0-beta.1")), Err(None));
        assert_eq!(decide(Stable, &v("0.2.0"), &v("0.2.1")), Ok(true));
        assert_eq!(decide(Stable, &v("0.2.1"), &v("0.2.1")), Err(None));
        assert_eq!(decide(Stable, &v("0.2.0-beta.3"), &v("0.2.0-beta.1")), Ok(true)); // switched back to stable
                                                                                      // The stable release is still the Electron app: not taken, and said why.
        let electron = decide(Stable, &v("0.2.0-beta.1"), &v("0.1.15"));
        assert!(electron.unwrap_err().unwrap().contains("by hand"));
        assert_eq!(decide(Beta, &v("0.2.0-beta.1"), &v("0.1.15")), Err(None)); // simply older
    }

    #[test]
    fn reads_electron_builders_feed_file() {
        let feed = "version: 0.1.14-beta.12\nfiles:\n  - url: Falcon-Scale-Station-Setup-0.1.14-beta.12.exe\n    sha512: RQ+oLPWo==\n    size: 259056705\npath: Falcon-Scale-Station-Setup-0.1.14-beta.12.exe\nsha512: RQ+oLPWo==\nreleaseDate: '2026-10-13T11:50:30.763Z'\n";
        let r = parse_feed(feed).unwrap();
        assert_eq!(r.version, v("0.1.14-beta.12"));
        assert_eq!(r.file, "Falcon-Scale-Station-Setup-0.1.14-beta.12.exe");
        assert_eq!(r.sha512, "RQ+oLPWo==");
        assert_eq!(r.size, Some(259_056_705));
        assert_eq!(r.url(), "https://github.com/Joepowles/fgr-scale-station/releases/download/v0.1.14-beta.12/Falcon-Scale-Station-Setup-0.1.14-beta.12.exe");
        assert!(parse_feed("files: []\n").is_err());
    }

    #[test]
    fn beta_follows_the_newest_stable_or_beta_release_in_the_feed() {
        let atom = r#"<feed><entry><link rel="alternate" type="text/html" href="https://github.com/Joepowles/fgr-scale-station/releases/tag/v0.3.0-alpha.1"/></entry>
            <entry><link rel="alternate" type="text/html" href="https://github.com/Joepowles/fgr-scale-station/releases/tag/v0.2.0-beta.2"/></entry>
            <entry><link rel="alternate" type="text/html" href="https://github.com/Joepowles/fgr-scale-station/releases/tag/v0.1.15"/></entry></feed>"#;
        assert_eq!(newest_beta_tag(atom).as_deref(), Some("v0.2.0-beta.2"));
        let stable_first = atom.replace("v0.3.0-alpha.1", "v0.2.0");
        assert_eq!(newest_beta_tag(&stable_first).as_deref(), Some("v0.2.0"));
        assert_eq!(newest_beta_tag("<feed></feed>"), None);
    }

    // Against the real release feed, so only on request:
    //   cargo test real_feed -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn real_feed_offers_a_release_on_each_channel() {
        let client = Updater::client().unwrap();
        for channel in [Channel::Stable, Channel::Beta] {
            let release = offered(&client, channel).await.unwrap();
            println!("{channel:?}: {release:?}");
            assert!(release.size.is_some());
        }
    }

    /// A server for the installer that honours ranges, and that can be told
    /// to drop the connection partway, as the yard's link does.
    async fn serve(body: Vec<u8>, drop_after: Option<usize>) -> (String, Arc<Mutex<Vec<Option<String>>>>) {
        use axum::http::{HeaderMap, StatusCode};
        use axum::response::IntoResponse;
        let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
        let (body, ranges) = (Arc::new(body), seen.clone());
        let drops = Arc::new(Mutex::new(drop_after));
        let handler = move |headers: HeaderMap| {
            let (body, ranges, drops) = (body.clone(), ranges.clone(), drops.clone());
            async move {
                let range = headers.get("range").and_then(|r| r.to_str().ok()).map(String::from);
                ranges.lock().unwrap().push(range.clone());
                let from: usize = range.as_deref().and_then(|r| r.strip_prefix("bytes=")?.trim_end_matches('-').parse().ok()).unwrap_or(0);
                let rest = body[from..].to_vec();
                let status = if from > 0 { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK };
                // The first answer stops short and errors, like a dropped link.
                if let Some(n) = drops.lock().unwrap().take() {
                    let head = rest[..n].to_vec();
                    // The bytes go out, then the link goes a moment later.
                    let stream = futures_util::stream::unfold(Some(head), |head| async move {
                        match head {
                            Some(head) => Some((Ok(bytes::Bytes::from(head)), None)),
                            None => {
                                tokio::time::sleep(Duration::from_millis(200)).await;
                                Some((Err(std::io::Error::other("link dropped")), None))
                            }
                        }
                    });
                    return (status, [("content-length", rest.len().to_string())], axum::body::Body::from_stream(stream)).into_response();
                }
                (status, rest).into_response()
            }
        };
        let app = axum::Router::new().route("/installer.exe", axum::routing::get(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/installer.exe", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, seen)
    }

    fn temp_target() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("station-update-{}", fastrand::u32(..)));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("Falcon-Scale-Station-Setup-0.2.0.exe")
    }

    fn checksum(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes))
    }

    #[tokio::test]
    async fn a_dropped_download_carries_on_from_where_it_stopped() {
        let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let (url, ranges) = serve(body.clone(), Some(100_000)).await;
        let target = temp_target();
        let client = reqwest::Client::new();
        let first = download(&client, &url, &target, &checksum(&body), Some(body.len() as u64), |_| {}).await;
        let err = first.unwrap_err();
        assert!(err.contains("carries on"), "{err}");
        assert_eq!(std::fs::metadata(target.with_extension("part")).unwrap().len(), 100_000);
        let percents = Mutex::new(Vec::new());
        download(&client, &url, &target, &checksum(&body), Some(body.len() as u64), |p| percents.lock().unwrap().push(p)).await.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), body);
        assert!(!target.with_extension("part").exists());
        assert_eq!(*ranges.lock().unwrap(), [None, Some("bytes=100000-".to_string())]);
        assert_eq!(percents.lock().unwrap().last(), Some(&100));
        // Already here and whole: nothing is fetched.
        download(&client, &url, &target, &checksum(&body), None, |_| {}).await.unwrap();
        assert_eq!(ranges.lock().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(target.parent().unwrap());
    }

    #[tokio::test]
    async fn an_installer_that_does_not_match_the_checksum_is_never_kept() {
        let body = b"not what the feed promised".to_vec();
        let (url, _) = serve(body, None).await;
        let target = temp_target();
        let err = download(&reqwest::Client::new(), &url, &target, &checksum(b"the real installer"), None, |_| {}).await.unwrap_err();
        assert!(err.contains("does not match"));
        assert!(!target.exists());
        assert!(!target.with_extension("part").exists());
        let _ = std::fs::remove_dir_all(target.parent().unwrap());
    }
}
