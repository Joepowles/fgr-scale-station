//! Camera URLs: putting the login into one, and keeping it out of anything
//! shown or logged.

use std::sync::LazyLock;

use regex::Regex;

/// The camera login as the URL wants it, the way JavaScript's
/// encodeURIComponent writes it: "pa#ss" is "pa%23ss".
fn encode_component(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Puts the login into an rtsp:// URL that does not already carry one.
///
///   inject_credentials("rtsp://192.168.1.1/stream1", "admin", "pass")
///     == "rtsp://admin:pass@192.168.1.1/stream1"
pub fn inject_credentials(url: &str, user: &str, pass: &str) -> String {
    let Some(rest) = url.get(7..).filter(|_| url[..7].eq_ignore_ascii_case("rtsp://")) else { return url.to_string() };
    if user.is_empty() || rest.contains('@') {
        return url.to_string();
    }
    format!("{}{}:{}@{rest}", &url[..7], encode_component(user), encode_component(pass))
}

static CREDENTIALS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)([a-z][a-z0-9+.-]*://)([^\s:/@]+):([^\s/]*)@").unwrap());

/// Camera URLs carry the login inline (rtsp://user:pass@host/path), and
/// ffmpeg quotes the URL back in its errors. Run anything derived from a
/// stream URL or an ffmpeg failure through here before showing or logging it.
///
///   redact("rtsp://admin:Egasi%23687@10.0.0.5:554/live")
///     == "rtsp://admin:***@10.0.0.5:554/live"
pub fn redact(text: &str) -> String {
    CREDENTIALS.replace_all(text, "$1$2:***@").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn puts_the_login_into_a_url_without_one() {
        assert_eq!(inject_credentials("rtsp://192.168.1.1/stream1", "admin", "pass"), "rtsp://admin:pass@192.168.1.1/stream1");
        assert_eq!(inject_credentials("RTSP://cam/live", "admin", "p@ss#1"), "RTSP://admin:p%40ss%231@cam/live");
    }

    #[test]
    fn leaves_a_url_alone_that_has_one_or_needs_none() {
        assert_eq!(inject_credentials("rtsp://u:p@cam/live", "admin", "x"), "rtsp://u:p@cam/live");
        assert_eq!(inject_credentials("rtsp://cam/live", "", "x"), "rtsp://cam/live");
        assert_eq!(inject_credentials("http://cam/snap.jpg", "admin", "x"), "http://cam/snap.jpg");
        assert_eq!(inject_credentials("rtsp", "admin", "x"), "rtsp");
    }

    #[test]
    fn takes_passwords_out_of_messages() {
        assert_eq!(redact("rtsp://admin:Egasi%23687@10.0.0.5:554/live"), "rtsp://admin:***@10.0.0.5:554/live");
        assert_eq!(
            redact("[rtsp @ 0x1] rtsp://admin:secret@127.0.0.1:8554/sub: Server returned 401"),
            "[rtsp @ 0x1] rtsp://admin:***@127.0.0.1:8554/sub: Server returned 401"
        );
        assert_eq!(redact("rtsp://cam/live"), "rtsp://cam/live");
    }
}
