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
/// How long the deck must read clear before the next truck is weighed: this
/// many readings in a row, over at least this long (see WeighingTrigger).
pub const CLEAR_READINGS: u32 = 2;
pub const CLEAR_MS: i64 = 400;

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
    /// A truck still not settled this long after arriving is weighed anyway
    /// (0: never), once its reading holds within `unsettled_tolerance`, the
    /// motion flag aside. See WeighingTrigger.
    #[serde(deserialize_with = "lenient_number")]
    pub unsettled_seconds: f64,
    #[serde(deserialize_with = "lenient_number")]
    pub unsettled_tolerance: f64,
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
        Self { min_weight: 5000.0, clear_weight: 1000.0, stable_seconds: 2.0, stable_tolerance: 40.0, unsettled_seconds: 20.0, unsettled_tolerance: 200.0 }
    }
}

/// What a reading amounted to, when it amounted to anything.
#[derive(Debug, Clone, PartialEq)]
pub enum TriggerEvent {
    /// Something over the minimum weight has come onto the deck: a truck
    /// rolling on, not settled yet. Once per truck.
    Arrived {
        weight: f64,
        at: i64,
    },
    Weighed {
        weight: f64,
        unit: Option<Unit>,
        gross: bool,
        at: i64,
        /// Weighed without settling (see WeighingTrigger).
        estimated: bool,
        /// Came on before the truck ahead (weighed at this) had left: the
        /// deck never read clear between them.
        follows: Option<f64>,
    },
    Reweighed {
        weight: f64,
        previous: f64,
        unit: Option<Unit>,
        gross: bool,
        at: i64,
        /// Not more of the truck: a weight inside the tolerance of the one
        /// taken that has now held, settled, longer than it did (ms).
        held: Option<i64>,
    },
    /// Settled lighter than the weight taken, but inside its band: the
    /// driver getting out, say. The weight taken is kept, and no longer
    /// changes for a weight held longer.
    Kept {
        weight: f64,
        locked: f64,
        at: i64,
    },
    Cleared {
        weight: f64,
        at: i64,
    },
    /// The weighed truck's weight has left its band: it is moving, or
    /// something is coming on or going off. Until the deck settles again
    /// nothing more is put down to the truck already weighed.
    Moving {
        weight: f64,
        locked: f64,
        at: i64,
    },
    /// Settled again after moving, without it being a new weighing: back at
    /// the weight it was weighed at (`same`: the truck shuffled), or lighter
    /// with nothing having come on since (part of it is still on the deck as
    /// it pulls off).
    Stopped {
        weight: f64,
        locked: f64,
        same: bool,
        at: i64,
    },
}

/// What a settle after moving amounts to (see WeighingTrigger).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterMoving {
    /// Back at the weight it was weighed at.
    SameTruck,
    /// Heavier, and it never went lighter: more of the same truck came on.
    MoreOfIt,
    /// It went lighter (the truck before leaving), then heavier again
    /// (another coming on), and settled at a weight of its own.
    NextTruck,
    /// Only ever lighter: the truck is pulling off and part of it is still
    /// on the deck.
    PartlyOff,
}

/// `locked`: the weight taken; `low`: the lightest reading since it began
/// to move; `weight`: where it has settled now. `band`: how far a weight may
/// be from another and be the same; `swap`: how far it must drop, and rise
/// again, for one truck to have gone and another come.
pub fn after_moving(locked: f64, low: f64, weight: f64, band: f64, swap: f64) -> AfterMoving {
    if (weight - locked).abs() <= band {
        AfterMoving::SameTruck
    } else if low >= locked - band && weight > locked {
        AfterMoving::MoreOfIt
    } else if low < locked - swap && weight - low > swap {
        AfterMoving::NextTruck
    } else {
        AfterMoving::PartlyOff
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LastWeighing {
    pub weight: f64,
    pub unit: Option<Unit>,
    pub at: i64,
    /// Taken without settling: the first real settle corrects it either way.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub estimated: bool,
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    at: i64,
    weight: f64,
    motion: bool,
}

/// The weighed truck has moved; see WeighingTrigger.
#[derive(Debug, Clone, Copy)]
struct Motion {
    since: i64,
    /// The lightest reading since it began.
    low: f64,
    /// Where it last stopped partly off, so that is said once.
    stopped: Option<f64>,
}

/// Where the trigger is, for the status and the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Nothing over the minimum weight on the deck.
    Empty,
    /// Something is coming on and has not settled yet.
    Arriving,
    /// Weighed, and holding its weight.
    Weighed,
    /// Weighed, and its weight has moved since.
    Moving,
}

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Phase::Empty => "empty",
            Phase::Arriving => "arriving",
            Phase::Weighed => "weighed",
            Phase::Moving => "moving",
        }
    }
}

/// How long the trace of recent readings goes back, and how far apart its
/// readings are: enough to see in a bug report what the deck did.
const TRACE_MS: i64 = 60_000;
const TRACE_EVERY_MS: i64 = 500;

/// When does a stream of readings amount to "a vehicle is on the deck"?
///
/// A truck rolling on takes a second or two to settle, and the indicator's
/// own motion flag lifts a moment before it has really stopped rocking. So
/// the weight has to sit inside a tolerance band for `stable_seconds`, with no
/// motion flag anywhere in that window, and be at or above `min_weight`. That
/// fires once. It does not fire again until the deck has gone back under
/// `clear_weight`: a truck shuffling forward a few feet is the same truck. One
/// low reading - a corrupted frame, a glitch on the line - is not a truck
/// leaving, and must not set up a second weighing of the truck still sitting
/// there, so the deck has to read clear for `CLEAR_READINGS` readings in a
/// row over at least `CLEAR_MS`. Not for the whole settling window: trucks
/// queued nose to tail leave the deck clear for a moment only, and waiting
/// two seconds missed the next truck (WC-Office, 2026-10-14, 35,580 lb) -
/// or took it for more of the one before.
///
/// A truck that never settles - an idling engine shaking the deck, an
/// indicator that keeps its motion flag up - was never weighed at all, and
/// trucks just checking their weight left the history with no trace
/// (WC-Office, 2026-10-15: 44 s on the deck, 120 lb apart, motion flag on).
/// So once a truck has been on the deck `unsettled_seconds` without settling,
/// it is weighed anyway when its reading holds within the looser
/// `unsettled_tolerance` over the window, motion flag or not: the weight is
/// the window's middle reading, and the weighing is marked as estimated. If
/// it settles properly later, that weight replaces it, up or down.
///
/// But a truck creeping on slowly can settle with only its front axle on the
/// deck, and a tractor can settle before its trailer is on. So while the deck
/// is loaded, a weight that settles again more than the tolerance above the
/// one already taken is raised as a correction (`Reweighed`): the same truck,
/// with more of it on the scale. That can happen more than once.
///
/// The first settle is not always the truck at rest: it can settle 20 lb
/// light while it is still easing back on its brakes, then hold the true
/// weight for a minute (FGR, 2026-10-28: 35,780 taken, 35,800 held, issue
/// #29). So a weight inside the tolerance of the one taken that holds
/// unchanged, settled, for longer than the weight taken did (and for at
/// least `stable_seconds`) replaces it (`Reweighed`, `held`): the weight
/// kept is the one the truck held longest. Until it settles lighter than
/// that by more than the tolerance (but inside `unsettled_tolerance`, short
/// of moving): the driver getting out of the cab. The weight is then kept
/// as it is (`Kept`) until the deck clears or the truck moves.
///
/// Customers in a hurry pull on while the truck ahead is still on the deck
/// (Hoffman, 2026-10-28), so the deck never reads clear between them. The
/// truck weighed is held at its weight ("locked") while it stays inside
/// `unsettled_tolerance` of it. Once the reading leaves that band, for
/// `CLEAR_READINGS` readings over `CLEAR_MS` (one stray frame is not
/// movement), the truck is moving (`Moving`), and the lightest reading from
/// then on is kept. When the deck settles again, `after_moving` decides:
///
/// - back within the band of the weight taken: the same truck, shuffled
///   (`Stopped`, same);
/// - heavier and never lighter than it: more of the same truck (`Reweighed`);
/// - lighter by more than the clear weight at some point, and heavier again
///   since by more than the clear weight: the truck ahead has gone and the
///   next one is on: weighed afresh (`Weighed`, `follows` the one before);
/// - only ever lighter: the truck pulling off, part of it still on
///   (`Stopped`, not same), still moving: if it then gets heavier again,
///   that is the next truck.
///
/// A next truck that never settles is weighed anyway on the same terms as
/// one on an empty deck, its seconds counted from when the moving began.
#[derive(Debug, Clone)]
pub struct WeighingTrigger {
    min_weight: f64,
    clear_weight: f64,
    stable_ms: i64,
    tolerance: f64,
    window: VecDeque<Sample>,
    pub armed: bool,
    /// Arrived has been raised for what is on the deck now.
    pub arrived: bool,
    arrived_at: Option<i64>,
    unsettled_ms: i64,
    unsettled_tolerance: f64,
    /// The readings in a row at or under the clear weight, and when they began.
    clear_run: Option<(u32, i64)>,
    /// The readings in a row outside the weighed truck's band.
    off_band: Option<(u32, i64)>,
    motion: Option<Motion>,
    /// A swap of trucks is a drop, then a rise, of more than this.
    swap_weight: f64,
    /// Recent readings, TRACE_EVERY_MS apart, for the log.
    trace: VecDeque<Sample>,
    pub settled: bool,
    pub last_weighing: Option<LastWeighing>,
    /// The reading held unchanged while settled since the truck was
    /// weighed, and since when.
    run: Option<(f64, i64)>,
    /// The longest the weight taken has held unchanged, settled (ms).
    held_ms: i64,
    /// Settled lighter than the weight taken: it stays as it is.
    kept: bool,
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
            arrived: false,
            arrived_at: None,
            unsettled_ms: (s.unsettled_seconds.max(0.0) * 1000.0) as i64,
            unsettled_tolerance: s.unsettled_tolerance.max(s.stable_tolerance.max(0.0)),
            clear_run: None,
            off_band: None,
            motion: None,
            swap_weight: s.clear_weight.min(s.min_weight).max(s.unsettled_tolerance.max(s.stable_tolerance.max(0.0))),
            trace: VecDeque::new(),
            settled: false,
            last_weighing: None,
            run: None,
            held_ms: 0,
            kept: false,
        }
    }

    pub fn phase(&self) -> Phase {
        match (self.armed, self.arrived, self.motion.is_some()) {
            (false, _, true) => Phase::Moving,
            (false, _, false) => Phase::Weighed,
            (true, true, _) => Phase::Arriving,
            (true, false, _) => Phase::Empty,
        }
    }

    /// The readings over the last `seconds`, one every half second, oldest
    /// first: "32000 31980 18040m ..." (m: the motion flag was on).
    pub fn trace(&self, now: i64, seconds: i64) -> String {
        let since = now - seconds * 1000;
        let mut out = String::new();
        for s in self.trace.iter().filter(|s| s.at >= since) {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&format!("{}{}", s.weight.round(), if s.motion { "m" } else { "" }));
        }
        out
    }

    /// The settling window now: lowest and highest weight in it, whether
    /// the motion flag is up anywhere in it, and the tolerance it is held to.
    /// For saying why a truck on the deck has not settled.
    pub fn window_spread(&self) -> Option<(f64, f64, bool, f64)> {
        let first = self.window.front()?;
        let (min, max) = self.window.iter().fold((first.weight, first.weight), |(lo, hi), s| (lo.min(s.weight), hi.max(s.weight)));
        Some((min, max, self.window.iter().any(|s| s.motion), self.tolerance))
    }

    pub fn push(&mut self, reading: &Reading, at: i64) -> Option<TriggerEvent> {
        if !reading.valid {
            return None;
        }
        let sample = Sample { at, weight: reading.weight, motion: reading.motion };
        match self.trace.back_mut() {
            Some(last) if at - last.at < TRACE_EVERY_MS => last.motion |= reading.motion,
            _ => self.trace.push_back(sample),
        }
        while self.trace.front().is_some_and(|s| s.at < at - TRACE_MS) {
            self.trace.pop_front();
        }
        self.window.push_back(sample);
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
        self.clear_run = if weight <= self.clear_weight { Some(self.clear_run.map_or((1, at), |(n, since)| (n + 1, since))) } else { None };
        let deck_clear = self.clear_run.is_some_and(|(n, since)| n >= CLEAR_READINGS && at - since >= CLEAR_MS);
        if !self.armed {
            if deck_clear {
                self.armed = true;
                self.arrived = false;
                self.arrived_at = None;
                self.motion = None;
                self.off_band = None;
                return Some(TriggerEvent::Cleared { weight, at });
            }
            let last = self.last_weighing?;
            let Some(mut motion) = self.motion else {
                // Heavier: more of the truck. Any settle at all after an
                // estimate: the real weight.
                if settled && (weight > last.weight + self.tolerance || last.estimated) {
                    self.off_band = None;
                    self.last_weighing = Some(LastWeighing { weight, unit: reading.unit, at, estimated: false });
                    self.taken(weight, at);
                    return Some(TriggerEvent::Reweighed { weight, previous: last.weight, unit: reading.unit, gross: reading.gross, at, held: None });
                }
                if let Some(event) = self.longest_held(reading, at, last) {
                    self.off_band = None;
                    return Some(event);
                }
                // Out of its band for long enough: on the move.
                self.off_band = if (weight - last.weight).abs() > self.unsettled_tolerance {
                    Some(self.off_band.map_or((1, at), |(n, since)| (n + 1, since)))
                } else {
                    None
                };
                if self.off_band.is_some_and(|(n, since)| n >= CLEAR_READINGS && at - since >= CLEAR_MS) {
                    let since = self.off_band.take().map_or(at, |(_, since)| since);
                    self.motion = Some(Motion { since, low: weight, stopped: None });
                    self.run = None;
                    return Some(TriggerEvent::Moving { weight, locked: last.weight, at });
                }
                return None;
            };
            motion.low = motion.low.min(weight);
            self.motion = Some(motion);
            if weight < self.min_weight {
                return None;
            }
            let (band, swap) = (self.unsettled_tolerance, self.swap_weight);
            let mut verdict = settled.then(|| (after_moving(last.weight, motion.low, weight, band, swap), weight, false));
            // The next truck, never settling: weighed anyway, as on an empty deck.
            let waited = self.unsettled_ms > 0 && at - motion.since >= self.unsettled_ms;
            if verdict.is_none() && waited && spans_window && (max - min) <= self.unsettled_tolerance {
                let middle = self.middle();
                if after_moving(last.weight, motion.low, middle, band, swap) == AfterMoving::NextTruck {
                    verdict = Some((AfterMoving::NextTruck, middle, true));
                }
            }
            let (kind, weight, estimated) = verdict?;
            let locked = last.weight;
            return match kind {
                AfterMoving::SameTruck => {
                    self.motion = None;
                    Some(TriggerEvent::Stopped { weight, locked, same: true, at })
                }
                AfterMoving::MoreOfIt => {
                    self.motion = None;
                    self.last_weighing = Some(LastWeighing { weight, unit: reading.unit, at, estimated: false });
                    self.taken(weight, at);
                    Some(TriggerEvent::Reweighed { weight, previous: locked, unit: reading.unit, gross: reading.gross, at, held: None })
                }
                AfterMoving::NextTruck => {
                    self.motion = None;
                    self.last_weighing = Some(LastWeighing { weight, unit: reading.unit, at, estimated });
                    self.taken(weight, at);
                    Some(TriggerEvent::Weighed { weight, unit: reading.unit, gross: reading.gross, at, estimated, follows: Some(locked) })
                }
                AfterMoving::PartlyOff => {
                    if motion.stopped.is_some_and(|w| (w - weight).abs() <= band) {
                        return None; // said already
                    }
                    self.motion = Some(Motion { stopped: Some(weight), ..motion });
                    Some(TriggerEvent::Stopped { weight, locked, same: false, at })
                }
            };
        }
        if settled && weight >= self.min_weight {
            self.armed = false;
            self.arrived = true;
            self.last_weighing = Some(LastWeighing { weight, unit: reading.unit, at, estimated: false });
            self.taken(weight, at);
            return Some(TriggerEvent::Weighed { weight, unit: reading.unit, gross: reading.gross, at, estimated: false, follows: None });
        }
        if !self.arrived && weight >= self.min_weight {
            self.arrived = true;
            self.arrived_at = Some(at);
            return Some(TriggerEvent::Arrived { weight, at });
        }
        // On the deck long enough without settling: weighed anyway if steady enough.
        let waited = self.arrived_at.is_some_and(|since| self.unsettled_ms > 0 && at - since >= self.unsettled_ms);
        if waited && spans_window && weight >= self.min_weight && (max - min) <= self.unsettled_tolerance {
            let middle = self.middle();
            self.armed = false;
            self.last_weighing = Some(LastWeighing { weight: middle, unit: reading.unit, at, estimated: true });
            self.taken(middle, at);
            return Some(TriggerEvent::Weighed { weight: middle, unit: reading.unit, gross: reading.gross, at, estimated: true, follows: None });
        }
        // A truck that came on and backed off again without settling.
        if self.arrived && deck_clear {
            self.arrived = false;
            self.arrived_at = None;
            return Some(TriggerEvent::Cleared { weight, at });
        }
        None
    }

    /// A weight has just been taken: what it holds is counted from here.
    fn taken(&mut self, weight: f64, at: i64) {
        self.run = Some((weight, at));
        self.held_ms = 0;
        self.kept = false;
    }

    /// The weighed truck settling again inside the tolerance of its weight:
    /// the weight it holds longest is the one kept, until it settles
    /// lighter (see WeighingTrigger).
    fn longest_held(&mut self, reading: &Reading, at: i64, last: LastWeighing) -> Option<TriggerEvent> {
        let weight = reading.weight;
        if !self.settled || self.kept {
            self.run = None;
            return None;
        }
        if weight < last.weight - self.tolerance {
            // Lighter but further off than the band is the truck moving, not this.
            if weight < last.weight - self.unsettled_tolerance {
                self.run = None;
                return None;
            }
            self.kept = true;
            self.run = None;
            return Some(TriggerEvent::Kept { weight, locked: last.weight, at });
        }
        let since = match self.run {
            Some((w, since)) if w == weight => since,
            _ => at,
        };
        self.run = Some((weight, since));
        let held = at - since;
        if weight == last.weight {
            self.held_ms = self.held_ms.max(held);
            return None;
        }
        if held < self.stable_ms || held <= self.held_ms {
            return None;
        }
        self.held_ms = held;
        self.last_weighing = Some(LastWeighing { weight, unit: reading.unit, at, estimated: false });
        Some(TriggerEvent::Reweighed { weight, previous: last.weight, unit: reading.unit, gross: reading.gross, at, held: Some(held) })
    }

    /// The middle reading of the settling window.
    fn middle(&self) -> f64 {
        let mut weights: Vec<f64> = self.window.iter().map(|s| s.weight).collect();
        weights.sort_by(f64::total_cmp);
        weights.get(weights.len() / 2).copied().unwrap_or_default()
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
            let settings = TriggerSettings { min_weight, clear_weight: 1000.0, stable_seconds, stable_tolerance: 40.0, unsettled_seconds: 0.0, unsettled_tolerance: 200.0 };
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
        /// The events that weigh or clear: less Arrived, and less Moving
        /// and Stopped, which only say what the truck is doing (see every_event).
        fn kinds(&self) -> Vec<&'static str> {
            self.every_event().into_iter().filter(|k| !matches!(*k, "arrived" | "moving" | "stopped" | "stopped same" | "kept")).collect()
        }
        fn every_event(&self) -> Vec<&'static str> {
            self.events
                .iter()
                .map(|e| match e {
                    TriggerEvent::Arrived { .. } => "arrived",
                    TriggerEvent::Weighed { .. } => "weighed",
                    TriggerEvent::Reweighed { .. } => "reweighed",
                    TriggerEvent::Kept { .. } => "kept",
                    TriggerEvent::Cleared { .. } => "cleared",
                    TriggerEvent::Moving { .. } => "moving",
                    TriggerEvent::Stopped { same: true, .. } => "stopped same",
                    TriggerEvent::Stopped { same: false, .. } => "stopped",
                })
                .collect()
        }
        /// The events kinds() lists.
        fn weighings(&self) -> Vec<&TriggerEvent> {
            self.events.iter().filter(|e| matches!(e, TriggerEvent::Weighed { .. } | TriggerEvent::Reweighed { .. } | TriggerEvent::Cleared { .. })).collect()
        }
        fn weight_of(&self, i: usize) -> f64 {
            match *self.weighings()[i] {
                TriggerEvent::Weighed { weight, .. } | TriggerEvent::Reweighed { weight, .. } | TriggerEvent::Cleared { weight, .. } => weight,
                _ => unreachable!(),
            }
        }
        fn follows_of(&self, i: usize) -> Option<f64> {
            match *self.weighings()[i] {
                TriggerEvent::Weighed { follows, .. } => follows,
                _ => panic!("event {i} is not a weighing"),
            }
        }
        fn previous_of(&self, i: usize) -> f64 {
            let events = self.weighings();
            match *events[i] {
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
        assert!(t.kinds().is_empty());
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
    fn says_a_truck_has_arrived_as_it_rolls_on_before_it_settles() {
        let mut t = Rig::new(5000.0, 2.0, 1000);
        t.feed_n(40, |_| 140.0, false);
        t.feed_n(5, |i| 2000.0 + i as f64 * 500.0, true); // front wheels, under the minimum
        assert!(t.kinds().is_empty());
        t.feed_n(20, |i| 20000.0 + i as f64 * 900.0, true); // rolling on
        assert_eq!(t.every_event(), ["arrived"]);
        t.feed_n(50, |_| 48300.0, false);
        assert_eq!(t.every_event(), ["arrived", "weighed"]);
        t.feed_n(50, |_| 140.0, false);
        assert_eq!(t.every_event(), ["arrived", "weighed", "cleared"]);
        // The next truck arrives afresh.
        t.feed_n(5, |_| 12000.0, true);
        assert_eq!(t.every_event(), ["arrived", "weighed", "cleared", "arrived"]);
    }

    #[test]
    fn a_truck_that_backs_off_without_settling_clears_the_deck() {
        let mut t = Rig::new(5000.0, 2.0, 1000);
        t.feed_n(40, |_| 140.0, false);
        t.feed_n(20, |i| 8000.0 + i as f64 * 100.0, true);
        t.feed_n(5, |_| 140.0, false); // a moment clear: not yet
        assert_eq!(t.every_event(), ["arrived"]);
        t.feed_n(50, |_| 140.0, false);
        assert_eq!(t.every_event(), ["arrived", "cleared"]);
        assert!(t.trigger.armed); // the next truck is weighed as usual
        t.feed_n(50, |_| 31000.0, false);
        assert_eq!(t.every_event(), ["arrived", "cleared", "arrived", "weighed"]);
    }

    #[test]
    fn trucks_nose_to_tail_are_each_weighed_on_a_slow_indicator() {
        // About 2.2 readings a second, near what FALCON-HE's indicator sends.
        let mut t = Rig::new(4000.0, 2.0, 1000);
        let step = 450;
        let feed = |t: &mut Rig, n: usize, weight: f64, motion: bool| {
            for _ in 0..n {
                let reading = Reading { weight, unit: Some(Unit::Lb), gross: true, motion, valid: true, raw: String::new() };
                if let Some(e) = t.trigger.push(&reading, t.now) {
                    t.events.push(e);
                }
                t.now += step;
            }
        };
        feed(&mut t, 10, 0.0, false);
        feed(&mut t, 4, 30000.0, true);
        feed(&mut t, 8, 41980.0, false); // the first truck, 41,980 lb
        feed(&mut t, 3, 15000.0, true); // driving off
        feed(&mut t, 2, 20.0, false); // under a second of empty deck
        feed(&mut t, 4, 20000.0, true); // the next one already rolling on
        feed(&mut t, 8, 35580.0, false); // 35,580 lb: lighter, so it was lost
        assert_eq!(t.kinds(), ["weighed", "cleared", "weighed"]);
        assert_eq!(t.weight_of(2), 35580.0);
    }

    #[test]
    fn one_low_reading_under_a_truck_is_not_the_truck_leaving() {
        let mut t = Rig::new(4000.0, 2.0, 1000);
        t.feed_n(40, |_| 140.0, false);
        t.feed_n(50, |_| 41980.0, false);
        t.feed(0.0, false); // a corrupted frame
        t.feed_n(5, |_| 41980.0, false);
        t.feed_n(3, |_| 0.0, false); // three readings, 0.1 s: still a glitch
        t.feed_n(50, |_| 41980.0, false);
        assert_eq!(t.kinds(), ["weighed"]);
    }

    #[test]
    fn a_truck_that_never_settles_is_weighed_anyway_after_its_seconds() {
        // WC-Office, 2026-10-15: 28,400 to 28,520 lb with the motion flag on, 44 s.
        let mut t = Rig::new(4000.0, 2.0, 1000);
        t.trigger.unsettled_ms = 20_000;
        t.feed_n(40, |_| 140.0, false);
        t.feed_n(10, |i| 10_000.0 + i as f64 * 1800.0, true);
        t.feed_n(300, |i| 28_400.0 + (i % 7) as f64 * 20.0, true); // 15 s: not yet
        assert_eq!(t.kinds(), Vec::<&str>::new());
        t.feed_n(200, |i| 28_400.0 + (i % 7) as f64 * 20.0, true); // past 20 s
        assert_eq!(t.kinds(), ["weighed"]);
        assert!(matches!(t.events.iter().find(|e| matches!(e, TriggerEvent::Weighed { .. })), Some(TriggerEvent::Weighed { estimated: true, .. })));
        assert!((28_400.0..=28_520.0).contains(&t.weight_of(0)));
        // It drives off: the next truck is weighed as usual.
        t.feed_n(50, |_| 140.0, false);
        t.feed_n(60, |_| 31_000.0, false);
        assert_eq!(t.kinds(), ["weighed", "cleared", "weighed"]);
    }

    #[test]
    fn an_estimate_is_corrected_by_a_real_settle_up_or_down() {
        let mut t = Rig::new(4000.0, 2.0, 1000);
        t.trigger.unsettled_ms = 20_000;
        t.feed_n(40, |_| 140.0, false);
        t.feed_n(450, |i| 30_000.0 + (i % 5) as f64 * 30.0, true);
        assert_eq!(t.kinds(), ["weighed"]);
        let estimate = t.weight_of(0);
        t.feed_n(60, |_| 29_980.0, false); // the engine off: it settles, lighter
        assert_eq!(t.kinds(), ["weighed", "reweighed"]);
        assert_eq!(t.weight_of(1), 29_980.0);
        assert_eq!(t.previous_of(1), estimate);
        t.feed_n(60, |_| 29_980.0, false); // and stays: nothing more
        assert_eq!(t.kinds(), ["weighed", "reweighed"]);
    }

    #[test]
    fn a_truck_wandering_too_far_is_not_estimated() {
        let mut t = Rig::new(4000.0, 2.0, 1000);
        t.trigger.unsettled_ms = 20_000;
        t.feed_n(40, |_| 140.0, false);
        t.feed_n(600, |i| 30_000.0 + (i % 2) as f64 * 400.0, true); // 400 lb apart, 30 s
        assert!(t.kinds().is_empty());
    }

    #[test]
    fn a_reading_that_wobbles_beyond_the_tolerance_never_settles() {
        let mut t = Rig::new(5000.0, 1.0, 0);
        t.feed_n(100, |i| 30000.0 + (i % 2) as f64 * 100.0, false);
        assert!(t.kinds().is_empty());
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
        assert_eq!(t.kinds().len(), 2);
        t.feed_n(20, |i| 40000.0 + i as f64 * 1000.0, true); // the trailer comes on
        t.feed_n(50, |i| 79000.0 + (i % 2) as f64 * 20.0, false);
        assert_eq!(t.kinds(), ["weighed", "reweighed", "reweighed"]);
        assert_eq!(t.previous_of(2), t.weight_of(1));
        t.feed_n(20, |_| 60000.0, true); // part of it rolls off: a lighter settle is not a correction
        t.feed_n(50, |_| 50000.0, false);
        assert_eq!(t.kinds().len(), 3);
        t.feed_n(45, |_| 140.0, false);
        assert_eq!(t.kinds(), ["weighed", "reweighed", "reweighed", "cleared"]);
        assert_eq!(t.trigger.last_weighing.unwrap().weight, t.weight_of(2));
    }

    #[test]
    fn issue_29_the_weight_held_longest_is_kept_not_the_first_settle_or_the_driver_getting_out() {
        let mut t = Rig::new(4000.0, 2.0, 1000);
        t.feed_n(40, |_| 140.0, false);
        t.feed_n(20, |i| 5740.0 + i as f64 * 1500.0, true); // rolling on
        t.feed_n(60, |_| 35_780.0, false); // first settle, 3 s, still easing back
        assert_eq!(t.kinds(), ["weighed"]);
        assert_eq!(t.weight_of(0), 35_780.0);
        t.feed_n(200, |_| 35_800.0, false); // then holds 35,800 for 10 s
        assert_eq!(t.kinds(), ["weighed", "reweighed"]);
        assert_eq!(t.weight_of(1), 35_800.0);
        assert_eq!(t.previous_of(1), 35_780.0);
        assert!(matches!(t.weighings()[1], TriggerEvent::Reweighed { held: Some(_), .. }));
        t.feed_n(10, |_| 35_700.0, true); // the driver climbs down
        t.feed_n(1200, |_| 35_640.0, false); // and the truck sits a minute without him
        assert_eq!(t.kinds(), ["weighed", "reweighed"]);
        assert_eq!(t.every_event().iter().filter(|k| **k == "kept").count(), 1);
        assert_eq!(t.trigger.last_weighing.unwrap().weight, 35_800.0);
        t.feed_n(45, |_| 140.0, false);
        assert_eq!(t.kinds(), ["weighed", "reweighed", "cleared"]);
    }

    #[test]
    fn a_weight_held_less_long_than_the_one_taken_does_not_replace_it() {
        let mut t = Rig::new(4000.0, 2.0, 1000);
        t.feed_n(40, |_| 140.0, false);
        t.feed_n(10, |i| 1000.0 + i as f64 * 3000.0, true);
        t.feed_n(240, |_| 30_000.0, false); // weighed, then holds 10 s
        t.feed_n(120, |_| 30_020.0, false); // 6 s a division up
        t.feed_n(400, |_| 30_000.0, false);
        assert_eq!(t.kinds(), ["weighed"]);
        assert_eq!(t.trigger.last_weighing.unwrap().weight, 30_000.0);
    }

    #[test]
    fn a_reading_flickering_between_two_divisions_keeps_the_first_settle() {
        let mut t = Rig::new(4000.0, 2.0, 1000);
        t.feed_n(40, |_| 140.0, false);
        t.feed_n(10, |i| 1000.0 + i as f64 * 3000.0, true);
        t.feed_n(1200, |i| if (i / 10) % 2 == 0 { 30_000.0 } else { 30_020.0 }, false); // half a second each
        assert_eq!(t.kinds(), ["weighed"]);
    }

    #[test]
    fn once_the_driver_is_out_a_later_hold_does_not_change_the_weight() {
        let mut t = Rig::new(4000.0, 2.0, 1000);
        t.feed_n(40, |_| 140.0, false);
        t.feed_n(10, |i| 1000.0 + i as f64 * 3000.0, true);
        t.feed_n(60, |_| 30_000.0, false);
        t.feed_n(100, |_| 29_860.0, false); // the driver out
        t.feed_n(1200, |_| 29_840.0, false); // a minute, a division lighter still
        t.feed_n(1200, |_| 29_980.0, false); // back in, a minute
        assert_eq!(t.kinds(), ["weighed"]);
        assert_eq!(t.trigger.last_weighing.unwrap().weight, 30_000.0);
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
    fn hoffman_a_truck_pulling_on_behind_one_still_on_the_scale_is_weighed_on_its_own() {
        // 2026-10-28: the truck ahead (no plate) was weighed; the next pulled
        // on before the scale went back to zero.
        let mut t = Rig::new(4000.0, 2.0, 0);
        t.feed_n(40, |_| 0.0, false);
        t.feed_n(10, |i| 5_000.0 + i as f64 * 2_500.0, true);
        t.feed_n(60, |_| 32_000.0, false);
        assert_eq!(t.kinds(), ["weighed"]);
        assert_eq!(t.trigger.phase(), Phase::Weighed);
        t.feed_n(10, |i| 32_000.0 + i as f64 * 1_200.0, true); // the next one's front wheels come on
        assert_eq!(t.trigger.phase(), Phase::Moving);
        t.feed_n(20, |i| 44_000.0 - i as f64 * 1_600.0, true); // the one ahead drives off
        t.feed_n(10, |i| 12_000.0 + i as f64 * 1_550.0, true); // the next rolls the rest of the way on
        t.feed_n(60, |_| 27_500.0, false);
        assert_eq!(t.kinds(), ["weighed", "weighed"]);
        assert_eq!(t.weight_of(1), 27_500.0);
        assert_eq!(t.follows_of(0), None);
        assert_eq!(t.follows_of(1), Some(32_000.0));
        assert_eq!(t.trigger.phase(), Phase::Weighed);
        t.feed_n(45, |_| 0.0, false);
        assert_eq!(t.kinds(), ["weighed", "weighed", "cleared"]);
        assert_eq!(t.trigger.phase(), Phase::Empty);
    }

    #[test]
    fn a_heavier_truck_behind_is_weighed_on_its_own_not_taken_for_more_of_the_one_ahead() {
        let mut t = Rig::new(4000.0, 2.0, 0);
        t.feed_n(40, |_| 0.0, false);
        t.feed_n(60, |_| 30_000.0, false);
        t.feed_n(20, |i| 30_000.0 - i as f64 * 600.0, true); // driving off...
        t.feed_n(20, |i| 18_000.0 + i as f64 * 1_700.0, true); // ...as the next comes on
        t.feed_n(60, |_| 52_000.0, false);
        assert_eq!(t.kinds(), ["weighed", "weighed"]);
        assert_eq!((t.weight_of(1), t.follows_of(1)), (52_000.0, Some(30_000.0)));
    }

    #[test]
    fn a_lighter_truck_nose_to_tail_without_the_deck_clearing_is_weighed() {
        let mut t = Rig::new(4000.0, 2.0, 0);
        t.feed_n(40, |_| 0.0, false);
        t.feed_n(60, |_| 41_980.0, false);
        t.feed_n(10, |_| 15_000.0, true); // driving off, never under the clear weight
        t.feed_n(10, |_| 25_000.0, true);
        t.feed_n(60, |_| 35_580.0, false);
        assert_eq!(t.kinds(), ["weighed", "weighed"]);
        assert_eq!((t.weight_of(1), t.follows_of(1)), (35_580.0, Some(41_980.0)));
    }

    #[test]
    fn a_truck_that_shuffles_and_settles_back_at_its_weight_is_the_same_truck() {
        let mut t = Rig::new(4000.0, 2.0, 0);
        t.feed_n(40, |_| 0.0, false);
        t.feed_n(60, |_| 40_000.0, false);
        t.feed_n(20, |_| 28_000.0, true); // the front axle off the end for a second
        assert_eq!(t.trigger.phase(), Phase::Moving);
        t.feed_n(60, |i| 40_000.0 + (i % 2) as f64 * 10.0, false); // and back on
        assert_eq!(t.every_event(), ["arrived", "weighed", "moving", "stopped same"]);
        assert_eq!(t.trigger.phase(), Phase::Weighed);
        t.feed_n(45, |_| 0.0, false);
        assert_eq!(t.kinds(), ["weighed", "cleared"]);
    }

    #[test]
    fn a_truck_stopping_part_way_off_is_not_a_new_truck() {
        let mut t = Rig::new(4000.0, 2.0, 0);
        t.feed_n(40, |_| 0.0, false);
        t.feed_n(60, |_| 48_000.0, false);
        t.feed_n(20, |i| 48_000.0 - i as f64 * 1_400.0, true);
        t.feed_n(200, |_| 20_000.0, false); // the trailer's axles still on, waiting at the gate
        assert_eq!(t.every_event(), ["arrived", "weighed", "moving", "stopped"]); // said once
        t.feed_n(45, |_| 0.0, false);
        assert_eq!(t.kinds(), ["weighed", "cleared"]);
    }

    #[test]
    fn the_next_truck_coming_on_after_one_stopped_part_way_off_is_weighed() {
        let mut t = Rig::new(4000.0, 2.0, 0);
        t.feed_n(40, |_| 0.0, false);
        t.feed_n(60, |_| 48_000.0, false);
        t.feed_n(10, |_| 30_000.0, true);
        t.feed_n(60, |_| 20_000.0, false); // part way off
        t.feed_n(10, |_| 9_000.0, true); // gone, and the next coming on
        t.feed_n(10, |_| 22_000.0, true);
        t.feed_n(60, |_| 33_000.0, false);
        assert_eq!(t.every_event(), ["arrived", "weighed", "moving", "stopped", "weighed"]);
        assert_eq!((t.weight_of(1), t.follows_of(1)), (33_000.0, Some(48_000.0)));
    }

    #[test]
    fn the_next_trucks_front_waiting_on_the_deck_is_corrected_once_it_is_on_alone() {
        // The scale alone takes a front on the deck for more of the truck
        // ahead (the pipeline asks the camera); once the one ahead has gone,
        // the next is weighed on its own all the same.
        let mut t = Rig::new(4000.0, 2.0, 0);
        t.feed_n(40, |_| 0.0, false);
        t.feed_n(60, |_| 32_000.0, false);
        t.feed_n(10, |_| 38_000.0, true);
        t.feed_n(60, |_| 44_000.0, false);
        assert_eq!(t.kinds(), ["weighed", "reweighed"]);
        t.feed_n(20, |i| 44_000.0 - i as f64 * 1_600.0, true);
        t.feed_n(60, |_| 27_500.0, false);
        assert_eq!(t.kinds(), ["weighed", "reweighed", "weighed"]);
        assert_eq!((t.weight_of(2), t.follows_of(2)), (27_500.0, Some(44_000.0)));
    }

    #[test]
    fn a_next_truck_that_never_settles_is_weighed_anyway() {
        let mut t = Rig::new(4000.0, 2.0, 0);
        t.trigger.unsettled_ms = 20_000;
        t.feed_n(40, |_| 0.0, false);
        t.feed_n(60, |_| 30_000.0, false);
        t.feed_n(10, |_| 10_000.0, true);
        t.feed_n(300, |i| 25_000.0 + (i % 5) as f64 * 30.0, true); // 15 s: not yet
        assert_eq!(t.kinds(), ["weighed"]);
        t.feed_n(200, |i| 25_000.0 + (i % 5) as f64 * 30.0, true);
        assert_eq!(t.kinds(), ["weighed", "weighed"]);
        assert!(matches!(t.weighings()[1], TriggerEvent::Weighed { estimated: true, follows: Some(30_000.0), .. }));
    }

    #[test]
    fn a_weighed_truck_rocking_inside_its_band_is_not_moving() {
        let mut t = Rig::new(4000.0, 2.0, 0);
        t.feed_n(40, |_| 0.0, false);
        t.feed_n(60, |_| 30_000.0, false);
        t.feed_n(400, |i| 30_000.0 + (i % 9) as f64 * 20.0 - 80.0, true); // an idling engine, 160 lb
        t.feed(5_000.0, false); // and a corrupted frame
        t.feed_n(40, |_| 30_000.0, false);
        assert_eq!(t.every_event(), ["arrived", "weighed"]);
    }

    #[test]
    fn decides_what_a_settle_after_moving_is() {
        let after = |low, weight| after_moving(30_000.0, low, weight, 200.0, 1_000.0);
        assert_eq!(after(18_000.0, 30_150.0), AfterMoving::SameTruck);
        assert_eq!(after(29_900.0, 45_000.0), AfterMoving::MoreOfIt);
        assert_eq!(after(12_000.0, 45_000.0), AfterMoving::NextTruck);
        assert_eq!(after(12_000.0, 25_000.0), AfterMoving::NextTruck);
        assert_eq!(after(20_000.0, 20_000.0), AfterMoving::PartlyOff);
        assert_eq!(after(20_000.0, 20_600.0), AfterMoving::PartlyOff); // a wobble back up is not a truck
    }

    #[test]
    fn keeps_a_trace_of_the_readings_for_the_log() {
        let mut t = Rig::new(4000.0, 2.0, 0);
        t.feed_n(20, |_| 0.0, false); // 1 s: two in the trace
        t.feed_n(20, |_| 30_000.0, true);
        assert_eq!(t.trigger.trace(t.now, 30), "0 0 30000m 30000m");
        t.feed_n(1_400, |_| 30_000.0, false); // 70 s later, 60 s are kept
        assert!((120..=121).contains(&t.trigger.trace(t.now, 120).split(' ').count()));
        assert!((60..=61).contains(&t.trigger.trace(t.now, 30).split(' ').count()));
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
