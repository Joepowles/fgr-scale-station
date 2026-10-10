//! Reading a truck scale's continuous serial output, and deciding when a
//! weight on it is "a vehicle, settled, worth a photo".
//!
//! Pure: no sockets, no clock of its own. `reader.rs` feeds bytes in and
//! timestamps readings; the tests below drive it with captured frames. Kept
//! apart from the socket code so the format quirks of the next indicator are a
//! change here and nowhere else.
//!
//! The yard's indicator sends, twenty times a second, through the NPort:
//!
//! ```text
//!     <STX>    140LG <CR><LF>
//! ```
//!
//! a seven-character weight, a unit letter (L = lb, K = kg), G for gross or N
//! for net, then a status character: space when the reading is good and still,
//! a letter when it is not (M = motion, O = over range, I = invalid on the
//! common Rice Lake / Cardinal formats). Other indicators drop the STX, send
//! "lb" in full, or put the status first; the parser is loose enough for those
//! without being told.

use std::collections::VecDeque;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

pub const STX: u8 = 0x02;

/// One parsed frame. `unit` is None when the indicator does not say.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reading {
    pub weight: f64,
    pub unit: Option<Unit>,
    pub gross: bool,
    pub motion: bool,
    pub valid: bool,
    pub raw: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    #[default]
    Lb,
    Kg,
}

impl std::fmt::Display for Unit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Unit::Lb => "lb",
            Unit::Kg => "kg",
        })
    }
}

// Compiled once, on first use. The first number on the line is the weight.
// Anything before it that is a letter is a status/mode prefix on some
// indicators; the same letters after it are the common case.
static FRAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^([A-Za-z\s]*?)([+-]?)\s*(\d+(?:\.\d+)?)\s*(lbs?|kgs?|L|K)?\s*(.*)$").unwrap());
static NET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bN\b|NET").unwrap());
static MODE_WORDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"NET|GR").unwrap());

/// Bytes off the wire as text, one char per byte, the way Node's 'latin1' does.
pub fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// One line (or STX-delimited frame) in, one reading or None out.
pub fn parse_frame(text: &str) -> Option<Reading> {
    let raw: String = text.chars().filter(|c| !matches!(c, '\x02' | '\x03' | '\r' | '\n')).collect();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let m = FRAME.captures(trimmed)?;
    let group = |i| m.get(i).map_or("", |g| g.as_str());

    let magnitude: f64 = group(3).parse().ok()?;
    let weight = if group(2) == "-" { -magnitude } else { magnitude };
    if !weight.is_finite() {
        return None;
    }
    let unit_letter = group(4).to_ascii_lowercase();
    let unit = match unit_letter.chars().next() {
        Some('k') => Some(Unit::Kg),
        Some(_) => Some(Unit::Lb),
        None => None,
    };
    // G/N say gross or net; the status letters say whether to trust the number.
    let flags = format!("{}{}", group(1), group(5)).to_uppercase();
    let status = MODE_WORDS.replace_all(&flags, "");
    Some(Reading { weight, unit, gross: !NET.is_match(&flags), motion: status.contains('M'), valid: !status.contains(['O', 'I']), raw })
}

/// Turns a byte stream into readings, however the chunks fall. A frame ends
/// at LF, or at the next STX for indicators that send no line ending.
#[derive(Debug, Default)]
pub struct FrameSplitter {
    pending: Vec<u8>,
    resync: bool,
}

impl FrameSplitter {
    /// A connection opened mid-stream starts partway through a frame: "0LG "
    /// is the tail of "    140LG ", and reads as a weight of zero. With
    /// `resync` the first frame boundary is where reading starts.
    pub fn new(resync: bool) -> Self {
        Self { pending: Vec::new(), resync }
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Reading> {
        self.pending.extend_from_slice(chunk);
        let mut readings = Vec::new();
        while let Some(frame) = self.next_frame() {
            if self.resync {
                self.resync = false;
                continue;
            }
            if let Some(reading) = parse_frame(&latin1(&frame)) {
                readings.push(reading);
            }
        }
        // Nothing sensible is longer than this: a stuck line without
        // terminators would otherwise grow forever.
        if self.pending.len() > 256 {
            self.pending.drain(..self.pending.len() - 64);
        }
        readings
    }

    /// Takes the next whole frame off the front of `pending`. An LF ends a
    /// frame and is dropped; an STX (other than one at the very start) starts
    /// the next frame and so stays in `pending`.
    ///
    /// The Electron version cut one byte short when an STX came with no LF
    /// anywhere after it, dropping the status letter, and with it the motion
    /// flag, from indicators that send no line endings. This one does not.
    fn next_frame(&mut self) -> Option<Vec<u8>> {
        let lf = self.pending.iter().position(|&b| b == b'\n');
        let stx = self.pending.iter().skip(1).position(|&b| b == STX).map(|i| i + 1);
        match (lf, stx) {
            (None, None) => None,
            (Some(lf), stx) if stx.is_none_or(|stx| lf < stx) => {
                let frame = self.pending[..lf].to_vec();
                self.pending.drain(..=lf);
                Some(frame)
            }
            (_, Some(stx)) => Some(self.pending.drain(..stx).collect()),
            (Some(_), None) => unreachable!("covered by the LF arm"),
        }
    }
}

/// The trigger's thresholds: the scale section of the settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TriggerSettings {
    #[serde(deserialize_with = "lenient_number")]
    pub min_weight: f64,
    #[serde(deserialize_with = "lenient_number")]
    pub clear_weight: f64,
    #[serde(deserialize_with = "lenient_number")]
    pub stable_seconds: f64,
    #[serde(deserialize_with = "lenient_number")]
    pub stable_tolerance: f64,
}

/// A settings number the way the Electron side read it, `Number(x) || 0`:
/// the Scale tab saves '' for a box left empty, and that is 0, not an error.
pub fn lenient_number<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    let n = match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
        serde_json::Value::String(s) if s.trim().is_empty() => 0.0,
        serde_json::Value::String(s) => s.trim().parse().unwrap_or(0.0),
        serde_json::Value::Bool(b) => f64::from(u8::from(b)),
        _ => 0.0,
    };
    Ok(if n.is_finite() { n } else { 0.0 })
}

impl Default for TriggerSettings {
    fn default() -> Self {
        Self { min_weight: 5000.0, clear_weight: 1000.0, stable_seconds: 2.0, stable_tolerance: 40.0 }
    }
}

/// What a reading amounted to, when it amounted to anything.
#[derive(Debug, Clone, PartialEq)]
pub enum TriggerEvent {
    Weighed { weight: f64, unit: Option<Unit>, gross: bool, at: i64 },
    Reweighed { weight: f64, previous: f64, unit: Option<Unit>, gross: bool, at: i64 },
    Cleared { weight: f64, at: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LastWeighing {
    pub weight: f64,
    pub unit: Option<Unit>,
    pub at: i64,
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    at: i64,
    weight: f64,
    motion: bool,
}

/// When does a stream of readings amount to "a vehicle is on the deck"?
///
/// A truck rolling on takes a second or two to settle, and the indicator's
/// own motion flag lifts a moment before it has really stopped rocking. So
/// the weight has to sit inside a tolerance band for `stable_seconds`, with no
/// motion flag anywhere in that window, and be at or above `min_weight`. That
/// fires once. It does not fire again until the deck has gone back under
/// `clear_weight`: a truck shuffling forward a few feet is the same truck. The
/// deck has to read clear for the whole window too: one low reading - a
/// corrupted frame, a glitch on the line - is not a truck leaving, and must
/// not set up a second weighing of the truck still sitting there.
///
/// But a truck creeping on slowly can settle with only its front axle on the
/// deck, and a tractor can settle before its trailer is on. So while the deck
/// is loaded, a weight that settles again more than the tolerance above the
/// one already taken is raised as a correction (`Reweighed`): the same truck,
/// with more of it on the scale. That can happen more than once.
#[derive(Debug, Clone)]
pub struct WeighingTrigger {
    min_weight: f64,
    clear_weight: f64,
    stable_ms: i64,
    tolerance: f64,
    window: VecDeque<Sample>,
    pub armed: bool,
    pub settled: bool,
    pub last_weighing: Option<LastWeighing>,
}

impl WeighingTrigger {
    pub fn new(s: &TriggerSettings) -> Self {
        let stable_seconds = if s.stable_seconds != 0.0 { s.stable_seconds } else { 2.0 };
        Self {
            min_weight: s.min_weight,
            clear_weight: s.clear_weight.min(s.min_weight),
            stable_ms: ((stable_seconds * 1000.0) as i64).max(500),
            tolerance: s.stable_tolerance.max(0.0),
            window: VecDeque::new(),
            armed: true,
            settled: false,
            last_weighing: None,
        }
    }

    pub fn push(&mut self, reading: &Reading, at: i64) -> Option<TriggerEvent> {
        if !reading.valid {
            return None;
        }
        self.window.push_back(Sample { at, weight: reading.weight, motion: reading.motion });
        let since = at - self.stable_ms;
        while self.window.front().is_some_and(|s| s.at < since) {
            self.window.pop_front();
        }

        let oldest = self.window.front().map_or(at, |s| s.at);
        let spans_window = self.window.len() >= 2 && (at - oldest) as f64 >= self.stable_ms as f64 * 0.9;
        let (min, max) = self.window.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), s| (lo.min(s.weight), hi.max(s.weight)));
        let moving = self.window.iter().any(|s| s.motion);
        let settled = spans_window && !moving && (max - min) <= self.tolerance;
        self.settled = settled;

        let weight = reading.weight;
        if !self.armed {
            if spans_window && max <= self.clear_weight {
                self.armed = true;
                return Some(TriggerEvent::Cleared { weight, at });
            }
            if let Some(last) = self.last_weighing {
                if settled && weight > last.weight + self.tolerance {
                    self.last_weighing = Some(LastWeighing { weight, unit: reading.unit, at });
                    return Some(TriggerEvent::Reweighed { weight, previous: last.weight, unit: reading.unit, gross: reading.gross, at });
                }
            }
            return None;
        }
        if settled && weight >= self.min_weight {
            self.armed = false;
            self.last_weighing = Some(LastWeighing { weight, unit: reading.unit, at });
            return Some(TriggerEvent::Weighed { weight, unit: reading.unit, gross: reading.gross, at });
        }
        None
    }
}

/// "48,320 lb" - the one way a weight is written everywhere it appears.
#[allow(dead_code)] // for the photo stamp, which comes with the weighing pipeline
pub fn format_weight(value: f64, unit: Unit) -> String {
    if !value.is_finite() {
        return String::new();
    }
    let rounded = if value.abs() >= 100.0 { value.round() } else { (value * 10.0).round() / 10.0 };
    let whole = rounded.trunc().abs() as u64;
    let mut digits = whole.to_string();
    let mut i = digits.len();
    while i > 3 {
        i -= 3;
        digits.insert(i, ',');
    }
    let sign = if rounded < 0.0 { "-" } else { "" };
    let fraction = rounded.fract().abs();
    if fraction > 0.0 {
        format!("{sign}{digits}.{} {unit}", (fraction * 10.0).round() as u8)
    } else {
        format!("{sign}{digits} {unit}")
    }
}

// The truck scale's serial output, as captured from the yard's NPort on
// 2026-10-06, and the decision of when a weight on it is a vehicle worth
// photographing. Ported one for one from tests/scaleFrames.test.js.
#[cfg(test)]
mod tests {
    use super::*;

    fn frame_with(weight: impl ToString, status: char, unit: char, mode: char) -> String {
        format!("\x02{:>7}{unit}{mode}{status}\r\n", weight.to_string())
    }
    fn frame(weight: impl ToString) -> String {
        frame_with(weight, ' ', 'L', 'G')
    }

    #[test]
    fn parses_the_yard_indicator_frame() {
        let r = parse_frame(&frame(140)).unwrap();
        assert_eq!((r.weight, r.unit, r.gross, r.motion, r.valid), (140.0, Some(Unit::Lb), true, false, true));
    }

    #[test]
    fn reads_motion_net_and_kilogram_variants() {
        assert!(parse_frame(&frame_with(48320, 'M', 'L', 'G')).unwrap().motion);
        assert!(!parse_frame(&frame_with(48320, ' ', 'L', 'N')).unwrap().gross);
        assert_eq!(parse_frame(&frame_with(21900, ' ', 'K', 'G')).unwrap().unit, Some(Unit::Kg));
        assert_eq!(parse_frame("  12345 lb GR\r\n").unwrap().weight, 12345.0);
        assert_eq!(parse_frame("-20LG \r\n").unwrap().weight, -20.0);
        assert!(!parse_frame(&frame_with(0, 'O', 'L', 'G')).unwrap().valid);
        assert_eq!(parse_frame("garbage"), None);
        assert_eq!(parse_frame(""), None);
    }

    #[test]
    fn splits_frames_however_the_bytes_arrive() {
        let mut s = FrameSplitter::new(false);
        let whole = frame(140) + &frame(145);
        let mut got = s.feed(&whole.as_bytes()[..5]);
        got.extend(s.feed(&whole.as_bytes()[5..]));
        assert_eq!(got.iter().map(|r| r.weight).collect::<Vec<_>>(), [140.0, 145.0]);
    }

    #[test]
    fn a_connection_opened_mid_frame_ignores_the_partial_first_frame() {
        let mut s = FrameSplitter::new(true);
        let readings = s.feed(format!("0LG \r\n{}{}", frame(140), frame(141)).as_bytes());
        assert_eq!(readings.iter().map(|r| r.weight).collect::<Vec<_>>(), [140.0, 141.0]);
    }

    #[test]
    fn splits_on_stx_when_the_indicator_sends_no_line_ending() {
        let mut s = FrameSplitter::new(false);
        let readings = s.feed(b"\x02    140LG \x02    150LG \x02");
        assert_eq!(readings.iter().map(|r| r.weight).collect::<Vec<_>>(), [140.0, 150.0]);
    }

    // New: the Electron splitter lost the last character of these frames.
    #[test]
    fn keeps_the_status_letter_of_frames_split_on_stx() {
        let mut s = FrameSplitter::new(false);
        let readings = s.feed(b"\x02  48320LGM\x02  48320LG \x02");
        assert_eq!(readings.iter().map(|r| r.motion).collect::<Vec<_>>(), [true, false]);
        assert_eq!(readings[0].raw, "  48320LGM");
    }

    #[test]
    fn a_stuck_line_does_not_grow_forever() {
        let mut s = FrameSplitter::new(false);
        s.feed(&[b'x'; 1000]);
        assert!(s.pending.len() <= 256);
    }

    /// Feeds readings into a trigger on a fake clock, 50 ms apart by default,
    /// and keeps what it raised.
    struct Rig {
        trigger: WeighingTrigger,
        now: i64,
        events: Vec<TriggerEvent>,
    }

    impl Rig {
        fn new(min_weight: f64, stable_seconds: f64, start: i64) -> Self {
            let settings = TriggerSettings { min_weight, clear_weight: 1000.0, stable_seconds, stable_tolerance: 40.0 };
            Self { trigger: WeighingTrigger::new(&settings), now: start, events: Vec::new() }
        }
        fn feed(&mut self, weight: f64, motion: bool) {
            let reading = Reading { weight, unit: Some(Unit::Lb), gross: true, motion, valid: true, raw: String::new() };
            if let Some(e) = self.trigger.push(&reading, self.now) {
                self.events.push(e);
            }
            self.now += 50;
        }
        fn feed_n(&mut self, n: usize, weight: impl Fn(usize) -> f64, motion: bool) {
            for i in 0..n {
                self.feed(weight(i), motion);
            }
        }
        fn kinds(&self) -> Vec<&'static str> {
            self.events
                .iter()
                .map(|e| match e {
                    TriggerEvent::Weighed { .. } => "weighed",
                    TriggerEvent::Reweighed { .. } => "reweighed",
                    TriggerEvent::Cleared { .. } => "cleared",
                })
                .collect()
        }
        fn weight_of(&self, i: usize) -> f64 {
            match self.events[i] {
                TriggerEvent::Weighed { weight, .. } | TriggerEvent::Reweighed { weight, .. } | TriggerEvent::Cleared { weight, .. } => weight,
            }
        }
        fn previous_of(&self, i: usize) -> f64 {
            match self.events[i] {
                TriggerEvent::Reweighed { previous, .. } => previous,
                _ => panic!("event {i} is not a reweigh"),
            }
        }
    }

    #[test]
    fn fires_once_per_truck_after_the_weight_settles_and_rearms_when_the_deck_clears() {
        let mut t = Rig::new(5000.0, 2.0, 1000);
        t.feed_n(40, |_| 140.0, false); // empty deck, 2 s
        t.feed_n(20, |i| 20000.0 + i as f64 * 900.0, true); // rolling on, moving
        t.feed_n(30, |i| 48300.0 + (i % 3) as f64 * 10.0, false); // 1.5 s settled: not yet
        assert!(t.events.is_empty());
        t.feed_n(20, |i| 48300.0 + (i % 3) as f64 * 10.0, false); // past 2 s
        assert_eq!(t.kinds(), ["weighed"]);
        assert!(t.trigger.last_weighing.unwrap().weight >= 48300.0);
        t.feed_n(60, |_| 48310.0, false); // still there: nothing more
        t.feed_n(10, |_| 30000.0, true); // rolling off
        t.feed_n(60, |_| 6000.0, false); // a second truck already on? no: deck never cleared
        assert_eq!(t.kinds(), ["weighed"]);
        t.feed_n(5, |_| 140.0, false); // a quarter second clear: not yet
        assert_eq!(t.kinds(), ["weighed"]);
        t.feed_n(40, |_| 140.0, false);
        assert_eq!(t.kinds(), ["weighed", "cleared"]);
        t.feed_n(50, |_| 31000.0, false);
        assert_eq!(t.kinds(), ["weighed", "cleared", "weighed"]);
    }

    #[test]
    fn a_reading_that_wobbles_beyond_the_tolerance_never_settles() {
        let mut t = Rig::new(5000.0, 1.0, 0);
        t.feed_n(100, |i| 30000.0 + (i % 2) as f64 * 100.0, false);
        assert!(t.events.is_empty());
        assert!(!t.trigger.settled);
    }

    #[test]
    fn a_truck_that_settles_heavier_is_reweighed_more_than_once_never_lighter() {
        let mut t = Rig::new(4000.0, 2.0, 1000);
        t.feed_n(40, |_| 140.0, false);
        t.feed_n(10, |i| 1000.0 + i as f64 * 400.0, true); // creeping on
        t.feed_n(50, |i| 4740.0 + (i % 2) as f64 * 10.0, false); // front axle only, steady 2.5 s
        assert_eq!(t.kinds(), ["weighed"]);
        assert!(t.weight_of(0) >= 4740.0);
        t.feed_n(20, |i| 4760.0 + i as f64 * 20.0, false); // a slow creep
        t.feed_n(20, |i| 10000.0 + i as f64 * 1500.0, true); // rolling the rest of the way on
        t.feed_n(50, |i| 38200.0 + (i % 3) as f64 * 10.0, false); // the whole tractor, steady
        assert_eq!(t.kinds(), ["weighed", "reweighed"]);
        assert_eq!(t.previous_of(1), t.weight_of(0));
        assert!(t.weight_of(1) >= 38200.0);
        assert_eq!(t.trigger.last_weighing.unwrap().weight, t.weight_of(1));
        t.feed_n(60, |_| 38210.0, false); // still there: nothing more
        assert_eq!(t.events.len(), 2);
        t.feed_n(20, |i| 40000.0 + i as f64 * 1000.0, true); // the trailer comes on
        t.feed_n(50, |i| 79000.0 + (i % 2) as f64 * 20.0, false);
        assert_eq!(t.kinds(), ["weighed", "reweighed", "reweighed"]);
        assert_eq!(t.previous_of(2), t.weight_of(1));
        t.feed_n(20, |_| 60000.0, true); // part of it rolls off: a lighter settle is not a correction
        t.feed_n(50, |_| 50000.0, false);
        assert_eq!(t.events.len(), 3);
        t.feed_n(45, |_| 140.0, false);
        assert_eq!(t.kinds(), ["weighed", "reweighed", "reweighed", "cleared"]);
        assert_eq!(t.trigger.last_weighing.unwrap().weight, t.weight_of(2));
    }

    #[test]
    fn a_wobble_that_stays_inside_the_tolerance_is_not_a_reweigh() {
        let mut t = Rig::new(4000.0, 1.0, 0);
        t.feed_n(200, |i| 30000.0 + (i % 5) as f64 * 10.0, false);
        assert_eq!(t.kinds(), ["weighed"]);
    }

    #[test]
    fn one_stray_low_reading_does_not_clear_the_deck_or_weigh_the_same_truck_again() {
        let mut t = Rig::new(4000.0, 2.0, 0);
        t.feed_n(40, |_| 140.0, false);
        t.feed_n(60, |_| 32000.0, false);
        assert_eq!(t.kinds(), ["weighed"]);
        for _ in 0..10 {
            // a corrupted frame every three seconds, for half a minute
            t.feed(3.0, false);
            t.feed_n(60, |_| 32000.0, false);
        }
        assert_eq!(t.kinds(), ["weighed"]);
        assert!(!t.trigger.armed);
        t.feed_n(45, |_| 0.0, false); // the truck really leaves
        assert_eq!(t.kinds(), ["weighed", "cleared"]);
    }

    #[test]
    fn reads_thresholds_the_way_the_settings_tab_saves_them() {
        let s: TriggerSettings = serde_json::from_value(serde_json::json!({ "minWeight": "", "clearWeight": "800", "stableSeconds": 3 })).unwrap();
        assert_eq!((s.min_weight, s.clear_weight, s.stable_seconds, s.stable_tolerance), (0.0, 800.0, 3.0, 40.0));
    }

    #[test]
    fn formats_weights_the_way_the_site_does() {
        assert_eq!(format_weight(48320.0, Unit::Lb), "48,320 lb");
        assert_eq!(format_weight(1234567.4, Unit::Kg), "1,234,567 kg");
        assert_eq!(format_weight(12.34, Unit::Lb), "12.3 lb");
        assert_eq!(format_weight(-20.0, Unit::Lb), "-20 lb");
    }
}
