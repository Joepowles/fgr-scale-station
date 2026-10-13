//! RTSP -> fragmented MP4 for the window's <video> element.
//!
//! The cameras emit HEVC, and HEVC in a <video> element is patchy, so ffmpeg
//! transcodes to H.264 (about a third of a core per camera while someone is
//! watching). Measured on the Electron side against the Front camera: at full
//! framerate this uses less bandwidth than the 5 fps MJPEG it replaced while
//! being about four times smoother. If the cameras are ever set to emit H.264
//! substreams, this should switch to '-c copy' and the encode cost goes.
//!
//! One ffmpeg per camera, shared by every viewer: cost scales with cameras,
//! not cameras x viewers. Keyed by the full URL including the login, so two
//! views of one stream share and different logins do not.
//!
//! fMP4 is not self-synchronising the way MJPEG is: a viewer who joins mid
//! stream and simply starts receiving bytes gets garbage. Every viewer needs
//! the initialisation segment (ftyp + moov) first, then fragments starting at
//! a moof boundary. So ffmpeg's output is cut into top-level boxes: everything
//! up to the moov is kept as the init segment and replayed to each new viewer,
//! and whole fragments (a moof and its mdat) after it are broadcast live.
//!
//! Each frame is its own fragment, so it goes out as soon as it is encoded:
//! fragments cut only at keyframes held every frame back until the next one,
//! two seconds of delay, always (issue #9). Most fragments then cannot be
//! decoded alone, so a new viewer waits for the next one that opens on a
//! keyframe and starts there.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures_util::Stream;
use tauri::async_runtime::JoinHandle;
use tokio::io::AsyncReadExt;
use tokio::sync::{broadcast, watch, Notify};
use tokio::time::{sleep, Instant};

use super::{ffmpeg, rtsp};

const MAX_RESTARTS: u32 = 50;
const RESTART_DELAY: Duration = Duration::from_secs(2);
// Kept alive briefly after the last viewer leaves so a page refresh does not
// pay the RTSP reconnect and a fresh init segment.
const LINGER: Duration = Duration::from_secs(5);
// A camera that accepts the connection but never produces a decodable stream
// would otherwise leave the viewer waiting on a blank player forever.
const INIT_TIMEOUT: Duration = Duration::from_secs(15);
// Boxes held for a viewer that has fallen behind. Past this it is dropped
// and its player reconnects, rather than being sent a stream with holes.
const BACKLOG: usize = 256;
// ffmpeg falling further behind the camera than this is reported.
const MAX_ENCODER_LAG_S: f64 = 5.0;
// ffmpeg can repeat a complaint many times a second; the log gets it once a
// minute, with how many were held back.
const WARN_EVERY: Duration = Duration::from_secs(60);
// What ffmpeg says on connecting goes in the log in full, up to this many lines.
const FULL_LINES_PER_CONNECTION: usize = 40;

pub type Log = Arc<dyn Fn(String) + Send + Sync>;

/// An ffmpeg filter passing nothing before the input's first keyframe, and
/// everything from it on.
pub const FROM_FIRST_KEYFRAME: &str = "select='key+not(isnan(prev_selected_t))'";

/// Cuts a byte stream into whole MP4 boxes, however the chunks fall.
#[derive(Default)]
pub struct BoxSplitter {
    buffer: BytesMut,
}

#[derive(Debug, PartialEq)]
pub struct Mp4Box {
    pub kind: [u8; 4],
    pub bytes: Bytes,
}

impl BoxSplitter {
    pub fn feed(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    /// The next whole box, None until one has fully arrived, or an error for
    /// a size this cannot follow. Size 0 means "to end of file" and size 1
    /// means a 64-bit length follows; neither is produced by the fragmenting
    /// muxer, and guessing would desynchronise the stream.
    pub fn next_box(&mut self) -> Result<Option<Mp4Box>, String> {
        if self.buffer.len() < 8 {
            return Ok(None);
        }
        let size = u32::from_be_bytes(self.buffer[..4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = self.buffer[4..8].try_into().unwrap();
        if size < 8 {
            return Err(format!("unparseable box '{}' (size {size})", String::from_utf8_lossy(&kind)));
        }
        if self.buffer.len() < size {
            return Ok(None);
        }
        Ok(Some(Mp4Box { kind, bytes: self.buffer.split_to(size).freeze() }))
    }
}

/// One frame's moof and mdat, and whether a player can start on it.
#[derive(Clone)]
struct Fragment {
    bytes: Bytes,
    keyframe: bool,
}

/// What a viewer joins: the init segment, and the fragments after it.
#[derive(Clone)]
struct Feed {
    init: Bytes,
    fragments: broadcast::Sender<Fragment>,
}

struct Camera {
    /// None until ffmpeg has produced an init segment, and again while it
    /// restarts: a new run means a new init segment, and fragments from it
    /// are useless to a viewer holding the old one.
    feed: watch::Sender<Option<Feed>>,
    viewers: AtomicUsize,
    task: Mutex<Option<JoinHandle<()>>>,
    /// The Reconnect button: a fresh connection to the camera.
    reconnect: Notify,
}

/// Every camera being streamed now.
#[derive(Clone)]
pub struct Streams {
    cameras: Arc<Mutex<HashMap<String, Arc<Camera>>>>,
    log: Log,
}

impl Streams {
    pub fn new(log: Log) -> Self {
        Self { cameras: Arc::default(), log }
    }

    /// The fragmented MP4 of `url` for one viewer, from the init segment on.
    /// Ends when the viewer falls behind, or ffmpeg restarts or gives up;
    /// the window's player reconnects. Dropping it is the viewer leaving.
    pub fn watch(&self, url: &str) -> impl Stream<Item = Result<Bytes, Infallible>> + Send + 'static {
        let camera = self.camera(url);
        let start = Watching::Waiting { feed: camera.feed.subscribe(), viewer: Viewer::new(camera) };
        futures_util::stream::unfold(start, |state| async move {
            match state {
                Watching::Waiting { viewer, mut feed } => {
                    // The init segment and the next keyframe fragment, sent as one.
                    let start = async {
                        let current = feed.wait_for(Option::is_some).await.ok()?.clone()?;
                        let mut fragments = current.fragments.subscribe();
                        loop {
                            let fragment = fragments.recv().await.ok()?;
                            if fragment.keyframe {
                                return Some(([current.init, fragment.bytes].concat().into(), fragments));
                            }
                        }
                    };
                    let (first, fragments): (Bytes, _) = tokio::time::timeout(INIT_TIMEOUT, start).await.ok()??;
                    Some((Ok(first), Watching::Live { viewer, fragments }))
                }
                Watching::Live { viewer, mut fragments } => {
                    // An error is falling behind, or this run of ffmpeg being over.
                    let fragment = fragments.recv().await.ok()?;
                    Some((Ok(fragment.bytes), Watching::Live { viewer, fragments }))
                }
            }
        })
    }

    /// The camera for `url`, starting its ffmpeg if nobody is watching it yet.
    fn camera(&self, url: &str) -> Arc<Camera> {
        let mut cameras = self.cameras.lock().unwrap();
        if let Some(camera) = cameras.get(url) {
            return camera.clone();
        }
        let camera = Arc::new(Camera { feed: watch::channel(None).0, viewers: AtomicUsize::new(0), task: Mutex::new(None), reconnect: Notify::new() });
        let task = tauri::async_runtime::spawn(supervise(camera.clone(), url.to_string(), self.cameras.clone(), self.log.clone()));
        *camera.task.lock().unwrap() = Some(task);
        cameras.insert(url.to_string(), camera.clone());
        camera
    }

    /// Stops every stream now: the settings changed, or the app is closing.
    pub fn stop_all(&self) {
        for (_, camera) in self.cameras.lock().unwrap().drain() {
            camera.stop();
        }
    }

    /// The Reconnect button: every stream connects to its camera afresh, for
    /// a picture that came up gray. Viewers are cut off and their players
    /// come back.
    pub fn reconnect(&self) {
        for camera in self.cameras.lock().unwrap().values() {
            (self.log)("camera stream: reconnecting, asked from the window".into());
            camera.reconnect.notify_one();
        }
    }

    /// How many cameras are being streamed.
    #[cfg(test)]
    pub fn active(&self) -> usize {
        self.cameras.lock().unwrap().len()
    }
}

impl Camera {
    fn stop(&self) {
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort(); // drops ffmpeg, which kills it
        }
        self.feed.send_replace(None);
    }
}

/// Where one viewer's stream is up to.
enum Watching {
    Waiting { viewer: Viewer, feed: watch::Receiver<Option<Feed>> },
    Live { viewer: Viewer, fragments: broadcast::Receiver<Fragment> },
}

/// Counts a viewer in, and out again when its stream is dropped.
struct Viewer(Arc<Camera>);

impl Viewer {
    fn new(camera: Arc<Camera>) -> Self {
        camera.viewers.fetch_add(1, Ordering::SeqCst);
        Self(camera)
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.0.viewers.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Runs ffmpeg for one camera for as long as someone watches: restarted
/// when it exits, given up after MAX_RESTARTS, stopped LINGER after the
/// last viewer leaves.
async fn supervise(camera: Arc<Camera>, url: String, cameras: Arc<Mutex<HashMap<String, Arc<Camera>>>>, log: Log) {
    let mut restarts = 0;
    loop {
        let outcome = run_ffmpeg(&camera, &url, &log).await;
        camera.feed.send_replace(None);
        match outcome {
            Ended::Unwatched => break,
            Ended::Reconnect => sleep(RESTART_DELAY).await,
            Ended::Exited(why) => {
                restarts += 1;
                if restarts > MAX_RESTARTS {
                    log(format!("camera stream: giving up after {MAX_RESTARTS} restarts"));
                    break;
                }
                log(format!("camera stream: {why}; restart {restarts}/{MAX_RESTARTS}"));
                sleep(RESTART_DELAY).await;
            }
        }
    }
    let mut map = cameras.lock().unwrap();
    if map.get(&url).is_some_and(|c| Arc::ptr_eq(c, &camera)) {
        map.remove(&url);
    }
}

enum Ended {
    Unwatched,
    Reconnect,
    Exited(String),
}

async fn run_ffmpeg(camera: &Camera, url: &str, log: &Log) -> Ended {
    let bitrate = std::env::var("CAMERA_STREAM_BITRATE").unwrap_or_else(|_| "1500k".into());
    let fps = std::env::var("CAMERA_STREAM_FPS").unwrap_or_else(|_| "15".into());
    // One flag and its value per line.
    #[rustfmt::skip]
    let args = [
        "-rtsp_transport", "tcp",
        "-fflags", "nobuffer",
        "-i", url,
        "-an",
        "-c:v", "libx264",
        "-preset", "ultrafast",
        "-tune", "zerolatency",
        "-b:v", &bitrate,
        // Forces a keyframe every two seconds at 15 fps. A new viewer starts
        // at a keyframe, so this is how long one waits for playable video -
        // the camera's own GOP can be several seconds.
        "-g", "30",
        // A fixed output rate, always. The yard's GeoVision camera does not
        // say its frame rate in a way ffmpeg can always detect; when it
        // cannot, it takes 90k fps, and libx264 shares the bitrate out over
        // 90,000 frames a second: a gray blocky picture that only the moving
        // parts redraw (issue #8). 15 fps is plenty for a scale.
        // Nothing until the camera's first keyframe: joining its stream
        // between keyframes, the frames decoded before one are gray smears
        // of whatever moved, and a new viewer (the HD switch while zoomed)
        // was shown them. Every frame after it passes.
        "-vf", FROM_FIRST_KEYFRAME,
        "-fps_mode", "cfr",
        "-r", &fps,
        "-f", "mp4",
        // One fragment per frame; see the top of this file.
        "-movflags", "frag_every_frame+empty_moov+default_base_moof",
        "pipe:1",
    ];

    log("camera stream: connecting to the camera".into());
    let mut child = match ffmpeg::command(&args).and_then(|mut c| c.spawn()) {
        Ok(child) => child,
        Err(err) => return Ended::Exited(err.to_string()),
    };
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");

    // Everything ffmpeg says about the connection goes in the log: what the
    // camera sends, trouble starting, "RTP: missed packets" - the picture
    // sometimes comes up gray (issues #6, #7) and the log has to say why.
    // Progress lines end in \r, not \n.
    let log_stderr = log.clone();
    tauri::async_runtime::spawn(async move {
        let mut said = StderrLog::default();
        let mut text = String::new();
        let mut chunk = vec![0u8; 8 * 1024];
        while let Ok(n) = stderr.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            text.push_str(&String::from_utf8_lossy(&chunk[..n]));
            while let Some(end) = text.find(['\r', '\n']) {
                let line: String = text.drain(..=end).collect();
                for message in said.line(&line, unix_seconds(), Instant::now()) {
                    log_stderr(format!("camera stream: {message}"));
                }
            }
        }
    });

    let mut splitter = BoxSplitter::default();
    let mut init = BytesMut::new();
    let mut fragments: Option<broadcast::Sender<Fragment>> = None;
    // A fragment is a moof and the mdat after it, sent as one so a viewer
    // joining between the two never gets media data without its header.
    let mut moof: Option<Bytes> = None;
    let mut chunk = vec![0u8; 64 * 1024];
    let mut unwatched_since: Option<Instant> = None;
    let mut check = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            read = stdout.read(&mut chunk) => {
                let n = match read {
                    Ok(0) | Err(_) => return Ended::Exited(exit_reason(&mut child).await),
                    Ok(n) => n,
                };
                splitter.feed(&chunk[..n]);
                loop {
                    let mp4_box = match splitter.next_box() {
                        Ok(Some(b)) => b,
                        Ok(None) => break,
                        Err(why) => return Ended::Exited(why),
                    };
                    match &fragments {
                        Some(tx) => match (&mp4_box.kind, moof.take()) {
                            (b"moof", _) => moof = Some(mp4_box.bytes),
                            (b"mdat", Some(header)) => {
                                let keyframe = starts_on_keyframe(&header);
                                let _ = tx.send(Fragment { bytes: [header, mp4_box.bytes].concat().into(), keyframe });
                            }
                            // Nothing else comes between fragments; were it to, a
                            // player that has started can take it.
                            _ => { let _ = tx.send(Fragment { bytes: mp4_box.bytes, keyframe: false }); }
                        },
                        None => {
                            init.extend_from_slice(&mp4_box.bytes);
                            // moov completes the init segment.
                            if &mp4_box.kind == b"moov" {
                                let tx = broadcast::channel(BACKLOG).0;
                                camera.feed.send_replace(Some(Feed { init: init.split().freeze(), fragments: tx.clone() }));
                                fragments = Some(tx);
                            }
                        }
                    }
                }
            }
            _ = camera.reconnect.notified() => return Ended::Reconnect, // child dropped: ffmpeg killed
            _ = check.tick() => {
                if camera.viewers.load(Ordering::SeqCst) > 0 {
                    unwatched_since = None;
                } else if unwatched_since.get_or_insert_with(Instant::now).elapsed() >= LINGER {
                    return Ended::Unwatched; // child dropped: ffmpeg killed
                }
            }
        }
    }
}

fn unix_seconds() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// What of ffmpeg's stderr goes in the log, for one connection.
#[derive(Default)]
struct StderrLog {
    lines: usize,
    /// Per kind of message: when one last went in, and how many since did not.
    warned: HashMap<&'static str, (Instant, u32)>,
    lag_baseline: Option<f64>,
}

impl StderrLog {
    /// The messages one line of stderr makes, given the time now.
    fn line(&mut self, raw: &str, unix_now: f64, now: Instant) -> Vec<String> {
        let line = rtsp::redact(raw.trim());
        if line.is_empty() {
            return vec![];
        }
        if line.starts_with("frame=") {
            // Progress: once a minute, and a warning when falling behind.
            let mut out: Vec<String> = self.at_most_every("progress", line.split_whitespace().collect::<Vec<_>>().join(" "), now).into_iter().collect();
            let behind = self.lag(&line, unix_now);
            if behind > MAX_ENCODER_LAG_S {
                out.extend(self.at_most_every("lag", format!("{behind:.1}s behind the camera"), now));
            }
            return out;
        }
        self.lines += 1;
        if self.lines <= FULL_LINES_PER_CONNECTION {
            vec![line]
        } else {
            self.at_most_every("ffmpeg", line, now).into_iter().collect()
        }
    }

    fn at_most_every(&mut self, kind: &'static str, message: String, now: Instant) -> Option<String> {
        match self.warned.get_mut(kind) {
            Some((at, held)) if now.duration_since(*at) < WARN_EVERY => {
                *held += 1;
                None
            }
            _ => {
                let held = self.warned.insert(kind, (now, 0)).map_or(0, |(_, held)| held);
                Some(if held > 0 { format!("{message} (and {held} more like it)") } else { message })
            }
        }
    }

    /// How many seconds ffmpeg has fallen behind since its first progress
    /// line. Its time= is the media time written so far; live, it should
    /// advance with the clock. Wall time minus media time is the delay, and
    /// its growth beyond the first reading (connecting, the first keyframe)
    /// is lag.
    fn lag(&mut self, line: &str, unix_now: f64) -> f64 {
        let Some(time) = line.split("time=").nth(1).map(|t| t.trim_start()) else { return 0.0 };
        let parts: Vec<f64> = time.split_whitespace().next().unwrap_or("").split(':').map_while(|p| p.parse().ok()).collect();
        let [h, m, s] = parts[..] else { return 0.0 };
        let offset = unix_now - (h * 3600.0 + m * 60.0 + s);
        offset - *self.lag_baseline.get_or_insert(offset)
    }
}

/// The child box of `kind` within `buf[start..end]`, as the range of its body.
fn child_box(buf: &[u8], start: usize, end: usize, kind: &[u8; 4]) -> Option<(usize, usize)> {
    let mut at = start;
    while at + 8 <= end {
        let size = u32::from_be_bytes(buf[at..at + 4].try_into().ok()?) as usize;
        if size < 8 || at + size > end {
            return None;
        }
        if &buf[at + 4..at + 8] == kind {
            return Some((at + 8, at + size));
        }
        at += size;
    }
    None
}

/// Whether a moof's first sample is a keyframe. Its flags are, in order of
/// precedence: trun's first_sample_flags, trun's first per-sample flags,
/// tfhd's default_sample_flags. The non-sync bit (0x10000) clear means a
/// keyframe; ffmpeg marks its keyframes in first_sample_flags. A moof that
/// cannot be read counts as a keyframe, so a viewer is never kept waiting by
/// a fragment this does not understand.
pub fn starts_on_keyframe(moof: &[u8]) -> bool {
    let u32_at = |at: usize| moof.get(at..at + 4).map(|b| u32::from_be_bytes(b.try_into().unwrap()));
    let read = || -> Option<bool> {
        let (traf_start, traf_end) = child_box(moof, 8, moof.len(), b"traf")?;
        let (trun, _) = child_box(moof, traf_start, traf_end, b"trun")?;
        let trun_flags = u32_at(trun)? & 0xff_ffff;
        let first = trun + 8 + if trun_flags & 0x1 != 0 { 4 } else { 0 };
        let flags = if trun_flags & 0x4 != 0 {
            u32_at(first)?
        } else if trun_flags & 0x400 != 0 {
            u32_at(first + if trun_flags & 0x100 != 0 { 4 } else { 0 } + if trun_flags & 0x200 != 0 { 4 } else { 0 })?
        } else {
            let tfhd = child_box(moof, traf_start, traf_end, b"tfhd").map(|(s, _)| s);
            let tfhd_flags = tfhd.and_then(u32_at).map_or(0, |f| f & 0xff_ffff);
            if tfhd_flags & 0x20 == 0 {
                return None;
            }
            let skip = |bit: u32, n: usize| if tfhd_flags & bit != 0 { n } else { 0 };
            u32_at(tfhd? + 8 + skip(0x1, 8) + skip(0x2, 4) + skip(0x8, 4) + skip(0x10, 4))?
        };
        Some(flags & 0x10000 == 0)
    };
    read().unwrap_or(true)
}

async fn exit_reason(child: &mut tokio::process::Child) -> String {
    match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
        Ok(Ok(status)) => format!("ffmpeg exited ({status})"),
        _ => "ffmpeg stopped sending".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = ((8 + body.len()) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(body);
        b
    }

    #[test]
    fn cuts_boxes_however_the_bytes_arrive() {
        let stream = [mp4_box(b"ftyp", b"isom"), mp4_box(b"moov", &[7; 100]), mp4_box(b"moof", &[1; 20])].concat();
        let mut s = BoxSplitter::default();
        let mut kinds = Vec::new();
        for chunk in stream.chunks(7) {
            s.feed(chunk);
            while let Some(b) = s.next_box().unwrap() {
                kinds.push(b.kind);
            }
        }
        assert_eq!(kinds, [*b"ftyp", *b"moov", *b"moof"]);
    }

    // A moof as ffmpeg writes one per frame: tfhd with default_sample_flags
    // non-sync, and trun with first_sample_flags only on a keyframe.
    fn frame_moof(key: bool) -> Vec<u8> {
        let full = |kind: &[u8; 4], flags: u32, words: &[u32]| {
            let mut body = flags.to_be_bytes().to_vec();
            words.iter().for_each(|w| body.extend_from_slice(&w.to_be_bytes()));
            mp4_box(kind, &body)
        };
        let tfhd = full(b"tfhd", 0x20038, &[1, 3000, 900, 0x101_0000]);
        let trun = if key { full(b"trun", 0x5, &[1, 120, 0x200_0000]) } else { full(b"trun", 0x1, &[1, 120]) };
        let traf = mp4_box(b"traf", &[tfhd, mp4_box(b"tfdt", &[0; 12]), trun].concat());
        mp4_box(b"moof", &[mp4_box(b"mfhd", &[0; 8]), traf].concat())
    }

    #[test]
    fn tells_a_keyframe_fragment_from_the_rest() {
        assert!(starts_on_keyframe(&frame_moof(true)));
        assert!(!starts_on_keyframe(&frame_moof(false)));
        assert!(starts_on_keyframe(&mp4_box(b"moof", b"x")));
    }

    #[test]
    fn measures_how_far_ffmpeg_falls_behind_the_clock() {
        let mut said = StderrLog::default();
        let line = |t: &str| format!("frame=  10 fps= 15 q=-1.0 size=  1kB time={t} bitrate= 1kbits/s speed=1x");
        // The first reading is the baseline: connecting is not lag.
        assert_eq!(said.lag(&line("00:00:01.00"), 100.0), 0.0);
        assert_eq!(said.lag(&line("00:00:11.00"), 110.0), 0.0);
        // Ten more seconds of clock, six of media: four seconds behind.
        assert_eq!(said.lag(&line("00:00:17.00"), 120.0), 4.0);
        assert_eq!(said.lag(&line("00:01:02.50"), 166.0), 4.5);
        assert_eq!(said.lag("frame= 1 time=N/A", 170.0), 0.0);
    }

    #[test]
    fn logs_the_connection_in_full_then_once_a_minute_with_a_count() {
        let mut said = StderrLog::default();
        let start = Instant::now();
        let first = said.line("Input #0, rtsp, from 'rtsp://admin:secret@10.0.0.5/sub':", 0.0, start);
        assert_eq!(first, ["Input #0, rtsp, from 'rtsp://admin:***@10.0.0.5/sub':"]);
        for _ in 1..FULL_LINES_PER_CONNECTION {
            assert_eq!(said.line("RTP: missed 3 packets", 0.0, start).len(), 1);
        }
        // Past the first lines: one now, then quiet for a minute.
        assert_eq!(said.line("RTP: missed 3 packets", 0.0, start).len(), 1);
        assert!(said.line("RTP: missed 3 packets", 0.0, start + Duration::from_secs(10)).is_empty());
        assert!(said.line("RTP: missed 3 packets", 0.0, start + Duration::from_secs(20)).is_empty());
        let later = said.line("RTP: missed 3 packets", 0.0, start + Duration::from_secs(61));
        assert_eq!(later, ["RTP: missed 3 packets (and 2 more like it)"]);
        // Progress once a minute, and a lag warning past five seconds behind.
        let progress = |t: &str| format!("frame= 10 fps=15 time={t} speed=1x");
        assert_eq!(said.line(&progress("00:00:01.00"), 100.0, start).len(), 1);
        assert!(said.line(&progress("00:00:02.00"), 101.0, start).is_empty());
        assert_eq!(said.line(&progress("00:00:03.00"), 110.0, start), ["8.0s behind the camera"]);
    }

    #[test]
    fn refuses_box_sizes_it_cannot_follow() {
        let mut s = BoxSplitter::default();
        s.feed(&[0, 0, 0, 1, b'm', b'd', b'a', b't', 0, 0]);
        assert!(s.next_box().is_err());
    }

    // Against tools/fake-camera.py, so only on request:
    //   python3 tools/fake-camera.py &
    //   cargo test fake_camera -- --ignored --nocapture
    const FAKE_CAMERA: &str = "rtsp://admin:secret@127.0.0.1:8554/sub";

    #[tokio::test]
    #[ignore]
    async fn fake_camera_streams_to_two_viewers_from_one_ffmpeg_and_stops_after_they_leave() {
        use futures_util::StreamExt;
        let streams = Streams::new(Arc::new(|line| println!("{line}")));
        let kind = |b: &Bytes| String::from_utf8_lossy(&b[4..8]).into_owned();

        let mut first = Box::pin(streams.watch(FAKE_CAMERA));
        let init = first.next().await.expect("an init segment").unwrap();
        assert_eq!(kind(&init), "ftyp");
        assert!(init.windows(4).any(|w| w == b"moof"), "the init segment and a keyframe fragment, as one");
        let fragment = first.next().await.expect("a fragment").unwrap();
        assert_eq!(kind(&fragment), "moof");

        // A second viewer joins mid-stream: the init segment and a keyframe
        // fragment, then every frame after it.
        let mut second = Box::pin(streams.watch(FAKE_CAMERA));
        let start = second.next().await.unwrap().unwrap();
        assert_eq!(kind(&start), "ftyp");
        assert_eq!(kind(&second.next().await.unwrap().unwrap()), "moof");
        assert_eq!(streams.active(), 1);

        drop(first);
        drop(second);
        sleep(LINGER + Duration::from_secs(2)).await;
        assert_eq!(streams.active(), 0);
    }
}
