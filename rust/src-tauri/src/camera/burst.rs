//! Frames from the full-resolution stream, a few a second, for as long as a
//! truck is on the deck.
//!
//! A snapshot (snapshot.rs) connects afresh each time and waits for a
//! keyframe: 1.6 s or more from the yard camera, and only ever a keyframe. A
//! truck rolls a long way in that time. This keeps one ffmpeg connected and
//! decoding instead, writing a JPEG every so often, and hands out the newest
//! one. Dropping the feed stops ffmpeg.
//!
//! Frames decoded before the first keyframe would be grey smears; they are
//! filtered out (stream::FROM_FIRST_KEYFRAME).

use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::sync::watch;
use tokio::time::Instant;

use super::ffmpeg;

/// Cuts ffmpeg's stream of JPEGs into whole pictures. ffmpeg's MJPEG encoder
/// writes no thumbnails, and an FF byte in the picture data is always
/// followed by 00, so FFD9 ends a picture and nothing else.
#[derive(Default)]
pub struct JpegSplitter {
    buffer: Vec<u8>,
}

impl JpegSplitter {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            let Some(start) = self.buffer.windows(2).position(|w| w == [0xFF, 0xD8]) else {
                self.buffer.clear();
                return out;
            };
            let Some(end) = self.buffer[start + 2..].windows(2).position(|w| w == [0xFF, 0xD9]) else {
                self.buffer.drain(..start);
                return out;
            };
            let end = start + 2 + end + 2;
            out.push(self.buffer[start..end].to_vec());
            self.buffer.drain(..end);
        }
    }
}

/// One frame and when it arrived.
pub type Frame = (Vec<u8>, Instant);

pub struct Burst {
    latest: watch::Receiver<Option<Frame>>,
    /// When the frame last handed out arrived, so the next is a newer one.
    last: Option<Instant>,
    _task: tokio::task::JoinHandle<()>,
}

impl Burst {
    /// Starts ffmpeg on `url` (login included), writing `fps` frames a second.
    pub fn start(url: &str, fps: f64) -> Result<Self, String> {
        // From the first keyframe on: a gray smear could otherwise be kept
        // as a truck's photo when no plate is read.
        let rate = format!("{},fps={fps}", super::stream::FROM_FIRST_KEYFRAME);
        // One flag and its value per line.
        #[rustfmt::skip]
        let args = [
            "-loglevel", "error",
            "-rtsp_transport", "tcp",
            "-i", url,
            "-an",
            "-vf", &rate,
            "-q:v", "2", // high quality JPEG (1 = best, 31 = worst)
            "-f", "image2pipe",
            "-c:v", "mjpeg",
            "pipe:1",
        ];
        let mut child = ffmpeg::command(&args).and_then(|mut c| c.spawn()).map_err(|e| e.to_string())?;
        let mut stdout = child.stdout.take().expect("piped");
        let (tx, latest) = watch::channel(None);
        let task = tokio::spawn(async move {
            let _child = child; // killed when this task ends or is aborted
            let mut splitter = JpegSplitter::default();
            let mut chunk = vec![0u8; 256 * 1024];
            while let Ok(n) = stdout.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                if let Some(frame) = splitter.feed(&chunk[..n]).pop() {
                    if tx.send(Some((frame, Instant::now()))).is_err() {
                        break;
                    }
                }
            }
        });
        Ok(Self { latest, last: None, _task: task })
    }

    /// The next frame newer than the last one handed out, waiting up to
    /// `wait`. None when none came, or ffmpeg stopped.
    pub async fn next(&mut self, wait: Duration) -> Option<Frame> {
        let last = self.last;
        let frame = tokio::time::timeout(wait, self.latest.wait_for(|f| f.as_ref().is_some_and(|(_, at)| Some(*at) != last))).await.ok()?.ok()?.clone()?;
        self.last = Some(frame.1);
        Some(frame)
    }
}

impl Drop for Burst {
    fn drop(&mut self) {
        self._task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(fill: u8, len: usize) -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8];
        // Picture data with stuffed FF bytes, which are not markers.
        j.extend(std::iter::repeat_n([fill, 0xFF, 0x00], len).flatten());
        j.extend([0xFF, 0xD9]);
        j
    }

    #[test]
    fn cuts_whole_pictures_however_the_bytes_arrive() {
        let (a, b, c) = (jpeg(1, 50), jpeg(2, 70), jpeg(3, 10));
        let stream = [a.clone(), b.clone(), c.clone()].concat();
        let mut s = JpegSplitter::default();
        let mut got = Vec::new();
        for chunk in stream.chunks(13) {
            got.extend(s.feed(chunk));
        }
        assert_eq!(got, [a, b, c]);
    }

    #[test]
    fn skips_bytes_before_a_picture_starts() {
        let mut s = JpegSplitter::default();
        let a = jpeg(9, 5);
        assert_eq!(s.feed(&[[0x00, 0x13, 0xFF].as_slice(), &a].concat()), [a]);
    }

    // Against tools/fake-camera.py, so only on request:
    //   python3 tools/fake-camera.py &
    //   cargo test fake_camera_burst -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn fake_camera_burst_hands_out_newer_frames() {
        let mut burst = Burst::start("rtsp://admin:secret@127.0.0.1:8554/main", 2.0).unwrap();
        let (first, at1) = burst.next(Duration::from_secs(10)).await.expect("a first frame");
        let (second, at2) = burst.next(Duration::from_secs(5)).await.expect("a second frame");
        assert!(at2 > at1);
        assert_eq!(super::super::snapshot::jpeg_size(&first), Some((1920, 1080)));
        assert_eq!(super::super::snapshot::jpeg_size(&second), Some((1920, 1080)));
        println!("two frames {:?} apart", at2 - at1);
    }
}
