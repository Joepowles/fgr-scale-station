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
//! and whole boxes after it are broadcast live.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures_util::Stream;
use tauri::async_runtime::JoinHandle;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::sync::{broadcast, watch};
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

pub type Log = Arc<dyn Fn(String) + Send + Sync>;

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

/// What a viewer joins: the init segment, and the fragments after it.
#[derive(Clone)]
struct Feed {
    init: Bytes,
    fragments: broadcast::Sender<Bytes>,
}

struct Camera {
    /// None until ffmpeg has produced an init segment, and again while it
    /// restarts: a new run means a new init segment, and fragments from it
    /// are useless to a viewer holding the old one.
    feed: watch::Sender<Option<Feed>>,
    viewers: AtomicUsize,
    task: Mutex<Option<JoinHandle<()>>>,
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
                    let current = tokio::time::timeout(INIT_TIMEOUT, feed.wait_for(Option::is_some)).await.ok()?.ok()?.clone()?;
                    let fragments = current.fragments.subscribe();
                    Some((Ok(current.init), Watching::Live { viewer, fragments }))
                }
                Watching::Live { viewer, mut fragments } => {
                    // An error is falling behind, or this run of ffmpeg being over.
                    let fragment = fragments.recv().await.ok()?;
                    Some((Ok(fragment), Watching::Live { viewer, fragments }))
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
        let camera = Arc::new(Camera { feed: watch::channel(None).0, viewers: AtomicUsize::new(0), task: Mutex::new(None) });
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
    Live { viewer: Viewer, fragments: broadcast::Receiver<Bytes> },
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
    Exited(String),
}

async fn run_ffmpeg(camera: &Camera, url: &str, log: &Log) -> Ended {
    let bitrate = std::env::var("CAMERA_STREAM_BITRATE").unwrap_or_else(|_| "1500k".into());
    let fps = std::env::var("CAMERA_STREAM_FPS").ok();
    let mut args = vec![
        "-loglevel",
        "error",
        "-rtsp_transport",
        "tcp",
        "-fflags",
        "nobuffer",
        "-i",
        url,
        "-an",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-tune",
        "zerolatency",
        "-b:v",
        &bitrate,
        // A keyframe about every two seconds. Fragments are cut at keyframes,
        // so this is also how long a new viewer waits for playable video.
        "-g",
        "30",
    ];
    if let Some(fps) = fps.as_deref() {
        args.extend(["-r", fps]);
    }
    args.extend(["-f", "mp4", "-movflags", "frag_keyframe+empty_moov+default_base_moof", "pipe:1"]);

    let mut child = match ffmpeg::command(&args).and_then(|mut c| c.spawn()) {
        Ok(child) => child,
        Err(err) => return Ended::Exited(err.to_string()),
    };
    let mut stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");

    // ffmpeg quotes the input URL back on failure, login included.
    let log_errors = log.clone();
    tauri::async_runtime::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            // Decoder chatter ("Could not find ref with POC 21" on joining an
            // HEVC stream mid-GOP) is normal; only failures are worth a line.
            let lower = line.to_lowercase();
            if ["error", "failed", "unauthor"].iter().any(|word| lower.contains(word)) {
                log_errors(format!("camera stream: {}", rtsp::redact(&line)));
            }
        }
    });

    let mut splitter = BoxSplitter::default();
    let mut init = BytesMut::new();
    let mut fragments: Option<broadcast::Sender<Bytes>> = None;
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
                            (b"mdat", Some(header)) => { let _ = tx.send([header, mp4_box.bytes].concat().into()); }
                            _ => { let _ = tx.send(mp4_box.bytes); }
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
        let fragment = first.next().await.expect("a fragment").unwrap();
        assert_eq!(kind(&fragment), "moof");

        // A second viewer joins mid-stream: same init segment, then a whole fragment.
        let mut second = Box::pin(streams.watch(FAKE_CAMERA));
        assert_eq!(second.next().await.unwrap().unwrap(), init);
        assert_eq!(kind(&second.next().await.unwrap().unwrap()), "moof");
        assert_eq!(streams.active(), 1);

        drop(first);
        drop(second);
        sleep(LINGER + Duration::from_secs(2)).await;
        assert_eq!(streams.active(), 0);
    }
}
