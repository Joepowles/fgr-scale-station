//! Just enough ONVIF to ask a camera what it is and what streams it has.
//!
//! Written by hand rather than with an ONVIF library because the cameras in
//! the yards are budget units whose SOAP is only loosely standard: the clock
//! may be stuck in 1970, addresses they advertise may not resolve, prefixes
//! vary from one firmware to the next. So replies are read for the few
//! fields wanted, by local name whatever the namespace prefix, and requests
//! are signed with the camera's own idea of the time.

use std::sync::LazyLock;
use std::time::Duration;

use base64::Engine;
use chrono::{DateTime, TimeZone, Utc};
use regex::Regex;
use reqwest::Url;
use sha1::{Digest, Sha1};

const SOAP_TIMEOUT: Duration = Duration::from_secs(10);

pub const NS_TDS: &str = "http://www.onvif.org/ver10/device/wsdl";
pub const NS_TRT: &str = "http://www.onvif.org/ver10/media/wsdl";
pub const NS_TT: &str = "http://www.onvif.org/ver10/schema";
const NS_S: &str = "http://www.w3.org/2003/05/soap-envelope";
const NS_WSSE: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd";
const NS_WSU: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd";

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The camera answered with a SOAP fault: it speaks ONVIF, but said no.
    Fault(String),
    /// An HTTP error status with no fault in it.
    Http(u16),
    /// No answer at all: refused, timed out, not HTTP.
    Transport(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Fault(reason) => write!(f, "ONVIF fault: {reason}"),
            Error::Http(status) => write!(f, "ONVIF HTTP {status}"),
            Error::Transport(why) => write!(f, "{why}"),
        }
    }
}

pub fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn unescape_xml(text: &str) -> String {
    text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// The text of the first element called `name`, whatever its namespace
/// prefix, unescaped and trimmed. Empty when there is none.
pub fn text_of(xml: &str, name: &str) -> String {
    let re = Regex::new(&format!(r"(?is)<(?:[A-Za-z0-9_.-]+:)?{}(?:\s[^>]*)?>([^<]*)<", regex::escape(name))).unwrap();
    re.captures(xml).map(|c| unescape_xml(c[1].trim())).unwrap_or_default()
}

/// The inside of the first element called `name`, for reading within it.
pub fn section_of<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let re = Regex::new(&format!(r"(?is)<(?:[A-Za-z0-9_.-]+:)?{0}\b[^>]*>(.*?)</(?:[A-Za-z0-9_.-]+:)?{0}>", regex::escape(name))).unwrap();
    re.captures(xml).map(|c| c.get(1).unwrap().as_str())
}

fn fault_in(xml: &str) -> Option<String> {
    static FAULT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)Fault>").unwrap());
    if !FAULT.is_match(xml) {
        return None;
    }
    let reason = ["Text", "Reason", "Subcode"].iter().map(|name| text_of(xml, name)).find(|t| !t.is_empty()).unwrap_or_else(|| "SOAP fault".into());
    Some(reason.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The WS-Security password digest: base64(sha1(nonce + created + password)).
pub fn password_digest(nonce: &[u8], created: &str, password: &str) -> String {
    let mut sha = Sha1::new();
    sha.update(nonce);
    sha.update(created.as_bytes());
    sha.update(password.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(sha.finalize())
}

/// `advertised`, but on the host and port it was reached at: some firmware
/// advertises a hostname or address that does not resolve from here, so only
/// its path is believed.
pub fn on_host_of(advertised: &str, given: &str) -> String {
    let (Ok(mut url), Ok(given)) = (Url::parse(advertised), Url::parse(given)) else { return advertised.to_string() };
    let _ = url.set_scheme(given.scheme());
    let _ = url.set_host(given.host_str());
    let _ = url.set_port(given.port());
    url.to_string()
}

pub struct Client {
    device_url: String,
    username: String,
    password: String,
    /// Camera clock minus ours, so a camera with a wrong clock still accepts
    /// the timestamp a request is signed with.
    clock_offset_ms: i64,
    http: reqwest::Client,
    media_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub token: String,
    pub name: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl Client {
    pub fn new(device_url: &str, username: &str, password: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(SOAP_TIMEOUT)
            // Cameras that do HTTPS do it with a certificate they made themselves.
            .tls_danger_accept_invalid_certs(true)
            .build()
            .expect("an HTTP client");
        Self { device_url: device_url.into(), username: username.into(), password: password.into(), clock_offset_ms: 0, http, media_url: None }
    }

    fn security_header(&self) -> String {
        if self.username.is_empty() {
            return String::new();
        }
        let nonce: [u8; 16] = std::array::from_fn(|_| fastrand::u8(..));
        let created = (Utc::now() + chrono::Duration::milliseconds(self.clock_offset_ms)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let digest = password_digest(&nonce, &created, &self.password);
        let nonce = base64::engine::general_purpose::STANDARD.encode(nonce);
        format!(
            "<wsse:Security s:mustUnderstand=\"1\" xmlns:wsse=\"{NS_WSSE}\" xmlns:wsu=\"{NS_WSU}\"><wsse:UsernameToken>\
             <wsse:Username>{}</wsse:Username>\
             <wsse:Password Type=\"http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest\">{digest}</wsse:Password>\
             <wsse:Nonce EncodingType=\"http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary\">{nonce}</wsse:Nonce>\
             <wsu:Created>{created}</wsu:Created></wsse:UsernameToken></wsse:Security>",
            escape_xml(&self.username)
        )
    }

    /// Posts one SOAP request and returns the reply's XML.
    pub async fn request(&self, url: &str, body: &str) -> Result<String, Error> {
        let envelope = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><s:Envelope xmlns:s=\"{NS_S}\"><s:Header>{}</s:Header><s:Body>{body}</s:Body></s:Envelope>",
            self.security_header()
        );
        let reply = self
            .http
            .post(url)
            .header("Content-Type", "application/soap+xml; charset=utf-8")
            .body(envelope)
            .send()
            .await
            .map_err(|e| Error::Transport(if e.is_timeout() { "ONVIF request timed out".into() } else { e.to_string() }))?;
        let status = reply.status().as_u16();
        let xml = reply.text().await.map_err(|e| Error::Transport(e.to_string()))?;
        if let Some(reason) = fault_in(&xml) {
            return Err(Error::Fault(reason));
        }
        if status >= 400 {
            return Err(Error::Http(status));
        }
        Ok(xml)
    }

    /// Learns the camera's clock, so later requests are signed in its time.
    pub async fn connect(&mut self) -> Result<(), Error> {
        let xml = self.request(&self.device_url, &format!("<tds:GetSystemDateAndTime xmlns:tds=\"{NS_TDS}\"/>")).await?;
        if let Some(camera_now) = section_of(&xml, "UTCDateTime").and_then(utc_date_time) {
            self.clock_offset_ms = camera_now.timestamp_millis() - Utc::now().timestamp_millis();
        }
        Ok(())
    }

    /// Where the camera's Media service is.
    pub async fn media_url(&mut self) -> Result<String, Error> {
        if let Some(url) = &self.media_url {
            return Ok(url.clone());
        }
        let caps = self
            .request(&self.device_url, &format!("<tds:GetCapabilities xmlns:tds=\"{NS_TDS}\"><tds:Category>Media</tds:Category></tds:GetCapabilities>"))
            .await?;
        let advertised = section_of(&caps, "Media").map(|m| text_of(m, "XAddr")).filter(|x| !x.is_empty());
        let url = match advertised {
            Some(url) => on_host_of(&url, &self.device_url),
            None => self.device_url.split("/onvif/").next().map_or_else(|| self.device_url.clone(), |base| format!("{base}/onvif/Media")),
        };
        self.media_url = Some(url.clone());
        Ok(url)
    }

    pub async fn profiles(&mut self) -> Result<Vec<Profile>, Error> {
        let media = self.media_url().await?;
        let xml = self.request(&media, &format!("<trt:GetProfiles xmlns:trt=\"{NS_TRT}\"/>")).await?;
        Ok(parse_profiles(&xml))
    }

    /// The URI GetStreamUri or GetSnapshotUri gives for a profile, or empty.
    pub async fn uri(&mut self, which: Uri, profile_token: &str) -> String {
        let Ok(media) = self.media_url().await else { return String::new() };
        let (tag, setup) = match which {
            Uri::Stream => {
                ("Stream", "<trt:StreamSetup><tt:Stream>RTP-Unicast</tt:Stream><tt:Transport><tt:Protocol>RTSP</tt:Protocol></tt:Transport></trt:StreamSetup>")
            }
            Uri::Snapshot => ("Snapshot", ""),
        };
        let body = format!(
            "<trt:Get{tag}Uri xmlns:trt=\"{NS_TRT}\" xmlns:tt=\"{NS_TT}\">{setup}<trt:ProfileToken>{}</trt:ProfileToken></trt:Get{tag}Uri>",
            escape_xml(profile_token)
        );
        self.request(&media, &body).await.map(|xml| text_of(&xml, "Uri")).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Uri {
    Stream,
    Snapshot,
}

fn utc_date_time(xml: &str) -> Option<DateTime<Utc>> {
    let n = |name: &str| text_of(xml, name).parse::<u32>().ok();
    let date = Utc.with_ymd_and_hms(n("Year")? as i32, n("Month")?, n("Day")?, n("Hour")?, n("Minute")?, n("Second")?);
    date.single()
}

/// The media profiles in a GetProfiles reply, with each one's encoder
/// resolution so a list can say which is the big stream.
pub fn parse_profiles(xml: &str) -> Vec<Profile> {
    static PROFILE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<(?:[A-Za-z0-9_.-]+:)?Profiles\b([^>]*)>(.*?)</(?:[A-Za-z0-9_.-]+:)?Profiles>").unwrap());
    static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"token="([^"]+)""#).unwrap());
    PROFILE
        .captures_iter(xml)
        .filter_map(|m| {
            let token = TOKEN.captures(&m[1])?[1].to_string();
            let inside = &m[2];
            let encoder = section_of(inside, "VideoEncoderConfiguration");
            let size = |name| encoder.and_then(|e| text_of(e, name).parse().ok());
            // The profile's own Name, not the one inside its configurations.
            let name = inside.split("Configuration").next().map(|head| text_of(head, "Name")).unwrap_or_default();
            Some(Profile { token, name, width: size("Width"), height: size("Height") })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fields_whatever_the_prefix() {
        let xml = "<tds:GetDeviceInformationResponse><tds:Manufacturer>Acme &amp; Co</tds:Manufacturer><Model>IPC-1</Model></tds:GetDeviceInformationResponse>";
        assert_eq!(text_of(xml, "Manufacturer"), "Acme & Co");
        assert_eq!(text_of(xml, "Model"), "IPC-1");
        assert_eq!(text_of(xml, "Serial"), "");
    }

    #[test]
    fn a_fault_gives_its_reason() {
        let xml = "<s:Envelope><s:Body><s:Fault><s:Code><s:Value>s:Sender</s:Value></s:Code><s:Reason><s:Text xml:lang=\"en\">Sender not\n authorized</s:Text></s:Reason></s:Fault></s:Body></s:Envelope>";
        assert_eq!(fault_in(xml), Some("Sender not authorized".into()));
        assert_eq!(fault_in("<ok/>"), None);
        // gSOAP cameras use SOAP-ENV, and a hyphen is fine in an XML prefix.
        let gsoap = "<SOAP-ENV:Fault><SOAP-ENV:Reason><SOAP-ENV:Text xml:lang=\"en\">Sender not authorized</SOAP-ENV:Text></SOAP-ENV:Reason></SOAP-ENV:Fault>";
        assert_eq!(fault_in(gsoap), Some("Sender not authorized".into()));
    }

    // The worked example from the WS-Security UsernameToken profile.
    #[test]
    fn signs_the_password_the_way_the_standard_says() {
        let nonce = base64::engine::general_purpose::STANDARD.decode("LKqI6G/AikKCQrN0zqZFlg==").unwrap();
        assert_eq!(password_digest(&nonce, "2010-09-16T07:50:45Z", "userpassword"), "tuOSpGlFlIXsozq4HFNeeGeFLEI=");
    }

    #[test]
    fn keeps_the_path_an_address_advertises_but_not_its_host() {
        assert_eq!(on_host_of("http://camera.local/onvif/Media", "http://192.168.1.9:8899/onvif/device_service"), "http://192.168.1.9:8899/onvif/Media");
        assert_eq!(on_host_of("not a url", "http://192.168.1.9/"), "not a url");
    }

    #[test]
    fn reads_profiles_with_their_resolutions() {
        let xml = r#"<trt:GetProfilesResponse>
            <trt:Profiles token="Profile_1" fixed="true"><tt:Name>MainStream</tt:Name>
              <tt:VideoSourceConfiguration token="VSC"><tt:Name>VideoSource</tt:Name></tt:VideoSourceConfiguration>
              <tt:VideoEncoderConfiguration token="VEC1"><tt:Name>Enc1</tt:Name><tt:Resolution><tt:Width>1920</tt:Width><tt:Height>1080</tt:Height></tt:Resolution></tt:VideoEncoderConfiguration>
            </trt:Profiles>
            <trt:Profiles token="Profile_2"><tt:Name>SubStream</tt:Name></trt:Profiles>
        </trt:GetProfilesResponse>"#;
        let profiles = parse_profiles(xml);
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0], Profile { token: "Profile_1".into(), name: "MainStream".into(), width: Some(1920), height: Some(1080) });
        assert_eq!(profiles[1], Profile { token: "Profile_2".into(), name: "SubStream".into(), width: None, height: None });
    }

    #[test]
    fn reads_the_cameras_clock() {
        let xml = "<tt:UTCDateTime><tt:Time><tt:Hour>7</tt:Hour><tt:Minute>50</tt:Minute><tt:Second>45</tt:Second></tt:Time><tt:Date><tt:Year>1970</tt:Year><tt:Month>1</tt:Month><tt:Day>2</tt:Day></tt:Date></tt:UTCDateTime>";
        assert_eq!(utc_date_time(xml).unwrap().to_rfc3339(), "1970-01-02T07:50:45+00:00");
    }
}
