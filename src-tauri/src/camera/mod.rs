//! The scale camera: its live picture for the window, and single frames for
//! the history, both through ffmpeg.
pub mod ffmpeg;
pub mod rtsp;
pub mod snapshot;
pub mod stream;

use serde::Deserialize;

/// The camera section of the settings, as far as streaming cares.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CameraSettings {
    pub username: String,
    pub password: String,
    /// The substream, for the window.
    pub stream_url: String,
    /// The main stream, for photos and plate reads.
    pub hd_stream_url: String,
}

impl CameraSettings {
    /// The stream to use, login included, or None when none is set up. The
    /// HD one falls back to the substream.
    pub fn url(&self, hd: bool) -> Option<String> {
        let url = if hd && !self.hd_stream_url.trim().is_empty() { &self.hd_stream_url } else { &self.stream_url };
        let url = url.trim();
        (!url.is_empty()).then(|| rtsp::inject_credentials(url, &self.username, &self.password))
    }
}
