//! Alerts: the Settings' Alerts tab, the rules on it, and when they go off.
//!
//! A rule is a "when" (the scale passing a weight, a truck weighed, a plate
//! read, movement in the camera picture, the scale or camera going quiet...)
//! and a "what" (a sound, for how long, a banner on the screen, a Windows
//! notification). Quiet hours and a mute button hold the sounds back.
//!
//! This is the deciding part only, kept free of the app so it can be tested:
//! the app feeds it what happens (`Input`) and the time, and plays, shows and
//! logs what comes back (`Fired`).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The kinds of "when", as the settings name them.
pub mod kind {
    pub const WEIGHT_ABOVE: &str = "weightAbove";
    pub const WEIGHED: &str = "weighed";
    pub const CLEARED: &str = "cleared";
    pub const PLATE: &str = "plate";
    pub const NO_PLATE: &str = "noPlate";
    pub const UNSETTLED: &str = "unsettled";
    pub const MOTION: &str = "motion";
    pub const SCALE_OFFLINE: &str = "scaleOffline";
    pub const CAMERA_OFFLINE: &str = "cameraOffline";
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AlertSettings {
    pub enabled: bool,
    /// 0 to 100, applied to every rule's own volume.
    pub volume: f64,
    /// Windows notifications at all (each rule says whether it sends one).
    pub notifications: bool,
    pub tray_icon: bool,
    pub quiet: Quiet,
    pub rules: Vec<Rule>,
}

impl Default for AlertSettings {
    fn default() -> Self {
        Self { enabled: true, volume: 80.0, notifications: true, tray_icon: true, quiet: Quiet::default(), rules: default_rules() }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Quiet {
    pub enabled: bool,
    /// "22:00": from, to; across midnight when `to` is earlier.
    pub from: String,
    pub to: String,
    /// The days it applies on, 0 = Sunday. Empty: every day.
    pub days: Vec<u8>,
    /// "sound": banners and notifications still show; "all": only the log.
    pub mode: String,
}

impl Default for Quiet {
    fn default() -> Self {
        Self { enabled: false, from: "22:00".into(), to: "06:00".into(), days: vec![], mode: "sound".into() }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub kind: String,
    /// weightAbove: the weight. weighed: only at or over this (0: any).
    pub threshold: f64,
    /// weighed: only at or under this (0: no limit).
    pub max_weight: f64,
    /// unsettled, scaleOffline, cameraOffline: how long first.
    pub seconds: f64,
    /// plate: "any", "listed" (only plates in `plates`) or "unlisted".
    pub plate_match: String,
    /// plate: comma- or space-separated.
    pub plates: String,
    /// motion: "low", "medium" or "high".
    pub sensitivity: String,
    /// motion: only while nothing is on the scale.
    pub only_empty_scale: bool,
    /// "chime", "bell", "beep", "double", "alarm", "custom" or "none".
    pub sound: String,
    /// custom: a WAV or MP3 on this PC.
    pub sound_file: String,
    /// 0 to 100.
    pub volume: f64,
    /// "once", "seconds" (for `length_seconds`) or "dismiss" (until dismissed).
    pub length: String,
    pub length_seconds: f64,
    pub banner: bool,
    pub notify: bool,
    /// Not again within this many seconds.
    pub cooldown_seconds: f64,
    pub ignore_quiet: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            enabled: true,
            kind: kind::WEIGHED.into(),
            threshold: 0.0,
            max_weight: 0.0,
            seconds: 30.0,
            plate_match: "any".into(),
            plates: String::new(),
            sensitivity: "medium".into(),
            only_empty_scale: true,
            sound: "chime".into(),
            sound_file: String::new(),
            volume: 100.0,
            length: "once".into(),
            length_seconds: 10.0,
            banner: true,
            notify: false,
            cooldown_seconds: 10.0,
            ignore_quiet: false,
        }
    }
}

/// The rules a fresh install starts with: one on, to hear trucks weighed, and
/// the rest ready to switch on.
pub fn default_rules() -> Vec<Rule> {
    let rule = |id: &str, name: &str, enabled: bool, r: Rule| Rule { id: id.into(), name: name.into(), enabled, ..r };
    vec![
        rule(
            "weighed",
            "Truck weighed",
            true,
            Rule { kind: kind::WEIGHED.into(), sound: "chime".into(), banner: false, cooldown_seconds: 0.0, ..Rule::default() },
        ),
        rule(
            "on-scale",
            "Something on the scale",
            false,
            Rule { kind: kind::WEIGHT_ABOVE.into(), threshold: 500.0, sound: "beep".into(), banner: false, ..Rule::default() },
        ),
        rule(
            "overweight",
            "Overweight",
            false,
            Rule {
                kind: kind::WEIGHED.into(),
                threshold: 80_000.0,
                sound: "alarm".into(),
                length: "dismiss".into(),
                notify: true,
                ignore_quiet: true,
                cooldown_seconds: 0.0,
                ..Rule::default()
            },
        ),
        rule(
            "unsettled",
            "Truck not settling",
            false,
            Rule { kind: kind::UNSETTLED.into(), seconds: 30.0, sound: "double".into(), notify: true, ..Rule::default() },
        ),
        rule("no-plate", "No plate read", false, Rule { kind: kind::NO_PLATE.into(), sound: "bell".into(), ..Rule::default() }),
        rule("motion", "Movement in the yard", false, Rule { kind: kind::MOTION.into(), sound: "beep".into(), cooldown_seconds: 60.0, ..Rule::default() }),
        rule(
            "scale-offline",
            "Scale not answering",
            false,
            Rule {
                kind: kind::SCALE_OFFLINE.into(),
                seconds: 60.0,
                sound: "alarm".into(),
                length: "seconds".into(),
                notify: true,
                cooldown_seconds: 600.0,
                ..Rule::default()
            },
        ),
        rule(
            "camera-offline",
            "Camera not answering",
            false,
            Rule {
                kind: kind::CAMERA_OFFLINE.into(),
                seconds: 60.0,
                sound: "alarm".into(),
                length: "seconds".into(),
                notify: true,
                cooldown_seconds: 600.0,
                ..Rule::default()
            },
        ),
    ]
}

/// The settings' "alerts" section as a fresh install has it.
pub fn defaults_json() -> Value {
    serde_json::to_value(AlertSettings::default()).unwrap_or_else(|_| json!({}))
}

/// "22:00" as minutes after midnight.
fn minutes(text: &str) -> Option<u32> {
    let (h, m) = text.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// Whether quiet hours hold at this weekday (0 = Sunday) and minute of the
/// day. Hours across midnight count from the day they start.
pub fn in_quiet_hours(q: &Quiet, weekday: u8, minute: u32) -> bool {
    if !q.enabled {
        return false;
    }
    let (Some(from), Some(to)) = (minutes(&q.from), minutes(&q.to)) else { return false };
    let day_ok = |d: u8| q.days.is_empty() || q.days.contains(&d);
    if from <= to {
        day_ok(weekday) && minute >= from && minute < to
    } else {
        // 22:00 to 06:00: tonight from 22:00, or this morning on from yesterday.
        (day_ok(weekday) && minute >= from) || (day_ok((weekday + 6) % 7) && minute < to)
    }
}

/// What happened, as the alerts see it.
#[derive(Debug, Clone)]
pub enum Input {
    /// Every half second: the scale's weight now and when it last sent a reading.
    Tick {
        weight: Option<f64>,
        scale_enabled: bool,
        last_reading_at: Option<i64>,
        started_at: i64,
    },
    Arrived,
    Weighed {
        weight: f64,
        unit: String,
    },
    Cleared,
    /// The plate decided for the truck on the deck.
    Plate {
        text: String,
    },
    /// The frames of a weighed truck are over and no plate was read.
    NoPlate {
        weight: f64,
        unit: String,
    },
    /// How much of the camera picture changed, 0 to 1.
    Motion {
        changed: f64,
    },
    /// A frame from the camera came (the motion watcher's).
    CameraFrame,
}

/// An alert to give.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Fired {
    pub rule_id: String,
    pub name: String,
    pub message: String,
    /// "none" when it is not to be heard (quiet hours, muted).
    pub sound: String,
    pub sound_file: String,
    /// 0 to 1, the rule's times the master volume.
    pub volume: f64,
    pub length: String,
    pub length_seconds: f64,
    pub banner: bool,
    pub notify: bool,
    /// Why it is silent, for the banner: "quiet hours", "muted", or empty.
    pub silenced: String,
}

/// Movement in the camera picture counts when at least this much of it changed.
pub fn motion_threshold(sensitivity: &str) -> f64 {
    match sensitivity {
        "high" => 0.01,
        "low" => 0.08,
        _ => 0.03,
    }
}

fn plate_key(text: &str) -> String {
    text.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_uppercase()).collect()
}

fn fmt_weight(weight: f64, unit: &str) -> String {
    let rounded = weight.round() as i64;
    let digits = rounded.abs().to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{}{grouped} {}", if rounded < 0 { "-" } else { "" }, if unit == "kg" { "kg" } else { "lb" })
}

#[derive(Default)]
pub struct Engine {
    last_fired: HashMap<String, i64>,
    /// weightAbove rules over their weight now (fire again once back under).
    above: HashMap<String, bool>,
    /// unsettled, scaleOffline, cameraOffline: said for this spell already.
    said: HashMap<String, bool>,
    arrived_at: Option<i64>,
    last_frame_at: Option<i64>,
    camera_watch_since: Option<i64>,
    weight: Option<f64>,
    unit: String,
    muted_until: Option<i64>,
}

impl Engine {
    pub fn mute(&mut self, until: Option<i64>) {
        self.muted_until = until;
    }

    pub fn muted_until(&self, now: i64) -> Option<i64> {
        self.muted_until.filter(|&t| t > now)
    }

    /// The camera watcher has started (camera rules count from here).
    pub fn watching_camera(&mut self, since: Option<i64>) {
        self.camera_watch_since = since;
        self.last_frame_at = None;
    }

    /// `quiet`: whether quiet hours hold now (see `in_quiet_hours`).
    pub fn handle(&mut self, s: &AlertSettings, input: &Input, now: i64, quiet: bool) -> Vec<Fired> {
        let mut due: Vec<(&Rule, String)> = Vec::new();
        match input {
            Input::Tick { weight, scale_enabled, last_reading_at, started_at } => {
                self.weight = *weight;
                for r in s.rules.iter().filter(|r| r.enabled) {
                    match r.kind.as_str() {
                        kind::WEIGHT_ABOVE => {
                            let Some(w) = weight else { continue };
                            let was = self.above.get(&r.id).copied().unwrap_or(false);
                            // Back under by a margin before it can go off again.
                            let rearm = r.threshold - (r.threshold * 0.1).max(50.0);
                            if !was && *w >= r.threshold {
                                self.above.insert(r.id.clone(), true);
                                due.push((r, format!("{} on the scale", fmt_weight(*w, &self.unit))));
                            } else if was && *w < rearm {
                                self.above.insert(r.id.clone(), false);
                            }
                        }
                        kind::UNSETTLED => {
                            if let Some(at) = self.arrived_at {
                                let secs = (now - at) as f64 / 1000.0;
                                if secs >= r.seconds && !self.said.get(&r.id).copied().unwrap_or(false) {
                                    self.said.insert(r.id.clone(), true);
                                    due.push((r, format!("A truck has been on the scale {:.0} s without settling", secs)));
                                }
                            }
                        }
                        kind::SCALE_OFFLINE if *scale_enabled => {
                            let since = last_reading_at.unwrap_or(*started_at);
                            let quiet_for = (now - since) as f64 / 1000.0;
                            let said = self.said.get(&r.id).copied().unwrap_or(false);
                            if quiet_for >= r.seconds && !said {
                                self.said.insert(r.id.clone(), true);
                                due.push((r, format!("No readings from the scale for {:.0} s", quiet_for)));
                            } else if quiet_for < r.seconds {
                                self.said.insert(r.id.clone(), false);
                            }
                        }
                        kind::CAMERA_OFFLINE => {
                            let Some(watching) = self.camera_watch_since else { continue };
                            let since = self.last_frame_at.unwrap_or(watching);
                            let quiet_for = (now - since) as f64 / 1000.0;
                            let said = self.said.get(&r.id).copied().unwrap_or(false);
                            if quiet_for >= r.seconds && !said {
                                self.said.insert(r.id.clone(), true);
                                due.push((r, format!("The camera has not sent a picture for {:.0} s", quiet_for)));
                            } else if quiet_for < r.seconds {
                                self.said.insert(r.id.clone(), false);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Input::Arrived => {
                self.arrived_at = Some(now);
                self.clear_said(s, kind::UNSETTLED);
            }
            Input::Weighed { weight, unit } => {
                self.arrived_at = None;
                self.unit = unit.clone();
                for r in s.rules.iter().filter(|r| r.enabled && r.kind == kind::WEIGHED) {
                    let over = r.threshold <= 0.0 || *weight >= r.threshold;
                    let under = r.max_weight <= 0.0 || *weight <= r.max_weight;
                    if over && under {
                        due.push((r, format!("{} weighed", fmt_weight(*weight, unit))));
                    }
                }
            }
            Input::Cleared => {
                self.arrived_at = None;
                for r in s.rules.iter().filter(|r| r.enabled && r.kind == kind::CLEARED) {
                    due.push((r, "The scale is clear".into()));
                }
            }
            Input::Plate { text } => {
                let key = plate_key(text);
                for r in s.rules.iter().filter(|r| r.enabled && r.kind == kind::PLATE) {
                    let listed = r.plates.split(|c: char| c == ',' || c.is_whitespace()).map(plate_key).any(|p| !p.is_empty() && p == key);
                    let wanted = match r.plate_match.as_str() {
                        "listed" => listed,
                        "unlisted" => !listed,
                        _ => true,
                    };
                    if wanted {
                        due.push((r, format!("Plate {text} read")));
                    }
                }
            }
            Input::NoPlate { weight, unit } => {
                for r in s.rules.iter().filter(|r| r.enabled && r.kind == kind::NO_PLATE) {
                    due.push((r, format!("No plate read for the {} truck", fmt_weight(*weight, unit))));
                }
            }
            Input::Motion { changed } => {
                let empty = self.weight.is_none_or(|w| w < 500.0) && self.arrived_at.is_none();
                for r in s.rules.iter().filter(|r| r.enabled && r.kind == kind::MOTION) {
                    if *changed >= motion_threshold(&r.sensitivity) && (!r.only_empty_scale || empty) {
                        due.push((r, "Movement in the camera picture".into()));
                    }
                }
            }
            Input::CameraFrame => self.last_frame_at = Some(now),
        }
        if !s.enabled {
            return vec![];
        }
        due.into_iter().filter_map(|(r, message)| self.fire(s, r, message, now, quiet)).collect()
    }

    fn clear_said(&mut self, s: &AlertSettings, kind: &str) {
        for r in s.rules.iter().filter(|r| r.kind == kind) {
            self.said.insert(r.id.clone(), false);
        }
    }

    fn fire(&mut self, s: &AlertSettings, r: &Rule, message: String, now: i64, quiet: bool) -> Option<Fired> {
        if let Some(&at) = self.last_fired.get(&r.id) {
            if ((now - at) as f64) < r.cooldown_seconds * 1000.0 {
                return None;
            }
        }
        self.last_fired.insert(r.id.clone(), now);
        Some(fired(s, r, message, quiet && !r.ignore_quiet, self.muted_until(now).is_some()))
    }

    /// The Test button: the rule as it would go off now, cooldown aside.
    pub fn test(&self, s: &AlertSettings, r: &Rule, now: i64, quiet: bool) -> Fired {
        fired(s, r, format!("Test: {}", if r.name.is_empty() { "alert" } else { &r.name }), quiet && !r.ignore_quiet, self.muted_until(now).is_some())
    }
}

fn fired(s: &AlertSettings, r: &Rule, message: String, quiet: bool, muted: bool) -> Fired {
    let all_quiet = quiet && s.quiet.mode == "all";
    let silenced = if muted {
        "muted"
    } else if quiet {
        "quiet hours"
    } else {
        ""
    };
    Fired {
        rule_id: r.id.clone(),
        name: r.name.clone(),
        message,
        sound: if silenced.is_empty() { r.sound.clone() } else { "none".into() },
        sound_file: r.sound_file.clone(),
        volume: (r.volume.clamp(0.0, 100.0) / 100.0) * (s.volume.clamp(0.0, 100.0) / 100.0),
        length: r.length.clone(),
        length_seconds: r.length_seconds.clamp(1.0, 600.0),
        banner: r.banner && !all_quiet,
        notify: r.notify && s.notifications && !all_quiet && !muted,
        silenced: silenced.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(rules: Vec<Rule>) -> AlertSettings {
        AlertSettings { rules, ..AlertSettings::default() }
    }

    fn rule(id: &str, kind: &str) -> Rule {
        Rule { id: id.into(), name: id.into(), kind: kind.into(), cooldown_seconds: 0.0, ..Rule::default() }
    }

    fn tick(weight: f64) -> Input {
        Input::Tick { weight: Some(weight), scale_enabled: true, last_reading_at: Some(0), started_at: 0 }
    }

    #[test]
    fn something_on_the_scale_goes_off_once_as_the_weight_passes_and_again_after_it_leaves() {
        let s = settings(vec![Rule { threshold: 500.0, ..rule("on", kind::WEIGHT_ABOVE) }]);
        let mut e = Engine::default();
        let fired: Vec<usize> = [100.0, 300.0, 620.0, 9000.0, 48000.0, 480.0, 300.0, 700.0].iter().map(|w| e.handle(&s, &tick(*w), 0, false).len()).collect();
        // 480 is not far enough under 500 to go off again; 300 is.
        assert_eq!(fired, [0, 0, 1, 0, 0, 0, 0, 1]);
        assert_eq!(e.handle(&s, &tick(100.0), 0, false), vec![]);
    }

    #[test]
    fn weighed_can_be_limited_to_a_range_for_overweight_or_light_loads() {
        let s = settings(vec![
            rule("any", kind::WEIGHED),
            Rule { threshold: 80_000.0, ..rule("over", kind::WEIGHED) },
            Rule { max_weight: 10_000.0, ..rule("light", kind::WEIGHED) },
        ]);
        let mut e = Engine::default();
        let ids = |f: Vec<Fired>| f.into_iter().map(|f| f.rule_id).collect::<Vec<_>>();
        assert_eq!(ids(e.handle(&s, &Input::Weighed { weight: 82_400.0, unit: "lb".into() }, 0, false)), ["any", "over"]);
        assert_eq!(ids(e.handle(&s, &Input::Weighed { weight: 8_660.0, unit: "lb".into() }, 1, false)), ["any", "light"]);
        let f = e.handle(&s, &Input::Weighed { weight: 48_320.0, unit: "lb".into() }, 2, false);
        assert_eq!(f[0].message, "48,320 lb weighed");
    }

    #[test]
    fn plates_any_on_a_list_or_not_on_it() {
        let s = settings(vec![
            Rule { plate_match: "listed".into(), plates: "P865012, 44816r".into(), ..rule("listed", kind::PLATE) },
            Rule { plate_match: "unlisted".into(), plates: "P865012, 44816r".into(), ..rule("unknown", kind::PLATE) },
        ]);
        let mut e = Engine::default();
        let ids = |f: Vec<Fired>| f.into_iter().map(|f| f.rule_id).collect::<Vec<_>>();
        assert_eq!(ids(e.handle(&s, &Input::Plate { text: "44816R".into() }, 0, false)), ["listed"]);
        assert_eq!(ids(e.handle(&s, &Input::Plate { text: "223-647H".into() }, 0, false)), ["unknown"]);
    }

    #[test]
    fn a_truck_not_settling_is_said_once_after_its_seconds() {
        let s = settings(vec![Rule { seconds: 30.0, ..rule("slow", kind::UNSETTLED) }]);
        let mut e = Engine::default();
        e.handle(&s, &Input::Arrived, 0, false);
        assert!(e.handle(&s, &tick(30_000.0), 29_000, false).is_empty());
        assert_eq!(e.handle(&s, &tick(30_000.0), 31_000, false)[0].message, "A truck has been on the scale 31 s without settling");
        assert!(e.handle(&s, &tick(30_000.0), 60_000, false).is_empty());
        // Weighed in the end: nothing more. The next truck can go off again.
        e.handle(&s, &Input::Weighed { weight: 30_000.0, unit: "lb".into() }, 61_000, false);
        e.handle(&s, &Input::Arrived, 100_000, false);
        assert_eq!(e.handle(&s, &tick(1.0), 140_000, false).len(), 1);
    }

    #[test]
    fn the_scale_going_quiet_is_said_once_until_readings_come_back() {
        let s = settings(vec![Rule { seconds: 60.0, ..rule("scale", kind::SCALE_OFFLINE) }]);
        let mut e = Engine::default();
        let at = |e: &mut Engine, last: i64, now: i64| {
            e.handle(&s, &Input::Tick { weight: None, scale_enabled: true, last_reading_at: Some(last), started_at: 0 }, now, false).len()
        };
        assert_eq!(at(&mut e, 1_000, 30_000), 0);
        assert_eq!(at(&mut e, 1_000, 62_000), 1);
        assert_eq!(at(&mut e, 1_000, 120_000), 0);
        assert_eq!(at(&mut e, 125_000, 125_500), 0); // back
        assert_eq!(at(&mut e, 125_000, 190_000), 1); // gone again
    }

    #[test]
    fn camera_quiet_counts_only_while_watching() {
        let s = settings(vec![Rule { seconds: 60.0, ..rule("cam", kind::CAMERA_OFFLINE) }]);
        let mut e = Engine::default();
        assert!(e.handle(&s, &tick(0.0), 500_000, false).is_empty()); // not watching
        e.watching_camera(Some(500_000));
        e.handle(&s, &Input::CameraFrame, 501_000, false);
        assert!(e.handle(&s, &tick(0.0), 550_000, false).is_empty());
        assert_eq!(e.handle(&s, &tick(0.0), 562_000, false).len(), 1);
    }

    #[test]
    fn motion_by_sensitivity_and_only_with_the_scale_empty() {
        let s = settings(vec![Rule { sensitivity: "medium".into(), ..rule("move", kind::MOTION) }]);
        let mut e = Engine::default();
        e.handle(&s, &tick(100.0), 0, false);
        assert!(e.handle(&s, &Input::Motion { changed: 0.01 }, 0, false).is_empty()); // the clock ticking over
        assert_eq!(e.handle(&s, &Input::Motion { changed: 0.2 }, 0, false).len(), 1);
        e.handle(&s, &tick(30_000.0), 0, false);
        assert!(e.handle(&s, &Input::Motion { changed: 0.2 }, 0, false).is_empty());
        // a truck on the scale
    }

    #[test]
    fn cooldown_holds_repeats_back() {
        let s = settings(vec![Rule { cooldown_seconds: 60.0, ..rule("clear", kind::CLEARED) }]);
        let mut e = Engine::default();
        assert_eq!(e.handle(&s, &Input::Cleared, 0, false).len(), 1);
        assert_eq!(e.handle(&s, &Input::Cleared, 30_000, false).len(), 0);
        assert_eq!(e.handle(&s, &Input::Cleared, 61_000, false).len(), 1);
    }

    #[test]
    fn quiet_hours_and_mute_hold_the_sound_back_but_not_an_alarm_that_ignores_them() {
        let mut s = settings(vec![rule("weighed", kind::WEIGHED), Rule { ignore_quiet: true, ..rule("over", kind::WEIGHED) }]);
        s.quiet.mode = "sound".into();
        let mut e = Engine::default();
        let f = e.handle(&s, &Input::Weighed { weight: 1.0, unit: "lb".into() }, 0, true);
        assert_eq!((f[0].sound.as_str(), f[0].silenced.as_str(), f[0].banner), ("none", "quiet hours", true));
        assert_eq!((f[1].sound.as_str(), f[1].silenced.as_str()), ("chime", ""));
        s.quiet.mode = "all".into();
        let f = e.handle(&s, &Input::Weighed { weight: 1.0, unit: "lb".into() }, 1, true);
        assert!(!f[0].banner && !f[0].notify);
        e.mute(Some(10_000));
        let f = e.handle(&s, &Input::Weighed { weight: 1.0, unit: "lb".into() }, 2, false);
        assert_eq!((f[0].sound.as_str(), f[0].silenced.as_str()), ("none", "muted"));
        let f = e.handle(&s, &Input::Weighed { weight: 1.0, unit: "lb".into() }, 20_000, false);
        assert_eq!(f[0].sound, "chime"); // the mute has run out
                                         // Volume: the rule's times the master.
        s.volume = 50.0;
        s.rules[0].volume = 80.0;
        assert_eq!(e.handle(&s, &Input::Weighed { weight: 1.0, unit: "lb".into() }, 30_000, false)[0].volume, 0.4);
        // All off: nothing.
        s.enabled = false;
        assert!(e.handle(&s, &Input::Weighed { weight: 1.0, unit: "lb".into() }, 40_000, false).is_empty());
    }

    #[test]
    fn quiet_hours_across_midnight_and_on_chosen_days() {
        let q = Quiet { enabled: true, from: "22:00".into(), to: "06:00".into(), days: vec![], mode: "sound".into() };
        assert!(in_quiet_hours(&q, 3, 23 * 60));
        assert!(in_quiet_hours(&q, 3, 5 * 60));
        assert!(!in_quiet_hours(&q, 3, 12 * 60));
        // Weekends only (Saturday 6, Sunday 0): Friday night is not, Saturday night into Sunday is.
        let weekend = Quiet { days: vec![6, 0], ..q.clone() };
        assert!(!in_quiet_hours(&weekend, 5, 23 * 60));
        assert!(in_quiet_hours(&weekend, 6, 23 * 60));
        assert!(in_quiet_hours(&weekend, 0, 3 * 60));
        assert!(in_quiet_hours(&weekend, 1, 3 * 60)); // Sunday night into Monday morning: Sunday's
        assert!(!in_quiet_hours(&weekend, 2, 3 * 60)); // Monday night into Tuesday: not
        let day = Quiet { from: "12:00".into(), to: "13:00".into(), ..q };
        assert!(in_quiet_hours(&day, 2, 12 * 60 + 30));
        assert!(!in_quiet_hours(&Quiet { enabled: false, ..day }, 2, 12 * 60 + 30));
    }

    #[test]
    fn settings_from_an_older_file_take_the_defaults() {
        let s: AlertSettings = serde_json::from_value(json!({ "volume": 40 })).unwrap();
        assert_eq!(s.volume, 40.0);
        assert!(s.enabled);
        assert_eq!(s.rules.len(), default_rules().len());
        let r: Rule = serde_json::from_value(json!({ "id": "x", "kind": "plate" })).unwrap();
        assert_eq!((r.plate_match.as_str(), r.sound.as_str()), ("any", "chime"));
    }
}
