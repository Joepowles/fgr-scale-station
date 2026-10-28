//! Sample pictures from the yard, for working on telling trucks apart by
//! how they look (no plate needed) with real frames rather than guesses.
//!
//! Off unless "Send sample pictures" is ticked on the Bug report tab. Then,
//! for each visit to the deck (from a truck rolling on until the deck reads
//! clear, so trucks that pull on nose to tail share one), its own connection
//! to the full-resolution stream writes a frame every couple of seconds,
//! shrunk to 1280 wide. When the deck clears, the frames, the log over the
//! visit (with the scale readings and each truck's number, plate and weight)
//! and a short visit.json are packed as one .tar under samples/pending in the
//! app's data folder. A background task uploads the packs to the relay one at
//! a time and deletes each once it is up; the relay keeps them until they are
//! fetched on the dev machine (relay/fetch-samples.mjs). A pack that cannot
//! go up yet waits on disk, so a yard connection that drops loses nothing.
//!
//! Nothing here touches weighing: the pipeline neither waits on it nor sees it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde_json::json;
use tauri::{AppHandle, Manager};

use crate::camera::{burst::Burst, CameraSettings};
use crate::{log, AppState};

/// A frame this often at first, and every LATER_EVERY after FIRST_FRAMES:
/// the coming on and pulling off are what matter, not ten minutes parked.
const EVERY: Duration = Duration::from_secs(2);
const FIRST_FRAMES: u32 = 60;
const LATER_EVERY: Duration = Duration::from_secs(10);
const MAX_FRAMES: u32 = 120;
const FRAME_WIDTH: u32 = 1280;
const FRAME_QUALITY: u8 = 80;
/// The log from this long before the truck came on, for the readings ahead of it.
const LOG_BEFORE: chrono::TimeDelta = chrono::TimeDelta::seconds(30);
/// Recording stops while this much waits to go up: the connection is down.
const PENDING_MAX_BYTES: u64 = 1024 * 1024 * 1024;
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const UPLOAD_PAUSE: Duration = Duration::from_secs(20);
const UPLOAD_BACKOFF: Duration = Duration::from_secs(120);
/// The relay has closed uploads: ask again this much later.
const CLOSED_BACKOFF: Duration = Duration::from_secs(3600);

/// The visit being recorded, if any.
pub struct Samples {
    dir: PathBuf,
    visit: Mutex<Option<Visit>>,
    visits: std::sync::atomic::AtomicU64,
}

struct Visit {
    id: String,
    folder: PathBuf,
    started: DateTime<Utc>,
    task: tauri::async_runtime::JoinHandle<u32>,
}

impl Samples {
    pub fn new(data_dir: &Path) -> Self {
        Self { dir: data_dir.join("samples"), visit: Mutex::new(None), visits: Default::default() }
    }

    fn pending(&self) -> PathBuf {
        self.dir.join("pending")
    }

    fn recording(&self) -> PathBuf {
        self.dir.join("recording")
    }
}

fn enabled(app: &AppHandle) -> bool {
    app.state::<AppState>().settings.lock().unwrap().get()["support"]["sendSamples"] == true
}

/// Something came onto the deck: start recording, unless already.
pub fn on_arrived(app: &AppHandle) {
    if !enabled(app) {
        return;
    }
    let state = app.state::<AppState>();
    let samples = &state.samples;
    let mut slot = samples.visit.lock().unwrap();
    if slot.is_some() {
        return;
    }
    if folder_bytes(&samples.pending()) > PENDING_MAX_BYTES {
        log(app, "samples: not recording this truck; over 1 GB is still waiting to upload");
        return;
    }
    let camera: CameraSettings = serde_json::from_value(state.settings.lock().unwrap().get()["camera"].clone()).unwrap_or_default();
    let Some(url) = camera.url(true) else { return };
    let n = samples.visits.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let id = format!("{}-v{n}", Local::now().format("%Y%m%d-%H%M%S"));
    let folder = samples.recording().join(&id);
    if let Err(err) = fs::create_dir_all(&folder) {
        log(app, format!("samples: could not make {}: {err}", folder.display()));
        return;
    }
    log(app, format!("samples: recording visit {id}"));
    let (app2, folder2) = (app.clone(), folder.clone());
    let task = tauri::async_runtime::spawn(async move { record(&app2, &url, &folder2).await });
    *slot = Some(Visit { id, folder, started: Utc::now(), task });
}

/// The deck is clear: pack the visit up for upload.
pub fn on_cleared(app: &AppHandle) {
    let Some(visit) = app.state::<AppState>().samples.visit.lock().unwrap().take() else { return };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        visit.task.abort();
        let frames = visit.task.await.unwrap_or_else(|_| count_frames(&visit.folder));
        let log_text = log_since(&app.state::<AppState>().log_file(), visit.started - LOG_BEFORE);
        let info = json!({
            "station": station(),
            "version": app.package_info().version.to_string(),
            "started": visit.started.to_rfc3339_opts(SecondsFormat::Secs, true),
            "ended": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            "frames": frames,
            "frameEverySeconds": EVERY.as_secs(),
            "scale": app.state::<AppState>().settings.lock().unwrap().get()["scale"].clone(),
        });
        let _ = fs::write(visit.folder.join("log.txt"), log_text);
        let _ = fs::write(visit.folder.join("visit.json"), serde_json::to_vec_pretty(&info).unwrap_or_default());
        let pending = app.state::<AppState>().samples.pending();
        match pack(&visit.folder, &pending, &visit.id) {
            Ok(bytes) => log(&app, format!("samples: visit {} packed, {frames} frames, {} KB, waiting to upload", visit.id, bytes / 1024)),
            Err(err) => log(&app, format!("samples: could not pack visit {}: {err}", visit.id)),
        }
    });
}

/// Frames until stopped; how many were kept.
async fn record(app: &AppHandle, url: &str, folder: &Path) -> u32 {
    let mut burst = match Burst::start(url, 1.0 / EVERY.as_secs_f64()) {
        Ok(b) => b,
        Err(err) => {
            log(app, format!("samples: no feed from the camera: {err}"));
            return 0;
        }
    };
    let mut kept = 0u32;
    let mut last = tokio::time::Instant::now() - LATER_EVERY;
    while kept < MAX_FRAMES {
        let Some((jpeg, at)) = burst.next(Duration::from_secs(15)).await else {
            log(app, "samples: the camera feed stopped; no more frames for this visit");
            break;
        };
        if kept >= FIRST_FRAMES && at.duration_since(last) < LATER_EVERY {
            continue;
        }
        last = at;
        let name = format!("frame-{:03}-{}.jpg", kept + 1, Local::now().format("%H%M%S%.3f"));
        let small = tokio::task::spawn_blocking(move || shrink(&jpeg, FRAME_WIDTH, FRAME_QUALITY).unwrap_or(jpeg)).await;
        if let Ok(small) = small {
            if fs::write(folder.join(name), small).is_ok() {
                kept += 1;
            }
        }
    }
    kept
}

/// The JPEG at most `width` wide, re-encoded at `quality`.
pub fn shrink(jpeg: &[u8], width: u32, quality: u8) -> Result<Vec<u8>, String> {
    let image = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).map_err(|e| e.to_string())?;
    let image = if image.width() > width { image.resize(width, u32::MAX, image::imageops::FilterType::Triangle) } else { image };
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).encode_image(&image.to_rgb8()).map_err(|e| e.to_string())?;
    Ok(out)
}

fn count_frames(folder: &Path) -> u32 {
    fs::read_dir(folder).map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".jpg")).count() as u32).unwrap_or(0)
}

fn folder_bytes(folder: &Path) -> u64 {
    fs::read_dir(folder).map(|d| d.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum()).unwrap_or(0)
}

/// The log file's lines stamped at or after `from`.
pub fn log_since(file: &Path, from: DateTime<Utc>) -> String {
    let Ok(text) = fs::read_to_string(file) else { return String::new() };
    let from = from.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    // Stamps are fixed width and UTC, so they sort as text.
    let start = text.lines().position(|l| l.get(..24).is_some_and(|stamp| stamp >= from.as_str())).unwrap_or(usize::MAX);
    text.lines().skip(start).flat_map(|l| [l, "\n"]).collect()
}

/// This PC's name, fit for a file name on the relay.
pub fn station() -> String {
    let name = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default();
    let clean: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).take(40).collect();
    if clean.is_empty() {
        "station".into()
    } else {
        clean
    }
}

/// The folder's files as `<pending>/<id>.tar`, then the folder removed.
fn pack(folder: &Path, pending: &Path, id: &str) -> Result<u64, String> {
    fs::create_dir_all(pending).map_err(|e| e.to_string())?;
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for entry in fs::read_dir(folder).map_err(|e| e.to_string())?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        files.push((name, fs::read(entry.path()).map_err(|e| e.to_string())?));
    }
    files.sort();
    let tar = tar(&files, Utc::now().timestamp().max(0) as u64);
    let partial = pending.join(format!("{id}.tar.part"));
    fs::write(&partial, &tar).map_err(|e| e.to_string())?;
    fs::rename(&partial, pending.join(format!("{id}.tar"))).map_err(|e| e.to_string())?;
    let _ = fs::remove_dir_all(folder);
    Ok(tar.len() as u64)
}

/// A plain ustar archive of `files`, which tar on any system unpacks.
pub fn tar(files: &[(String, Vec<u8>)], mtime: u64) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, data) in files {
        let mut header = [0u8; 512];
        let field = |header: &mut [u8; 512], at: usize, value: &[u8]| header[at..at + value.len()].copy_from_slice(value);
        field(&mut header, 0, &name.as_bytes()[..name.len().min(99)]);
        field(&mut header, 100, b"0000644\0");
        field(&mut header, 108, b"0000000\0");
        field(&mut header, 116, b"0000000\0");
        field(&mut header, 124, format!("{:011o}\0", data.len()).as_bytes());
        field(&mut header, 136, format!("{:011o}\0", mtime).as_bytes());
        field(&mut header, 148, b"        ");
        header[156] = b'0';
        field(&mut header, 257, b"ustar\0");
        field(&mut header, 263, b"00");
        let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        field(&mut header, 148, format!("{sum:06o}\0 ").as_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.resize(out.len() + 1024, 0);
    out
}

/// Packs anything a closed or crashed app left mid-recording, then uploads
/// whatever is waiting, for as long as the app runs.
pub fn start_uploader(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let (recording, pending) = {
            let s = &app.state::<AppState>().samples;
            (s.recording(), s.pending())
        };
        for entry in fs::read_dir(&recording).into_iter().flatten().flatten() {
            let id = entry.file_name().to_string_lossy().to_string();
            let _ = pack(&entry.path(), &pending, &format!("{id}-unfinished"));
        }
        let client = match reqwest::Client::builder().timeout(UPLOAD_TIMEOUT).user_agent("fgr-scale-station").build() {
            Ok(c) => c,
            Err(err) => return log(&app, format!("samples: no uploads: {err}")),
        };
        let mut said_closed = false;
        loop {
            let next = oldest_pack(&pending);
            let pause = match next {
                None => UPLOAD_PAUSE,
                Some(file) => match upload(&app, &client, &file).await {
                    Upload::Done(kb) => {
                        let _ = fs::remove_file(&file);
                        said_closed = false;
                        log(&app, format!("samples: uploaded {} ({kb} KB)", file.file_name().unwrap_or_default().to_string_lossy()));
                        Duration::from_secs(1)
                    }
                    Upload::Refused(why) => {
                        log(&app, format!("samples: the relay refused {}, so it is deleted: {why}", file.display()));
                        let _ = fs::remove_file(&file);
                        Duration::from_secs(1)
                    }
                    Upload::Closed => {
                        if !said_closed {
                            log(&app, "samples: the relay is not taking samples now; they wait here and it is asked again in an hour");
                            said_closed = true;
                        }
                        CLOSED_BACKOFF
                    }
                    Upload::Failed(why) => {
                        log(&app, format!("samples: upload failed, trying again in 2 minutes: {why}"));
                        UPLOAD_BACKOFF
                    }
                },
            };
            tokio::time::sleep(pause).await;
        }
    });
}

fn oldest_pack(pending: &Path) -> Option<PathBuf> {
    let mut packs: Vec<PathBuf> = fs::read_dir(pending).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "tar")).collect();
    packs.sort();
    packs.into_iter().next()
}

enum Upload {
    Done(u64),
    /// Never going up: too large, or not a name the relay takes.
    Refused(String),
    Closed,
    Failed(String),
}

async fn upload(app: &AppHandle, client: &reqwest::Client, file: &Path) -> Upload {
    let relay = {
        let s = app.state::<AppState>();
        let settings = s.settings.lock().unwrap();
        let chosen = settings.get()["support"]["relayUrl"].as_str().unwrap_or("").trim().to_string();
        if chosen.is_empty() {
            crate::settings::defaults()["support"]["relayUrl"].as_str().unwrap_or("").to_string()
        } else {
            chosen
        }
    };
    let Ok(body) = fs::read(file) else { return Upload::Failed(format!("could not read {}", file.display())) };
    let size = body.len() as u64;
    let name = file.file_name().unwrap_or_default().to_string_lossy().to_string();
    let url = format!("{}/samples/{}/{name}", relay.trim_end_matches('/'), station());
    match client.put(&url).header("Content-Type", "application/x-tar").body(body).send().await {
        Ok(r) if r.status().is_success() => Upload::Done(size / 1024),
        Ok(r) if r.status().as_u16() == 410 || r.status().as_u16() == 503 => Upload::Closed,
        Ok(r) if r.status().as_u16() == 400 || r.status().as_u16() == 413 => Upload::Refused(r.text().await.unwrap_or_default()),
        Ok(r) => Upload::Failed(format!("the relay answered {}", r.status())),
        Err(err) => Upload::Failed(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_tar_that_tar_unpacks() {
        let files = vec![("frame-001.jpg".to_string(), vec![7u8; 700]), ("log.txt".to_string(), b"hello\n".to_vec())];
        let archive = tar(&files, 1_700_000_000);
        assert_eq!(archive.len() % 512, 0);
        assert_eq!(&archive[257..262], b"ustar");
        // The checksum is the header's bytes summed with the field as spaces.
        let mut header = archive[..512].to_vec();
        let stored = u32::from_str_radix(std::str::from_utf8(&header[148..154]).unwrap(), 8).unwrap();
        header[148..156].copy_from_slice(b"        ");
        assert_eq!(stored, header.iter().map(|&b| u32::from(b)).sum::<u32>());
        // The second file starts after the first's data, padded to 512.
        assert_eq!(&archive[512 + 1024..512 + 1024 + 7], b"log.txt");
        assert_eq!(&archive[512 + 1024 + 512..512 + 1024 + 512 + 6], b"hello\n");

        let dir = std::env::temp_dir().join(format!("samples-tar-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.tar"), &archive).unwrap();
        if let Ok(status) = std::process::Command::new("tar").arg("-xf").arg("a.tar").current_dir(&dir).status() {
            assert!(status.success());
            assert_eq!(fs::read(dir.join("frame-001.jpg")).unwrap(), vec![7u8; 700]);
            assert_eq!(fs::read_to_string(dir.join("log.txt")).unwrap(), "hello\n");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn packs_a_visit_folder_and_removes_it() {
        let root = std::env::temp_dir().join(format!("samples-pack-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = root.join("recording").join("v1");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("frame-001.jpg"), [1, 2, 3]).unwrap();
        let bytes = pack(&folder, &root.join("pending"), "20261029-101500-v1").unwrap();
        assert_eq!(bytes, 512 + 512 + 1024);
        assert!(!folder.exists());
        assert_eq!(oldest_pack(&root.join("pending")), Some(root.join("pending").join("20261029-101500-v1.tar")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn takes_the_log_from_a_moment_on() {
        let file = std::env::temp_dir().join(format!("samples-log-{}.log", std::process::id()));
        fs::write(&file, "2026-10-29T10:14:00.000Z before\n2026-10-29T10:15:00.000Z coming on\n2026-10-29T10:16:00.000Z deck clear\n").unwrap();
        let from = DateTime::parse_from_rfc3339("2026-10-29T10:14:30Z").unwrap().with_timezone(&Utc);
        assert_eq!(log_since(&file, from), "2026-10-29T10:15:00.000Z coming on\n2026-10-29T10:16:00.000Z deck clear\n");
        let later = DateTime::parse_from_rfc3339("2026-10-29T11:00:00Z").unwrap().with_timezone(&Utc);
        assert_eq!(log_since(&file, later), "");
        let _ = fs::remove_file(&file);
    }

    #[test]
    fn shrinks_a_big_frame_to_1280_wide() {
        let big = image::RgbImage::from_pixel(2560, 1440, image::Rgb([90, 120, 30]));
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90).encode_image(&big).unwrap();
        let small = shrink(&jpeg, 1280, 80).unwrap();
        assert_eq!(crate::camera::snapshot::jpeg_size(&small), Some((1280, 720)));
        let tiny = image::RgbImage::from_pixel(640, 360, image::Rgb([1, 2, 3]));
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90).encode_image(&tiny).unwrap();
        assert_eq!(crate::camera::snapshot::jpeg_size(&shrink(&jpeg, 1280, 80).unwrap()), Some((640, 360)));
    }

    #[test]
    fn names_the_station_safely() {
        let s = station();
        assert!(!s.is_empty() && s.len() <= 40 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }
}
