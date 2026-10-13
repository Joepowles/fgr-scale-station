//! Finding ffmpeg and starting it.
//!
//! An installed app has its own ffmpeg beside the executable: Tauri copies
//! the sidecar there (`bundle.externalBin` in tauri.conf.json, filled in by
//! scripts/fetch-ffmpeg.cjs). Failing that, whatever ffmpeg is on the PATH.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;

use tokio::process::Command;

const EXE: &str = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };

fn on_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(EXE)).find(|candidate| candidate.is_file())
}

fn beside_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join(EXE)).filter(|p| p.is_file())
}

/// The ffmpeg to run, or None when there is none.
pub fn path() -> Option<&'static Path> {
    static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();
    FOUND.get_or_init(|| beside_exe().or_else(on_path)).as_deref()
}

pub fn available() -> bool {
    path().is_some()
}

/// An ffmpeg command: no console window on Windows, stdin closed, stdout
/// and stderr piped, and killed if what holds it is dropped.
pub fn command(args: &[&str]) -> std::io::Result<Command> {
    let exe = path().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "ffmpeg is missing from this build"))?;
    let mut cmd = Command::new(exe);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    Ok(cmd)
}
