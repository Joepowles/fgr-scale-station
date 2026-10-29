//! Two weighings of the same truck less than an hour apart are one load: in
//! loaded and out empty, or the other way round. The heavier is the gross,
//! the lighter the tare, and the difference the net. Both entries carry the
//! pair, so either one shows all three:
//!
//! ```text
//! "pair": { "with": "<the other entry's id>", "role": "gross",
//!           "gross": 80120, "tare": 31460, "net": 48660,
//!           "by": "plate", "match": "7KX2210", "minutes": 23 }
//! ```
//!
//! What says two weighings are the same truck is a `Matcher`. Today that is
//! the plate read (`PlateMatch`); a truck's appearance (the fingerprint being
//! prototyped from the yard's sample pictures) is meant to be another one,
//! added to `matchers()`, and pairs the same way. The first matcher that
//! recognises the truck names the pair.
//!
//! A weighing is in one pair at most. A truck that comes back again within
//! the hour starts a new pair with its next weighing, rather than taking one
//! from the pair before. Of the weighings that could pair, the nearest in
//! time does. Pairs are worked out again whenever an entry's plate or weight
//! changes, so a plate corrected by a later frame can make a pair or undo one.

use chrono::DateTime;
use serde_json::{json, Value};

use crate::history::{weight_value, Entry, History};
use crate::plate_choice;

/// Less than this apart.
pub const WITHIN_MS: i64 = 60 * 60 * 1000;

/// Says whether two weighings are of the same truck.
pub trait Matcher: Send + Sync {
    /// For the entry and the log: "plate", "fingerprint".
    fn name(&self) -> &'static str;
    /// What the two have in common (the plate, say), when they are the same truck.
    fn same_truck(&self, a: &Entry, b: &Entry) -> Option<String>;
}

/// The same plate, however it was written ("P86-5012" is "P865012").
pub struct PlateMatch;

impl Matcher for PlateMatch {
    fn name(&self) -> &'static str {
        "plate"
    }

    fn same_truck(&self, a: &Entry, b: &Entry) -> Option<String> {
        let plate = |e: &Entry| e.get("plate")?.get("text")?.as_str().map(plate_choice::normalize).filter(|p| !p.is_empty());
        let (a, b) = (plate(a)?, plate(b)?);
        (a == b).then_some(a)
    }
}

/// The matchers in use, surest first.
pub fn matchers() -> Vec<Box<dyn Matcher>> {
    vec![Box::new(PlateMatch)]
}

fn id(e: &Entry) -> Option<&str> {
    e.get("id")?.as_str()
}

fn at_ms(e: &Entry) -> Option<i64> {
    Some(DateTime::parse_from_rfc3339(e.get("at")?.as_str()?).ok()?.timestamp_millis())
}

fn weight(e: &Entry) -> Option<f64> {
    e.get("weight")?.as_f64()
}

fn unit(e: &Entry) -> &str {
    e.get("unit").and_then(Value::as_str).unwrap_or("lb")
}

fn partner(e: &Entry) -> Option<&str> {
    e.get("pair")?.get("with")?.as_str()
}

fn weighing(e: &Entry) -> bool {
    e.get("type").and_then(Value::as_str).unwrap_or("weighing") == "weighing" && weight(e).is_some() && at_ms(e).is_some()
}

/// A pair worked out for an entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Pair {
    pub with: String,
    pub by: &'static str,
    pub matched: String,
}

/// The weighing `entry` pairs with, from `entries`: the same truck by one of
/// the matchers, under an hour apart, in no other pair, and a different weight.
pub fn find(entries: &[Entry], entry: &Entry, matchers: &[Box<dyn Matcher>]) -> Option<Pair> {
    let own = id(entry)?;
    if !weighing(entry) {
        return None;
    }
    let (at, w) = (at_ms(entry)?, weight(entry)?);
    entries
        .iter()
        .filter(|c| id(c).is_some_and(|c| c != own) && weighing(c) && unit(c) == unit(entry))
        .filter(|c| partner(c).is_none_or(|p| p == own))
        .filter(|c| weight(c) != Some(w))
        .filter_map(|c| {
            let apart = (at_ms(c)? - at).abs();
            if apart >= WITHIN_MS {
                return None;
            }
            let (by, matched) = matchers.iter().find_map(|m| m.same_truck(entry, c).map(|what| (m.name(), what)))?;
            Some((apart, Pair { with: id(c)?.to_string(), by, matched }))
        })
        .min_by_key(|(apart, _)| *apart)
        .map(|(_, pair)| pair)
}

/// The `pair` field for `e`, paired with `other`.
fn pair_field(e: &Entry, other: &Entry, by: &str, matched: &str) -> Value {
    let (w, ow) = (weight(e).unwrap_or_default(), weight(other).unwrap_or_default());
    let (gross, tare) = if w > ow { (w, ow) } else { (ow, w) };
    let minutes = ((at_ms(e).unwrap_or_default() - at_ms(other).unwrap_or_default()).abs() as f64 / 60_000.0).round();
    json!({
        "with": id(other),
        "role": if w > ow { "gross" } else { "tare" },
        "gross": weight_value(gross),
        "tare": weight_value(tare),
        "net": weight_value(gross - tare),
        "by": by,
        "match": matched,
        "minutes": minutes as i64,
    })
}

/// The changes to make to the history so `entry_id`'s pair is right: the
/// patches, in the order to write them (the entry itself last), and the pair
/// when there is one.
pub fn plan(entries: &[Entry], entry_id: &str, matchers: &[Box<dyn Matcher>]) -> (Vec<(String, Entry)>, Option<Pair>) {
    let by_id = |want: &str| entries.iter().find(|e| id(e) == Some(want));
    let Some(entry) = by_id(entry_id) else { return (Vec::new(), None) };
    let found = find(entries, entry, matchers);
    let mut patches = Vec::new();
    let mut set = |e: &Entry, pair: Value| {
        if e.get("pair").unwrap_or(&Value::Null) != &pair {
            let mut patch = Entry::new();
            patch.insert("pair".into(), pair);
            patches.push((id(e).unwrap_or_default().to_string(), patch));
        }
    };
    // The one it was paired with before, if that is not the one now.
    if let Some(old) = partner(entry).filter(|old| found.as_ref().is_none_or(|f| f.with != *old)).and_then(by_id) {
        if partner(old) == Some(entry_id) {
            set(old, Value::Null);
        }
    }
    match found.as_ref().and_then(|f| Some((f, by_id(&f.with)?))) {
        Some((pair, other)) => {
            set(other, pair_field(other, entry, pair.by, &pair.matched));
            set(entry, pair_field(entry, other, pair.by, &pair.matched));
        }
        None => set(entry, Value::Null),
    }
    (patches, found)
}

/// Works out `entry_id`'s pair and writes it to both entries. Returns the
/// pair's line for the log when a pair was made or changed.
pub fn update(history: &History, entry_id: &str) -> std::io::Result<Option<String>> {
    let entries = history.read();
    let (patches, pair) = plan(&entries, entry_id, &matchers());
    if patches.is_empty() {
        return Ok(None);
    }
    let mut written = None;
    for (id, patch) in &patches {
        let entry = history.update(id, patch, None)?;
        if id == entry_id {
            written = entry;
        }
    }
    let line = match (pair, written.as_ref().and_then(|e| e.get("pair")).filter(|p| !p.is_null())) {
        (Some(pair), Some(field)) => {
            let unit = written.as_ref().map_or("lb", unit);
            let w = |key: &str| format!("{} {unit}", field[key]);
            format!(
                "paired with {} ({} {}, {} min apart): gross {}, tare {}, net {}",
                pair.with,
                pair.by,
                pair.matched,
                field["minutes"],
                w("gross"),
                w("tare"),
                w("net")
            )
        }
        _ => "no longer paired".to_string(),
    };
    Ok(Some(line))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, at: &str, weight: f64, plate: Option<&str>) -> Entry {
        let plate = plate.map(|p| json!({ "text": p, "confidence": 0.9 }));
        json!({ "id": id, "at": at, "type": "weighing", "weight": weight_value(weight), "unit": "lb", "plate": plate, "note": "" }).as_object().unwrap().clone()
    }

    /// Applies a plan, the way the history would.
    fn apply(entries: &mut [Entry], id: &str) -> Option<Pair> {
        let (patches, pair) = plan(entries, id, &matchers());
        for (target, patch) in patches {
            entries.iter_mut().find(|e| e["id"] == target.as_str()).unwrap().extend(patch);
        }
        pair
    }

    #[test]
    fn pairs_two_weighings_of_one_plate_under_an_hour_apart() {
        let mut entries = vec![
            entry("in", "2026-10-29T10:00:00Z", 80120.0, Some("7KX2210")),
            entry("other", "2026-10-29T10:10:00Z", 52000.0, Some("P865012")),
            entry("out", "2026-10-29T10:23:00Z", 31460.0, Some("7KX-2210")),
        ];
        let pair = apply(&mut entries, "out").unwrap();
        assert_eq!((pair.with.as_str(), pair.by, pair.matched.as_str()), ("in", "plate", "7KX2210"));
        let (gross, tare) = (&entries[0]["pair"], &entries[2]["pair"]);
        assert_eq!(gross["role"], "gross");
        assert_eq!(gross["with"], "out");
        assert_eq!(tare["role"], "tare");
        assert_eq!(tare["with"], "in");
        for p in [gross, tare] {
            assert_eq!((p["gross"].clone(), p["tare"].clone(), p["net"].clone()), (json!(80120), json!(31460), json!(48660)));
            assert_eq!(p["minutes"], 23);
        }
        assert!(entries[1].get("pair").is_none());
        // Nothing more to do the second time.
        assert!(plan(&entries, "out", &matchers()).0.is_empty());
    }

    #[test]
    fn an_hour_or_more_apart_is_not_a_pair() {
        let mut entries = vec![entry("in", "2026-10-29T10:00:00Z", 80120.0, Some("7KX2210")), entry("out", "2026-10-29T11:00:00Z", 31460.0, Some("7KX2210"))];
        assert_eq!(apply(&mut entries, "out"), None);
        assert!(entries.iter().all(|e| e.get("pair").is_none()));
    }

    #[test]
    fn needs_a_plate_on_both_and_a_different_weight() {
        let entries = vec![
            entry("a", "2026-10-29T10:00:00Z", 80120.0, None),
            entry("b", "2026-10-29T10:05:00Z", 31460.0, None),
            entry("c", "2026-10-29T10:06:00Z", 31460.0, Some("7KX2210")),
            entry("d", "2026-10-29T10:07:00Z", 31460.0, Some("7KX2210")),
        ];
        assert_eq!(plan(&entries, "b", &matchers()).1, None);
        assert_eq!(plan(&entries, "d", &matchers()).1, None);
    }

    #[test]
    fn the_nearest_in_time_pairs_and_a_paired_weighing_is_taken() {
        let mut entries = vec![
            entry("first-in", "2026-10-29T09:30:00Z", 79000.0, Some("7KX2210")),
            entry("first-out", "2026-10-29T09:50:00Z", 31400.0, Some("7KX2210")),
            entry("second-in", "2026-10-29T10:10:00Z", 80100.0, Some("7KX2210")),
        ];
        apply(&mut entries, "first-out");
        assert_eq!(entries[0]["pair"]["with"], "first-out");
        // Back for another load: first-out is nearer, but taken.
        assert_eq!(apply(&mut entries, "second-in"), None);
        entries.push(entry("second-out", "2026-10-29T10:30:00Z", 31500.0, Some("7KX2210")));
        assert_eq!(apply(&mut entries, "second-out").unwrap().with, "second-in");
        assert_eq!(entries[2]["pair"]["net"], 48600);
        assert_eq!(entries[1]["pair"]["with"], "first-in");
    }

    #[test]
    fn a_corrected_plate_undoes_the_pair_and_a_new_weight_redoes_the_numbers() {
        let mut entries = vec![entry("in", "2026-10-29T10:00:00Z", 80120.0, Some("7KX2210")), entry("out", "2026-10-29T10:20:00Z", 31460.0, Some("7KX2210"))];
        apply(&mut entries, "out");
        entries[0]["weight"] = json!(80500);
        apply(&mut entries, "in");
        assert_eq!(entries[1]["pair"]["net"], 49040);
        entries[1]["plate"] = json!({ "text": "TKX2210", "confidence": 0.95 });
        assert_eq!(apply(&mut entries, "out"), None);
        assert!(entries.iter().all(|e| e["pair"].is_null()));
    }

    #[test]
    fn another_matcher_can_pair_trucks_with_no_plate() {
        struct Painted;
        impl Matcher for Painted {
            fn name(&self) -> &'static str {
                "fingerprint"
            }
            fn same_truck(&self, a: &Entry, b: &Entry) -> Option<String> {
                let look = |e: &Entry| e.get("vehicle")?.get("look")?.as_str().map(String::from);
                look(a).filter(|l| Some(l) == look(b).as_ref())
            }
        }
        let with_look = |id, at, w| {
            let mut e = entry(id, at, w, None);
            e.insert("vehicle".into(), json!({ "look": "blue-cab" }));
            e
        };
        let entries = vec![with_look("in", "2026-10-29T10:00:00Z", 80120.0), with_look("out", "2026-10-29T10:20:00Z", 31460.0)];
        let both: Vec<Box<dyn Matcher>> = vec![Box::new(PlateMatch), Box::new(Painted)];
        let (patches, pair) = plan(&entries, "out", &both);
        assert_eq!(pair.unwrap().by, "fingerprint");
        assert_eq!(patches.len(), 2);
        assert_eq!(patches[0].1["pair"]["by"], "fingerprint");
    }

    #[test]
    fn kg_and_lb_do_not_pair() {
        let mut out = entry("out", "2026-10-29T10:20:00Z", 14270.0, Some("7KX2210"));
        out.insert("unit".into(), json!("kg"));
        let entries = vec![entry("in", "2026-10-29T10:00:00Z", 80120.0, Some("7KX2210")), out];
        assert_eq!(plan(&entries, "out", &matchers()).1, None);
    }
}
