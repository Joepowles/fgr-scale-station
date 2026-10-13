//! A bug report from the yard PC: what the user typed, plus what the app
//! knows - version, machine, settings with passwords removed, the last
//! weighings and the tail of the log - filed as an issue on GitHub so it can
//! be looked at from anywhere without a trip to the machine. The Electron
//! app's bugReport.js.
//!
//! Everything goes in the issue body (GitHub allows 65,536 characters), so a
//! report is one small request: on the yard's connection that matters. It
//! goes through the relay (relay/ in the repository), which holds the GitHub
//! token, unless a token was typed on the Bug report tab.

use std::sync::LazyLock;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use futures_util::future::BoxFuture;
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::camera::rtsp::redact;

const BODY_LIMIT: usize = 60_000;
const LOG_TAIL_CHARS: usize = 40_000;
pub const HISTORY_ENTRIES: usize = 40;
const TIMEOUT: Duration = Duration::from_secs(30);
const LABEL: &str = "from-the-yard";

static SECRET_KEYS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)password|token|secret|passwd").unwrap());
static REPO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\w.-]+/[\w.-]+$").unwrap());

/// A copy of the settings with every password-like value blanked and any
/// credentials inside URLs starred out.
pub fn redact_settings(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(redact_settings).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let set = !(v.is_null() || v == "" || v == false);
                    let out = if SECRET_KEYS.is_match(k) { json!(if set { "***" } else { "" }) } else { redact_settings(v) };
                    (k.clone(), out)
                })
                .collect::<Map<_, _>>(),
        ),
        Value::String(s) => Value::String(redact(s)),
        other => other.clone(),
    }
}

fn fence(text: &str, lang: &str) -> String {
    format!("```{lang}\n{}\n```", text.replace("```", "'''"))
}

pub fn title_for(description: &str) -> String {
    let first = description.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("Problem at the scale");
    if first.chars().count() > 80 {
        format!("{}…", first.chars().take(77).collect::<String>())
    } else {
        first.to_string()
    }
}

/// From `cut`, rounded up to just after the next line break, as the
/// Electron side's indexOf('\n', cut) + 1 does (the start when none).
fn after_line_break(text: &str, cut: usize) -> &str {
    let mut cut = cut.min(text.len());
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    text[cut..].find('\n').map_or(text, |i| &text[cut + i + 1..])
}

pub struct Report {
    pub title: String,
    pub body: String,
}

/// What a report is made of. `info` is the window's app_info; `scale` the
/// scale reader's status, as JSON.
pub struct Input<'a> {
    pub description: &'a str,
    pub info: &'a Value,
    pub settings: &'a Value,
    pub log: &'a str,
    pub history: &'a [Value],
    pub scale: Option<&'a Value>,
    pub machine: String,
    pub now: DateTime<Utc>,
}

pub fn build(input: &Input) -> Report {
    let title = title_for(input.description);
    let info = input.info;
    let flag = |v: &Value| v.as_bool().unwrap_or(false);
    let scale = match input.scale {
        Some(s) => {
            let weight = s["weight"].as_f64().map_or("-".into(), |w| w.to_string());
            let state = if flag(&s["settled"]) {
                "settled"
            } else if flag(&s["motion"]) {
                "moving"
            } else {
                "settling"
            };
            let error = s["lastError"].as_str().filter(|e| !e.is_empty()).map_or(String::new(), |e| format!(" · last error: {e}"));
            format!(
                "{} · {weight} {} · {state} · {} · {} frames{error}",
                if flag(&s["connected"]) { "connected" } else { "offline" },
                s["unit"].as_str().unwrap_or(""),
                if flag(&s["loaded"]) { "loaded" } else { "clear" },
                s["frames"].as_u64().unwrap_or(0),
            )
        }
        None => "not set up".into(),
    };
    let details = [
        ("Reported", input.now.to_rfc3339_opts(SecondsFormat::Millis, true)),
        ("App version", info["version"].as_str().unwrap_or("?").to_string()),
        ("Machine", input.machine.clone()),
        ("Runtime", "Tauri 2 (Rust)".to_string()),
        ("ffmpeg", if flag(&info["ffmpeg"]) { "found" } else { "MISSING" }.to_string()),
        ("Plate reader", if info["plateReader"]["installed"] == false { "MISSING" } else { "bundled" }.to_string()),
        ("Scale", scale),
    ];
    let mut table = String::from("| | |\n|---|---|");
    for (k, v) in details {
        table.push_str(&format!("\n| {k} | {} |", redact(&v)));
    }
    let shown = &input.history[..input.history.len().min(HISTORY_ENTRIES)];
    let entries: Vec<String> = shown
        .iter()
        .map(|e| {
            let mut e = e.clone();
            if let Some(map) = e.as_object_mut() {
                map.remove("photo");
            }
            e.to_string()
        })
        .collect();
    let entries = entries.join("\n");
    let log_text = redact(input.log);
    let mut tail: &str = if log_text.len() > LOG_TAIL_CHARS { after_line_break(&log_text, log_text.len() - LOG_TAIL_CHARS) } else { &log_text };
    let settings = serde_json::to_string_pretty(&redact_settings(input.settings)).unwrap_or_default();
    let description = input.description.trim();

    let assemble = |tail: &str| {
        [
            "## What happened".to_string(),
            String::new(),
            if description.is_empty() { "_(nothing typed)_".to_string() } else { description.to_string() },
            String::new(),
            "## Station".into(),
            String::new(),
            table.clone(),
            String::new(),
            "<details><summary>Settings (passwords removed)</summary>".into(),
            String::new(),
            fence(&settings, "json"),
            String::new(),
            "</details>".into(),
            String::new(),
            format!("<details><summary>Last {} weighings (newest first)</summary>", shown.len()),
            String::new(),
            fence(if entries.is_empty() { "(none)" } else { &entries }, ""),
            String::new(),
            "</details>".into(),
            String::new(),
            format!("<details><summary>Log (last {} lines)</summary>", tail.split('\n').filter(|l| !l.is_empty()).count()),
            String::new(),
            fence(if tail.is_empty() { "(empty)" } else { tail }, ""),
            String::new(),
            "</details>".into(),
            String::new(),
        ]
        .join("\n")
    };

    let mut body = assemble(tail);
    while body.len() > BODY_LIMIT && tail.len() > 1000 {
        tail = after_line_break(tail, tail.len() - tail.len() * 7 / 10);
        body = assemble(tail);
    }
    if body.len() > BODY_LIMIT {
        let mut cut = BODY_LIMIT - 20;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body = format!("{}\n…(cut)", &body[..cut]);
    }
    Report { title, body }
}

/// This PC, as the report's Machine row: name, system and processor.
pub fn machine() -> String {
    let name = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_string()))
        .unwrap_or_else(|| "?".into());
    format!("{name} · {} · {}", std::env::consts::OS, std::env::consts::ARCH)
}

/// An HTTP POST of a JSON body: (status, response text), or why it never
/// got an answer. Passed in so the tests need no network.
pub type Post = dyn Fn(String, Vec<(&'static str, String)>, String) -> BoxFuture<'static, Result<(u16, String), String>> + Send + Sync;

/// The real thing, over reqwest.
pub fn http_post() -> Box<Post> {
    Box::new(|url, headers, body| {
        Box::pin(async move {
            let client = reqwest::Client::builder().timeout(TIMEOUT).build().map_err(|e| e.to_string())?;
            let mut request = client.post(&url).body(body).header("Content-Type", "application/json").header("User-Agent", "fgr-scale-station");
            for (k, v) in headers {
                request = request.header(k, v);
            }
            let response = request.send().await.map_err(|e| if e.is_timeout() { "timeout".to_string() } else { e.to_string() })?;
            let status = response.status().as_u16();
            Ok((status, response.text().await.unwrap_or_default()))
        })
    })
}

#[derive(Debug, PartialEq, serde::Serialize)]
pub struct Issue {
    pub number: Option<u64>,
    pub url: Option<String>,
}

fn explain_status(status: u16, text: &str) -> String {
    let short: String = text.chars().take(200).collect();
    match status {
        401 => "GitHub rejected the token. Check it on the Bug report tab.".into(),
        403 => "GitHub refused: the token cannot open issues on this repository.".into(),
        404 => "GitHub could not find the repository, or the token has no access to it.".into(),
        422 => format!("GitHub rejected the report: {short}"),
        _ => format!("GitHub answered {status}: {short}"),
    }
}

/// Opens the issue on GitHub with a token.
pub async fn file_issue(post: &Post, repo: &str, token: &str, title: &str, body: &str) -> Result<Issue, String> {
    let name = repo.trim().trim_start_matches("https://github.com/").trim_start_matches("http://github.com/");
    let name = name.trim_end_matches('/').trim_end_matches(".git").trim_end_matches('/');
    if !REPO.is_match(name) {
        return Err("The repository must be written as owner/name".into());
    }
    if token.trim().is_empty() {
        return Err("No GitHub token is set. Add one on the Bug report tab.".into());
    }
    let url = format!("https://api.github.com/repos/{name}/issues");
    let headers = || {
        vec![
            ("Authorization", format!("Bearer {}", token.trim())),
            ("Accept", "application/vnd.github+json".to_string()),
            ("X-GitHub-Api-Version", "2022-11-28".to_string()),
        ]
    };
    let mut labels = vec![LABEL];
    loop {
        let payload = json!({ "title": title, "body": body, "labels": labels }).to_string();
        let (status, text) = post(url.clone(), headers(), payload).await.map_err(|e| {
            if e == "timeout" {
                format!("GitHub did not answer within {} s", TIMEOUT.as_secs())
            } else {
                format!("Could not reach GitHub: {e}")
            }
        })?;
        // A label the repository does not have must not sink the report.
        if status == 422 && !labels.is_empty() {
            labels.clear();
            continue;
        }
        if !(200..300).contains(&status) {
            return Err(explain_status(status, &text));
        }
        let json: Value = serde_json::from_str(&text).unwrap_or_default();
        return Ok(Issue { number: json["number"].as_u64(), url: json["html_url"].as_str().map(String::from) });
    }
}

/// Sends the report through the relay, which holds the GitHub token.
pub async fn send_via_relay(post: &Post, relay_url: &str, title: &str, body: &str) -> Result<Issue, String> {
    let base = relay_url.trim().trim_end_matches('/');
    if !base.starts_with("https://") {
        return Err("This build has no bug report relay or GitHub token in it. Enter a token on the Bug report tab, or save the report to a file.".into());
    }
    let payload = json!({ "title": title, "body": body, "labels": [LABEL] }).to_string();
    let (status, text) = post(format!("{base}/report"), vec![], payload).await.map_err(|e| {
        if e == "timeout" {
            format!("The report relay did not answer within {} s", TIMEOUT.as_secs())
        } else {
            format!("Could not reach the report relay: {e}")
        }
    })?;
    let json: Value = serde_json::from_str(&text).unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(json["error"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| format!("The report relay answered {status}: {}", text.chars().take(200).collect::<String>())));
    }
    Ok(Issue { number: json["number"].as_u64(), url: json["url"].as_str().map(String::from) })
}

/// Whichever way is set up: a token on the tab goes straight to GitHub,
/// else the relay. `support` is the settings' support section.
pub async fn send(post: &Post, support: &Value, builtin_relay: &str, title: &str, body: &str) -> Result<Issue, String> {
    let token = support["githubToken"].as_str().unwrap_or("");
    if !token.trim().is_empty() {
        return file_issue(post, support["repo"].as_str().unwrap_or(""), token, title, body).await;
    }
    let relay = support["relayUrl"].as_str().filter(|r| !r.trim().is_empty()).unwrap_or(builtin_relay);
    send_via_relay(post, relay, title, body).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn settings() -> Value {
        json!({
            "camera": { "host": "10.0.0.5", "username": "admin", "password": "Egasi#687", "streamUrl": "rtsp://admin:Egasi%23687@10.0.0.5:554/live" },
            "support": { "githubToken": "github_pat_abc", "repo": "Joepowles/fgr-scale-station" },
            "scale": { "minWeight": 4000 }
        })
    }

    fn input<'a>(description: &'a str, info: &'a Value, settings: &'a Value, log: &'a str, history: &'a [Value], scale: Option<&'a Value>) -> Input<'a> {
        Input { description, info, settings, log, history, scale, machine: "YARD-PC · windows · x86_64".into(), now: Utc::now() }
    }

    #[test]
    fn redacts_passwords_tokens_and_url_credentials() {
        let r = redact_settings(&settings());
        assert_eq!(r["camera"]["password"], "***");
        assert_eq!(r["support"]["githubToken"], "***");
        assert_eq!(r["camera"]["streamUrl"], "rtsp://admin:***@10.0.0.5:554/live");
        assert_eq!(r["camera"]["host"], "10.0.0.5");
        assert_eq!(r["scale"]["minWeight"], 4000);
    }

    #[test]
    fn builds_a_report_with_the_description_first_the_log_tail_and_recent_weighings_nothing_secret() {
        let log: Vec<String> =
            (0..5000).map(|i| format!("2026-10-08T11:29:0{}.000Z weighed 32000 lb line {i} rtsp://admin:Egasi%23687@10.0.0.5/live", i % 10)).collect();
        let log = log.join("\n");
        let history = [
            json!({ "id": "a", "weight": 32240, "plate": { "text": "P1033192" }, "photo": "a.jpg" }),
            json!({ "id": "b", "weight": 32000, "note": "camera busy", "photo": null }),
        ];
        let info = json!({ "version": "0.2.0", "ffmpeg": true });
        let scale = json!({ "connected": true, "weight": 32000.0, "unit": "lb", "settled": true, "loaded": true, "frames": 10 });
        let s = settings();
        let r = build(&input("Truck weighed eleven times\nIt sat on the scale at 32,000 lb.", &info, &s, &log, &history, Some(&scale)));
        assert_eq!(r.title, "Truck weighed eleven times");
        assert!(r.body.starts_with("## What happened\n\nTruck weighed eleven times\nIt sat on the scale at 32,000 lb."));
        assert!(!r.body.contains("Egasi"));
        assert!(!r.body.contains("github_pat_abc"));
        assert!(r.body.contains("line 4999")); // the end of the log is there
        assert!(!r.body.contains("line 0 ")); // the start is not
        assert!(r.body.contains(r#""weight":32240"#));
        assert!(!r.body.contains(r#""photo""#));
        assert!(r.body.contains("App version | 0.2.0"));
        assert!(r.body.contains("connected · 32000 lb · settled · loaded"));
        assert!(r.body.len() <= BODY_LIMIT);
    }

    #[test]
    fn a_long_title_is_cut_and_an_empty_description_still_reports() {
        let (info, s) = (json!({}), json!({}));
        let long = "x".repeat(200);
        let r = build(&input(&long, &info, &s, "", &[], None));
        assert_eq!(r.title.chars().count(), 78);
        assert_eq!(build(&input("", &info, &s, "", &[], None)).title, "Problem at the scale");
        assert!(r.body.contains("(empty)"));
        assert!(r.body.contains("| Scale | not set up |"));
    }

    type Calls = Arc<Mutex<Vec<(String, Vec<(&'static str, String)>, Value)>>>;

    /// A POST that records what it was sent and answers from `answers` in turn.
    fn fake(answers: Vec<Result<(u16, &'static str), &'static str>>) -> (Box<Post>, Calls) {
        let calls: Calls = Arc::default();
        let answers = Arc::new(Mutex::new(answers.into_iter()));
        let seen = calls.clone();
        let post: Box<Post> = Box::new(move |url, headers, body| {
            seen.lock().unwrap().push((url, headers, serde_json::from_str(&body).unwrap()));
            let answer = answers.lock().unwrap().next().expect("an answer");
            Box::pin(async move { answer.map(|(s, t)| (s, t.to_string())).map_err(String::from) })
        });
        (post, calls)
    }

    #[tokio::test]
    async fn files_the_issue_with_the_token_and_reads_back_its_link() {
        let (post, calls) = fake(vec![Ok((201, r#"{"number": 12, "html_url": "https://github.com/Joepowles/fgr-scale-station/issues/12"}"#))]);
        let r = file_issue(&*post, "https://github.com/Joepowles/fgr-scale-station/", " tok ", "T", "B").await.unwrap();
        assert_eq!(r, Issue { number: Some(12), url: Some("https://github.com/Joepowles/fgr-scale-station/issues/12".into()) });
        let calls = calls.lock().unwrap();
        assert_eq!(calls[0].0, "https://api.github.com/repos/Joepowles/fgr-scale-station/issues");
        assert!(calls[0].1.contains(&("Authorization", "Bearer tok".into())));
        assert_eq!(calls[0].2, json!({ "title": "T", "body": "B", "labels": ["from-the-yard"] }));
    }

    #[tokio::test]
    async fn explains_a_bad_token_a_missing_repo_and_no_token() {
        let answer = |status| fake(vec![Ok((status, r#"{"message":"Bad credentials"}"#))]).0;
        assert!(file_issue(&*answer(401), "a/b", "t", "T", "B").await.unwrap_err().contains("rejected the token"));
        assert!(file_issue(&*answer(404), "a/b", "t", "T", "B").await.unwrap_err().contains("could not find the repository"));
        assert!(file_issue(&*answer(201), "a/b", "", "T", "B").await.unwrap_err().contains("No GitHub token"));
        assert!(file_issue(&*answer(201), "nonsense", "t", "T", "B").await.unwrap_err().contains("owner/name"));
        let offline = fake(vec![Err("dns error: api.github.com")]).0;
        assert!(file_issue(&*offline, "a/b", "t", "T", "B").await.unwrap_err().contains("Could not reach GitHub"));
    }

    #[tokio::test]
    async fn a_label_the_repository_refuses_is_dropped_and_the_report_still_goes() {
        let (post, calls) = fake(vec![Ok((422, "Validation Failed")), Ok((201, r#"{"number": 3, "html_url": "u"}"#))]);
        assert_eq!(file_issue(&*post, "a/b", "t", "T", "B").await.unwrap().number, Some(3));
        let labels: Vec<Value> = calls.lock().unwrap().iter().map(|c| c.2["labels"].clone()).collect();
        assert_eq!(labels, [json!(["from-the-yard"]), json!([])]);
    }

    #[tokio::test]
    async fn sends_through_the_relay_and_reads_back_its_answer_or_its_error() {
        let (post, calls) = fake(vec![Ok((201, r#"{"number": 9, "url": "https://github.com/Joepowles/fgr-scale-station/issues/9"}"#))]);
        let r = send_via_relay(&*post, "https://scale-station-reports.example.workers.dev/", "T", "B").await.unwrap();
        assert_eq!(r, Issue { number: Some(9), url: Some("https://github.com/Joepowles/fgr-scale-station/issues/9".into()) });
        assert_eq!(calls.lock().unwrap()[0].0, "https://scale-station-reports.example.workers.dev/report");
        assert_eq!(calls.lock().unwrap()[0].2, json!({ "title": "T", "body": "B", "labels": ["from-the-yard"] }));
        let limited = fake(vec![Ok((429, r#"{"error": "Too many reports from this address; try again in a minute."}"#))]).0;
        assert!(send_via_relay(&*limited, "https://r.example", "T", "B").await.unwrap_err().contains("Too many reports"));
        assert!(send_via_relay(&*fake(vec![]).0, "", "T", "B").await.unwrap_err().contains("no bug report relay or GitHub token"));
        let offline = fake(vec![Err("dns error")]).0;
        assert!(send_via_relay(&*offline, "https://r.example", "T", "B").await.unwrap_err().contains("Could not reach the report relay"));
    }

    #[tokio::test]
    async fn a_token_on_the_tab_goes_straight_to_github_otherwise_the_relay_built_in() {
        let ok = r#"{"number": 1, "url": "u", "html_url": "u"}"#;
        let (post, calls) = fake(vec![Ok((201, ok)), Ok((201, ok)), Ok((201, ok))]);
        send(&*post, &json!({ "githubToken": "t", "repo": "a/b" }), "https://r.example", "T", "B").await.unwrap();
        send(&*post, &json!({ "githubToken": "", "repo": "a/b" }), "https://r.example", "T", "B").await.unwrap();
        send(&*post, &json!({ "relayUrl": "https://mine.example" }), "https://r.example", "T", "B").await.unwrap();
        let urls: Vec<String> = calls.lock().unwrap().iter().map(|c| c.0.clone()).collect();
        assert_eq!(urls, ["https://api.github.com/repos/a/b/issues", "https://r.example/report", "https://mine.example/report"]);
    }
}
