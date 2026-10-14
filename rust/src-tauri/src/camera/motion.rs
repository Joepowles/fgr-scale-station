//! Movement in the camera picture, for the alerts.
//!
//! ffmpeg decodes the small stream and writes a tiny grey picture twice a
//! second; each is compared with the one before, and the share of it that
//! changed is handed on. Tiny, so it costs next to nothing: the decoding is
//! most of it. It also shows whether the camera is sending at all. It runs
//! only while an alert wants it, and stops when dropped.

use std::time::Duration;

use tokio::io::AsyncReadExt;

use super::ffmpeg;
use super::stream::FROM_FIRST_KEYFRAME;

const WIDTH: usize = 96;
const HEIGHT: usize = 54;
const FPS: u32 = 2;
/// A pixel this much brighter or darker counts as changed: sensor noise and
/// compression are under it.
const PIXEL_CHANGE: u8 = 25;
const RESTART_DELAY: Duration = Duration::from_secs(5);

/// The share of pixels that changed between two pictures, 0 to 1.
pub fn changed(a: &[u8], b: &[u8]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let moved = a.iter().zip(b).filter(|(x, y)| x.abs_diff(**y) > PIXEL_CHANGE).count();
    moved as f64 / a.len() as f64
}

pub struct Watcher {
    task: tokio::task::JoinHandle<()>,
}

impl Watcher {
    /// Watches `url` (login included); `on_frame` hears each picture's change.
    pub fn start(url: String, on_frame: impl Fn(f64) + Send + Sync + 'static) -> Self {
        let task = tokio::spawn(async move {
            loop {
                let _ = watch(&url, &on_frame).await;
                tokio::time::sleep(RESTART_DELAY).await;
            }
        });
        Self { task }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.task.abort(); // drops ffmpeg, which kills it
    }
}

async fn watch(url: &str, on_frame: &(impl Fn(f64) + Send + Sync)) -> Result<(), String> {
    let filter = format!("{FROM_FIRST_KEYFRAME},fps={FPS},scale={WIDTH}:{HEIGHT},format=gray");
    // One flag and its value per line.
    #[rustfmt::skip]
    let args = [
        "-loglevel", "error",
        "-rtsp_transport", "tcp",
        "-i", url,
        "-an",
        "-vf", &filter,
        "-f", "rawvideo",
        "pipe:1",
    ];
    let mut child = ffmpeg::command(&args).and_then(|mut c| c.spawn()).map_err(|e| e.to_string())?;
    let mut stdout = child.stdout.take().expect("piped");
    let mut previous: Option<Vec<u8>> = None;
    let mut frame = vec![0u8; WIDTH * HEIGHT];
    loop {
        stdout.read_exact(&mut frame).await.map_err(|e| e.to_string())?;
        if let Some(before) = &previous {
            on_frame(changed(before, &frame));
        }
        previous = Some(frame.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_the_share_of_the_picture_that_changed() {
        let a = vec![100u8; 100];
        let mut b = a.clone();
        assert_eq!(changed(&a, &b), 0.0);
        b[..10].fill(110); // under the noise floor
        assert_eq!(changed(&a, &b), 0.0);
        b[..10].fill(200);
        assert_eq!(changed(&a, &b), 0.1);
        assert_eq!(changed(&a, &[1, 2]), 0.0);
    }

    // Against tools/fake-camera.py, so only on request:
    //   cargo test fake_camera_motion -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn fake_camera_motion_is_measured() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let keep = seen.clone();
        let _w = Watcher::start("rtsp://admin:secret@127.0.0.1:8554/sub".into(), move |c| keep.lock().unwrap().push(c));
        tokio::time::sleep(Duration::from_secs(8)).await;
        let seen = seen.lock().unwrap();
        println!("{seen:?}");
        assert!(seen.len() >= 6, "about two a second");
    }
}
