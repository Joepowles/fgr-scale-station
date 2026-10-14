//! Crash and freeze reports, sent without anyone pressing anything.
//!
//! A yard PC has no one watching the log, and a frozen app looks alive: the
//! camera picture moves on while the scale reading has stopped (0.2.2, a
//! lock taken twice). So:
//!
//! - a watchdog thread, outside everything it watches, checks every few
//!   seconds that the window's thread, the background workers and the scale
//!   reader still answer. One that has not for a while is a freeze: a report
//!   goes up with the log, a notification says so, and the app restarts
//!   itself (at most three times an hour, so a fault that comes straight
//!   back does not restart for ever);
//! - a panic anywhere is written down and reported;
//! - a run that ended without closing (killed in Task Manager after a
//!   freeze, a crash, the power) is reported on the next start, from the
//!   marker it leaves behind.
//!
//! Reports go the way the Bug report tab sends them, labelled as problems,
//! titled "Crash:" or "Froze:", unless "Send crash reports" is off.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::scale::reader::now_ms;
use crate::{log, AppState};

const MARKER: &str = "running";
const RESTARTS: &str = "restarts.json";
/// The argument a restart starts the new copy with: it waits for this one to go.
pub const AFTER_RESTART: &str = "--after-restart";
const CHECK_EVERY: Duration = Duration::from_secs(5);
/// Not answering for this long is a freeze.
const FROZEN_AFTER_MS: i64 = 20_000;
const MAX_RESTARTS_PER_HOUR: usize = 3;

static APP: OnceLock<AppHandle> = OnceLock::new();
static REPORTED_PANIC: AtomicBool = AtomicBool::new(false);

/// Writes panics down as they happen, and reports the first of each run.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        let place = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic".into());
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        let what = format!("{message} (at {place}, thread {thread})");
        if let Some(app) = APP.get() {
            log(app, format!("crash: {what}"));
            if !REPORTED_PANIC.swap(true, Ordering::SeqCst) {
                send_report(
                    app,
                    "Crash",
                    &what,
                    &format!("Something inside the app failed: {what}\n\nThe app carried on; it may not be working fully until it is restarted."),
                );
            }
        }
    }));
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Marker {
    version: String,
    started_at: i64,
    /// Restarted by the watchdog: not an unexpected end.
    restarting: bool,
}

/// At start: report a previous run that ended without closing, leave this
/// run's marker, and start watching.
pub fn start(app: &AppHandle, dir: &Path) {
    let _ = APP.set(app.clone());
    let marker = dir.join(MARKER);
    if let Some(previous) = std::fs::read_to_string(&marker).ok().and_then(|t| serde_json::from_str::<Marker>(&t).ok()) {
        if !previous.restarting {
            let started = chrono::DateTime::from_timestamp_millis(previous.started_at)
                .map(|d| d.with_timezone(&chrono::Local).format("%b %-d, %-I:%M %p").to_string())
                .unwrap_or_default();
            let what = format!("version {} started {started} ended without closing", previous.version);
            log(app, format!("crash: the last run ({what})"));
            send_report(
                app,
                "Crash",
                &format!("the last run ended without closing ({started})"),
                &format!(
                    "The app's last run ({what}): it crashed, was ended in Task Manager, or the PC lost power or restarted. Its log is below; look before {started} onwards for what it was doing last."
                ),
            );
        }
    }
    let now = Marker { version: app.package_info().version.to_string(), started_at: now_ms(), restarting: false };
    let _ = std::fs::write(&marker, serde_json::to_string(&now).unwrap_or_default());
    watch(app.clone(), dir.to_path_buf());
    #[cfg(debug_assertions)]
    test_freeze(app);
}

/// Development builds only: STATION_TEST_FREEZE=window, scale or panic
/// makes that happen 20 s after start, to see the watchdog and the reports
/// at work.
#[cfg(debug_assertions)]
fn test_freeze(app: &AppHandle) {
    let Ok(what) = std::env::var("STATION_TEST_FREEZE") else { return };
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(20));
        log(&app, format!("test: freezing the {what}"));
        match what.as_str() {
            "window" => {
                let _ = app.run_on_main_thread(|| std::thread::sleep(Duration::from_secs(600)));
            }
            "scale" => {
                let state = app.state::<AppState>();
                let _held = state.scale.lock().unwrap();
                std::thread::sleep(Duration::from_secs(600));
            }
            _ => panic!("a test panic"),
        }
    });
}

/// A clean exit: no marker, nothing to report next time.
pub fn clean_exit(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(MARKER));
}

/// When each part last answered the watchdog.
struct Pings {
    main: AtomicI64,
    runtime: AtomicI64,
}

fn watch(app: AppHandle, dir: PathBuf) {
    let pings = Arc::new(Pings { main: AtomicI64::new(now_ms()), runtime: AtomicI64::new(now_ms()) });
    std::thread::Builder::new()
        .name("watchdog".into())
        .spawn(move || {
            let mut scale_stuck_since: Option<i64> = None;
            let mut reported = false;
            let mut last_round = now_ms();
            loop {
                std::thread::sleep(CHECK_EVERY);
                let now = now_ms();
                // Woken far later than asked: the PC slept. Nothing could
                // answer meanwhile; count afresh rather than call it a freeze.
                if now - last_round > CHECK_EVERY.as_millis() as i64 * 3 {
                    pings.main.store(now, Ordering::SeqCst);
                    pings.runtime.store(now, Ordering::SeqCst);
                    scale_stuck_since = None;
                }
                last_round = now;
                let p = pings.clone();
                let _ = app.run_on_main_thread(move || p.main.store(now_ms(), Ordering::SeqCst));
                let p = pings.clone();
                tauri::async_runtime::spawn(async move { p.runtime.store(now_ms(), Ordering::SeqCst) });

                // The scale reader holds its lock only for a moment per reading.
                let state = app.state::<AppState>();
                let scale_ok = match state.scale.try_lock() {
                    Ok(slot) => slot.as_ref().is_none_or(|r| r.answers()),
                    Err(_) => false,
                };
                if scale_ok {
                    scale_stuck_since = None;
                } else {
                    scale_stuck_since.get_or_insert(now);
                }

                let main_quiet = now - pings.main.load(Ordering::SeqCst);
                let runtime_quiet = now - pings.runtime.load(Ordering::SeqCst);
                let frozen = if main_quiet > FROZEN_AFTER_MS {
                    Some(format!("the window has not answered for {} s", main_quiet / 1000))
                } else if runtime_quiet > FROZEN_AFTER_MS {
                    Some(format!("the background workers have not answered for {} s", runtime_quiet / 1000))
                } else {
                    scale_stuck_since
                        .filter(|since| now - since > FROZEN_AFTER_MS)
                        .map(|since| format!("the scale reader has been stuck for {} s", (now - since) / 1000))
                };
                if let (Some(what), false) = (frozen, reported) {
                    reported = true;
                    froze(&app, &dir, &what);
                }
            }
        })
        .expect("start the watchdog");
}

fn froze(app: &AppHandle, dir: &Path, what: &str) {
    log(app, format!("watchdog: froze - {what}"));
    let restart = may_restart(dir);
    let body = format!(
        "The app stopped working properly: {what}. Nothing was pressed; the watchdog noticed.{}",
        if restart { " It restarted the app." } else { " It has restarted the app three times in the last hour already, so it left it as it is." }
    );
    // Sent before restarting: the restart waits for it, up to half a minute.
    let sent = send_report(app, "Froze", what, &body);
    let _ = app
        .notification()
        .builder()
        .title("Falcon Scale Station stopped working")
        .body(if restart {
            "It is restarting itself. A report has been sent."
        } else {
            "Please close it (in Task Manager if need be) and open it again. A report has been sent."
        })
        .show();
    if restart {
        let _ = sent.recv_timeout(Duration::from_secs(30));
        restart_now(app, dir);
    }
}

/// Fewer than three restarts in the last hour; records this one.
fn may_restart(dir: &Path) -> bool {
    let file = dir.join(RESTARTS);
    let hour_ago = now_ms() - 3_600_000;
    let mut times: Vec<i64> = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    times.retain(|&t| t > hour_ago);
    if times.len() >= MAX_RESTARTS_PER_HOUR {
        return false;
    }
    times.push(now_ms());
    let _ = std::fs::write(&file, serde_json::to_string(&times).unwrap_or_default());
    true
}

fn restart_now(app: &AppHandle, dir: &Path) {
    log(app, "watchdog: restarting the app");
    let marker = Marker { version: app.package_info().version.to_string(), started_at: now_ms(), restarting: true };
    let _ = std::fs::write(dir.join(MARKER), serde_json::to_string(&marker).unwrap_or_default());
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg(AFTER_RESTART).spawn();
    }
    // Not app.exit: the window's thread may be the part that is stuck.
    std::process::exit(1);
}

/// Started with AFTER_RESTART: wait for the copy that started this one to be
/// gone, or the one-copy-at-a-time check hands over to it and quits.
pub fn wait_if_restarted() {
    if std::env::args().any(|a| a == AFTER_RESTART) {
        std::thread::sleep(Duration::from_secs(3));
    }
}

/// Sends a report in the background; the receiver hears when it is done.
/// Nothing here waits on anything that might be stuck: the report is built
/// from what can be had without waiting.
fn send_report(app: &AppHandle, kind: &str, what: &str, body_intro: &str) -> std::sync::mpsc::Receiver<()> {
    let (done, finished) = std::sync::mpsc::channel();
    let state = app.state::<AppState>();
    let support = match state.settings.try_lock() {
        Ok(s) => s.get()["support"].clone(),
        Err(_) => crate::settings::defaults()["support"].clone(),
    };
    if support["autoReport"] == false {
        log(app, format!("{kind} report not sent: sending crash reports is off"));
        let _ = done.send(());
        return finished;
    }
    let report = crate::careful_report(app, &format!("{kind}: {what}\n\n{body_intro}"));
    let report = crate::bug_report::Report { title: crate::bug_report::title_for(&format!("{kind}: {what}")), ..report };
    let builtin = crate::settings::defaults()["support"]["relayUrl"].as_str().unwrap_or("").to_string();
    let app = app.clone();
    std::thread::spawn(move || {
        let sent = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())
            .and_then(|rt| rt.block_on(crate::bug_report::send(&*crate::bug_report::http_post(), &support, &builtin, &report)));
        match sent {
            Ok(issue) => log(&app, format!("crash report sent: {}", issue.url.unwrap_or_default())),
            Err(err) => log(&app, format!("crash report not sent: {err}")),
        }
        let _ = done.send(());
    });
    finished
}

/// For the tests: whether a restart would be allowed with these earlier ones.
#[cfg(test)]
fn allowed_with(dir: &Path, earlier: &[i64]) -> bool {
    let _ = std::fs::write(dir.join(RESTARTS), serde_json::to_string(earlier).unwrap());
    may_restart(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restarts_at_most_three_times_an_hour() {
        let dir = std::env::temp_dir().join(format!("station-crash-{}", fastrand::u32(..)));
        std::fs::create_dir_all(&dir).unwrap();
        let now = now_ms();
        assert!(allowed_with(&dir, &[]));
        assert!(allowed_with(&dir, &[now - 60_000, now - 120_000]));
        assert!(!allowed_with(&dir, &[now - 60_000, now - 120_000, now - 180_000]));
        // Older than an hour do not count.
        assert!(allowed_with(&dir, &[now - 4_000_000, now - 5_000_000, now - 6_000_000]));
        let _ = std::fs::remove_dir_all(dir);
    }
}
