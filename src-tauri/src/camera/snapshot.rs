//! One JPEG frame from an RTSP stream, by asking ffmpeg for exactly one and
//! reading it off its stdout.

use std::time::Duration;

use super::{ffmpeg, rtsp};

/// Captures a frame. `url` may carry the login already; if not, `user` and
/// `pass` are put in. The error is fit to show: no password in it.
pub async fn capture(url: &str, user: &str, pass: &str, wait: Duration) -> Result<Vec<u8>, String> {
    let full = rtsp::inject_credentials(url, user, pass);
    let args = [
        "-loglevel",
        "error",
        "-rtsp_transport",
        "tcp", // TCP is more reliable than UDP for LAN cameras
        "-i",
        &full,
        "-frames:v",
        "1",
        "-q:v",
        "2", // high quality JPEG (1 = best, 31 = worst)
        "-f",
        "image2pipe",
        "-c:v",
        "mjpeg",
        "pipe:1",
    ];
    let child = ffmpeg::command(&args).and_then(|mut c| c.spawn()).map_err(|e| e.to_string())?;
    // On a timeout the future is dropped, and with it the child: killed.
    let output = match tokio::time::timeout(wait, child.wait_with_output()).await {
        Err(_) => return Err(format!("RTSP stream timed out - no response from camera within {}s", wait.as_secs())),
        Ok(Err(err)) => return Err(format!("ffmpeg failed to run: {err}")),
        Ok(Ok(output)) => output,
    };
    if output.status.success() && is_jpeg(&output.stdout) {
        return Ok(output.stdout);
    }
    Err(explain(&String::from_utf8_lossy(&output.stderr)))
}

/// ffmpeg's complaint, as something a yard operator can act on.
fn explain(stderr: &str) -> String {
    let message = rtsp::redact(stderr);
    if message.contains("401") || message.contains("Unauthorized") {
        "RTSP authentication failed - check username and password".into()
    } else if message.contains("Connection refused") {
        "RTSP connection refused - check the camera IP and port".into()
    } else if message.contains("Name or service not known") || message.contains("No such host") || message.contains("not known") {
        "RTSP host not found - check the camera address".into()
    } else if let Some(line) = message.lines().find(|l| !l.trim().is_empty()) {
        format!("ffmpeg RTSP capture failed: {}", line.trim())
    } else {
        "ffmpeg returned no image".into()
    }
}

fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.len() > 4 && bytes.starts_with(&[0xFF, 0xD8])
}

/// A JPEG's width and height, from its start-of-frame header.
pub fn jpeg_size(jpeg: &[u8]) -> Option<(u16, u16)> {
    if !is_jpeg(jpeg) {
        return None;
    }
    let mut i = 2;
    while i + 9 < jpeg.len() {
        if jpeg[i] != 0xFF {
            return None;
        }
        let marker = jpeg[i + 1];
        let length = u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]) as usize;
        // SOF0 to SOF15, less the three that are not frame headers.
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let height = u16::from_be_bytes([jpeg[i + 5], jpeg[i + 6]]);
            let width = u16::from_be_bytes([jpeg[i + 7], jpeg[i + 8]]);
            return Some((width, height));
        }
        i += 2 + length;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_size_from_a_jpeg_header() {
        // SOI, an APP0 segment, then SOF0 for 960x480.
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01, 0xE0, 0x03, 0xC0, 0x03]);
        assert_eq!(jpeg_size(&jpeg), Some((960, 480)));
        assert_eq!(jpeg_size(b"not a jpeg"), None);
    }

    #[test]
    fn explains_ffmpeg_failures_without_the_password() {
        let e = explain("[rtsp @ 0x1] method DESCRIBE failed: 401 Unauthorized\nrtsp://admin:secret@cam/sub: Server returned 401");
        assert_eq!(e, "RTSP authentication failed - check username and password");
        let e = explain("rtsp://admin:secret@cam/sub: Invalid data found when processing input");
        assert_eq!(e, "ffmpeg RTSP capture failed: rtsp://admin:***@cam/sub: Invalid data found when processing input");
        assert!(!e.contains("secret"));
    }

    // Against tools/fake-camera.py; see camera/stream.rs.
    #[tokio::test]
    #[ignore]
    async fn fake_camera_gives_a_frame_and_refuses_a_wrong_password() {
        let url = "rtsp://127.0.0.1:8554/main";
        let jpeg = capture(url, "admin", "secret", Duration::from_secs(12)).await.unwrap();
        assert_eq!(jpeg_size(&jpeg), Some((1920, 1080)));
        let refused = capture(url, "admin", "wrong", Duration::from_secs(12)).await.unwrap_err();
        assert_eq!(refused, "RTSP authentication failed - check username and password");
    }
}
