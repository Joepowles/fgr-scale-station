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
//! A truck can come back again and again: four loads dumped in the yard are
//! eight weighings of one plate inside the hour. So a weighing is not simply
//! paired with the nearest one. Instead:
//!
//! - The truck's weighings are taken together, as runs: weighings less than
//!   an hour apart are in one run.
//! - Each is empty or loaded. Empty is within `EMPTY_BAND` of the lightest
//!   the truck has weighed in the history; anything heavier is loaded. Two
//!   loaded weighings never pair, so a missed or misread empty weighing in
//!   the middle does not make a "load" of the difference between two loads.
//! - In a run, each loaded weighing pairs with the empty weighing next to
//!   it: the one after it (in loaded, out empty) or the one before it (in
//!   empty, out loaded), whichever way round pairs more of the run.
//! - A loaded weighing left over (its empty weighing was missed, or the
//!   driver tares once for several loads) takes its tare from the nearest
//!   empty weighing of the truck under an hour away, though that one is in a
//!   pair already. Its pair says `"shared": true`.
//!
//! Pairs are worked out again whenever an entry's plate or weight changes,
//! so a plate corrected by a later frame can make a pair or undo one. Only
//! the run the changed weighing is in is rewritten: older runs keep their
//! pairs.

use chrono::DateTime;
use serde_json::{json, Value};

use crate::history::{weight_value, Entry, History};
use crate::{plate_choice, trucks};

/// Less than this apart.
pub const WITHIN_MS: i64 = 60 * 60 * 1000;
/// Up to this much over the truck's lightest weighing is empty: fuel, the
/// driver, mud. 5% of a 31,000 lb truck is about 1,500 lb.
pub const EMPTY_BAND: f64 = 0.05;

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

/// The same name ("Hoffman/Steve"), given by the operator or found by the
/// exact plate (not a guess from a similar one), for
/// a truck with no plate read on at least one of its weighings. Two plates
/// that differ are two trucks, whatever they are called: a driver may move
/// between them.
pub struct NameMatch;

impl Matcher for NameMatch {
    fn name(&self) -> &'static str {
        "name"
    }

    fn same_truck(&self, a: &Entry, b: &Entry) -> Option<String> {
        if trucks::plate_of(a).is_some() && trucks::plate_of(b).is_some() {
            return None;
        }
        let name = |e: &Entry| trucks::sure(e).then(|| trucks::name_of(e)).flatten().map(str::to_lowercase);
        (name(a)? == name(b)?).then(|| trucks::name_of(a).unwrap_or_default().to_string())
    }
}

/// The matchers in use, surest first.
pub fn matchers() -> Vec<Box<dyn Matcher>> {
    vec![Box::new(PlateMatch), Box::new(NameMatch)]
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

fn same_truck(matchers: &[Box<dyn Matcher>], a: &Entry, b: &Entry) -> Option<(&'static str, String)> {
    matchers.iter().find_map(|m| m.same_truck(a, b).map(|what| (m.name(), what)))
}

/// A pair worked out for an entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Pair {
    pub with: String,
    pub by: &'static str,
    pub matched: String,
}

/// The `pair` field for `e`, paired with `other`.
fn pair_field(e: &Entry, other: &Entry, by: &str, matched: &str, shared: bool) -> Value {
    let (w, ow) = (weight(e).unwrap_or_default(), weight(other).unwrap_or_default());
    let (gross, tare) = if w > ow { (w, ow) } else { (ow, w) };
    let minutes = ((at_ms(e).unwrap_or_default() - at_ms(other).unwrap_or_default()).abs() as f64 / 60_000.0).round();
    let mut field = json!({
        "with": id(other),
        "role": if w > ow { "gross" } else { "tare" },
        "gross": weight_value(gross),
        "tare": weight_value(tare),
        "net": weight_value(gross - tare),
        "by": by,
        "match": matched,
        "minutes": minutes as i64,
    });
    if shared {
        field["shared"] = true.into();
    }
    field
}

/// The truck's weighings, oldest first: `entry` and every weighing a
/// matcher says is the same truck, in the same unit.
fn truck<'a>(entries: &'a [Entry], entry: &'a Entry, matchers: &[Box<dyn Matcher>]) -> Vec<&'a Entry> {
    if !weighing(entry) {
        return Vec::new();
    }
    let mut truck: Vec<&Entry> =
        entries.iter().filter(|c| weighing(c) && unit(c) == unit(entry)).filter(|c| id(c) == id(entry) || same_truck(matchers, entry, c).is_some()).collect();
    truck.sort_by_key(|e| at_ms(e));
    truck
}

/// Pairs within one run (oldest first, under an hour between neighbours):
/// for each weighing, its partner's index and whether that tare is shared.
fn pair_run(run: &[&Entry], empty: &[bool]) -> Vec<Option<(usize, bool)>> {
    let n = run.len();
    let close = |a: usize, b: usize| (at_ms(run[a]).unwrap_or_default() - at_ms(run[b]).unwrap_or_default()).abs() < WITHIN_MS;
    // Neighbours one way round: loaded then empty (`loaded_first`), or empty then loaded.
    let neighbours = |loaded_first: bool| {
        let mut out = vec![None; n];
        let mut i = 0;
        while i + 1 < n {
            if empty[i] != loaded_first && empty[i + 1] == loaded_first && close(i, i + 1) {
                out[i] = Some((i + 1, false));
                out[i + 1] = Some((i, false));
                i += 2;
            } else {
                i += 1;
            }
        }
        out
    };
    let (a, b) = (neighbours(true), neighbours(false));
    let count = |p: &[Option<(usize, bool)>]| p.iter().flatten().count();
    let first = |p: &[Option<(usize, bool)>]| p.iter().position(Option::is_some).unwrap_or(n);
    // More pairs wins; then the one whose first pair starts sooner.
    let mut pairs = if (count(&b), first(&a)) > (count(&a), first(&b)) { b } else { a };
    // A load with no empty weighing beside it takes the nearest one's tare.
    for i in 0..n {
        if empty[i] || pairs[i].is_some() {
            continue;
        }
        let at = at_ms(run[i]).unwrap_or_default();
        let nearest = (0..n).filter(|&j| empty[j] && close(i, j)).min_by_key(|&j| (at_ms(run[j]).unwrap_or_default() - at).abs());
        if let Some(j) = nearest {
            pairs[i] = Some((j, true));
        }
    }
    pairs
}

/// The changes to make to the history so the pairs of `entry_id`'s truck are
/// right around it: the patches, in the order to write them (the entry itself
/// last), and the entry's pair when it has one.
pub fn plan(entries: &[Entry], entry_id: &str, matchers: &[Box<dyn Matcher>]) -> (Vec<(String, Entry)>, Option<Pair>) {
    let by_id = |want: &str| entries.iter().find(|e| id(e) == Some(want));
    let Some(entry) = by_id(entry_id) else { return (Vec::new(), None) };
    let mut fields: Vec<(&Entry, Value)> = Vec::new();
    let mut found = None;
    // The truck it was paired with before, if that is not this one now.
    let old = partner(entry).and_then(by_id).filter(|old| same_truck(matchers, entry, old).is_none());
    for anchor in [old, Some(entry)].into_iter().flatten() {
        let truck = truck(entries, anchor, matchers);
        let Some(here) = truck.iter().position(|e| id(e) == id(anchor)) else { continue };
        let at = |i: usize| at_ms(truck[i]).unwrap_or_default();
        // The run around it.
        let (mut start, mut end) = (here, here);
        while start > 0 && at(start) - at(start - 1) < WITHIN_MS {
            start -= 1;
        }
        while end + 1 < truck.len() && at(end + 1) - at(end) < WITHIN_MS {
            end += 1;
        }
        let run = &truck[start..=end];
        let lightest = truck.iter().filter_map(|e| weight(e)).fold(f64::INFINITY, f64::min);
        let empty: Vec<bool> = run.iter().map(|e| weight(e).unwrap_or_default() <= lightest * (1.0 + EMPTY_BAND)).collect();
        for (i, pair) in pair_run(run, &empty).into_iter().enumerate() {
            let e = run[i];
            let field = match pair {
                Some((j, shared)) => {
                    let (by, matched) = same_truck(matchers, e, run[j]).unwrap_or(("plate", String::new()));
                    if id(e) == Some(entry_id) {
                        found = Some(Pair { with: id(run[j]).unwrap_or_default().to_string(), by, matched: matched.clone() });
                    }
                    pair_field(e, run[j], by, &matched, shared)
                }
                None => Value::Null,
            };
            fields.push((e, field));
        }
    }
    if !fields.iter().any(|(e, _)| id(e) == Some(entry_id)) {
        fields.push((entry, Value::Null));
    }
    let mut patches: Vec<(String, Entry)> = fields
        .into_iter()
        .filter(|(e, field)| e.get("pair").unwrap_or(&Value::Null) != field)
        .map(|(e, field)| {
            let mut patch = Entry::new();
            patch.insert("pair".into(), field);
            (id(e).unwrap_or_default().to_string(), patch)
        })
        .collect();
    patches.sort_by_key(|(id, _)| id == entry_id);
    (patches, found)
}

/// Works out the pairs around `entry_id` and writes them. Returns the
/// entry's line for the log when its pair was made or changed.
pub fn update(history: &History, entry_id: &str) -> std::io::Result<Option<String>> {
    let entries = history.read();
    let (patches, pair) = plan(&entries, entry_id, &matchers());
    for (id, patch) in &patches {
        history.update(id, patch, None)?;
    }
    let Some((_, patch)) = patches.iter().find(|(id, _)| id == entry_id) else { return Ok(None) };
    let field = &patch["pair"];
    let line = match pair {
        Some(pair) if !field.is_null() => {
            let unit = entries.iter().find(|e| id(e) == Some(entry_id)).map_or("lb", unit);
            let w = |key: &str| format!("{} {unit}", field[key]);
            format!(
                "paired with {} ({} {}, {} min apart{}): gross {}, tare {}, net {}",
                pair.with,
                pair.by,
                pair.matched,
                field["minutes"],
                if field["shared"] == true { ", tare shared" } else { "" },
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

    fn named(mut e: Entry, name: &str, by: &str) -> Entry {
        e.insert("nickname".into(), json!({ "name": name, "by": by }));
        e
    }

    #[test]
    fn a_name_pairs_a_truck_with_no_plate() {
        let mut entries = vec![
            named(entry("in", "2026-10-29T10:00:00Z", 80120.0, Some("7KX2210")), "Hoffman/Steve", "plate"),
            named(entry("out", "2026-10-29T10:20:00Z", 31460.0, None), "hoffman/steve", "operator"),
        ];
        let pair = apply(&mut entries, "out").unwrap();
        assert_eq!((pair.with.as_str(), pair.by, pair.matched.as_str()), ("in", "name", "hoffman/steve"));
        assert_eq!(entries[0]["pair"]["net"], json!(48660));
    }

    #[test]
    fn a_name_does_not_join_two_plates_or_trust_a_guess() {
        let two_plates = vec![
            named(entry("in", "2026-10-29T10:00:00Z", 80120.0, Some("7KX2210")), "Hoffman/Steve", "plate"),
            named(entry("out", "2026-10-29T10:20:00Z", 31460.0, Some("P865012")), "Hoffman/Steve", "plate"),
        ];
        assert_eq!(plan(&two_plates, "out", &matchers()).1, None);
        let guessed = vec![
            named(entry("in", "2026-10-29T10:00:00Z", 80120.0, Some("7KX2211")), "Hoffman/Steve", "similar plate"),
            named(entry("out", "2026-10-29T10:20:00Z", 31460.0, None), "Hoffman/Steve", "operator"),
        ];
        assert_eq!(plan(&guessed, "out", &matchers()).1, None);
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
    fn a_load_takes_the_last_tare_until_its_own_empty_weighing_comes() {
        let mut entries = vec![
            entry("first-in", "2026-10-29T09:30:00Z", 79000.0, Some("7KX2210")),
            entry("first-out", "2026-10-29T09:50:00Z", 31400.0, Some("7KX2210")),
            entry("second-in", "2026-10-29T10:10:00Z", 80100.0, Some("7KX2210")),
        ];
        apply(&mut entries, "first-out");
        assert_eq!(entries[0]["pair"]["with"], "first-out");
        // Back for another load: first-out's tare for now, shared.
        assert_eq!(apply(&mut entries, "second-in").unwrap().with, "first-out");
        assert_eq!(entries[2]["pair"]["shared"], true);
        assert_eq!(entries[1]["pair"]["with"], "first-in");
        entries.push(entry("second-out", "2026-10-29T10:30:00Z", 31500.0, Some("7KX2210")));
        assert_eq!(apply(&mut entries, "second-out").unwrap().with, "second-in");
        assert_eq!(entries[2]["pair"]["net"], 48600);
        assert!(entries[2]["pair"].get("shared").is_none());
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

/// A truck dumping load after load in the yard: in loaded, out empty, again
/// and again, the weighings arriving one at a time.
#[cfg(test)]
mod yard_runs {
    use super::*;

    const LOADS: [(&str, u32, f64); 8] = [
        ("in1", 0, 80000.0),
        ("out1", 7, 31000.0),
        ("in2", 14, 79000.0),
        ("out2", 21, 31100.0),
        ("in3", 28, 81000.0),
        ("out3", 35, 30900.0),
        ("in4", 42, 78000.0),
        ("out4", 49, 31050.0),
    ];

    fn arrive(entries: &mut Vec<Entry>, id: &str, minute: u32, weight: f64, plate: Option<&str>) {
        let at = format!("2026-10-29T10:{minute:02}:00Z");
        let plate = plate.map(|p| json!({ "text": p, "confidence": 0.9 }));
        let e = json!({ "id": id, "at": at, "type": "weighing", "weight": weight_value(weight), "unit": "lb", "plate": plate });
        entries.push(e.as_object().unwrap().clone());
        let (patches, _) = plan(entries, id, &matchers());
        for (target, patch) in patches {
            entries.iter_mut().find(|e| e["id"] == target.as_str()).unwrap().extend(patch);
        }
    }

    /// "gross+tare=net" for each load.
    fn loads(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .filter_map(|e| {
                e.get("pair").filter(|p| p["role"] == "gross").map(|p| format!("{}+{}={}", e["id"].as_str().unwrap(), p["with"].as_str().unwrap(), p["net"]))
            })
            .collect()
    }

    fn run(plate_of: impl Fn(&str) -> Option<&'static str>) -> Vec<Entry> {
        let mut entries = Vec::new();
        for (id, minute, weight) in LOADS {
            arrive(&mut entries, id, minute, weight, plate_of(id));
        }
        entries
    }

    #[test]
    fn four_loads_pair_in_with_out() {
        let entries = run(|_| Some("7KX2210"));
        assert_eq!(loads(&entries), ["in1+out1=49000", "in2+out2=47900", "in3+out3=50100", "in4+out4=46950"]);
        assert!(entries.iter().all(|e| e["pair"].get("shared").is_none()));
    }

    #[test]
    fn a_missed_empty_weighing_does_not_pair_two_loads() {
        // Before: in1 and in2 paired for a 1,000 lb "load".
        let entries = run(|id| (id != "out1").then_some("7KX2210"));
        assert_eq!(loads(&entries), ["in1+out2=48900", "in2+out2=47900", "in3+out3=50100", "in4+out4=46950"]);
        assert_eq!(entries[0]["pair"]["shared"], true);
    }

    #[test]
    fn a_missed_load_does_not_shift_the_pairs_after_it() {
        // Before: out2 went with in3, out3 with in4, and out4 was left over.
        let entries = run(|id| (id != "in2").then_some("7KX2210"));
        assert_eq!(loads(&entries), ["in1+out1=49000", "in3+out3=50100", "in4+out4=46950"]);
    }

    #[test]
    fn a_misread_corrected_later_puts_the_pairs_right() {
        let mut entries = run(|id| Some(if id == "out1" { "7KX2Z10" } else { "7KX2210" }));
        entries[1]["plate"] = json!({ "text": "7KX2210", "confidence": 0.95 });
        let (patches, _) = plan(&entries, "out1", &matchers());
        for (target, patch) in patches {
            entries.iter_mut().find(|e| e["id"] == target.as_str()).unwrap().extend(patch);
        }
        assert_eq!(loads(&entries), ["in1+out1=49000", "in2+out2=47900", "in3+out3=50100", "in4+out4=46950"]);
    }

    #[test]
    fn one_tare_serves_every_load_in_the_hour() {
        let mut entries = Vec::new();
        for (id, minute, weight) in [("tare", 0, 31000.0), ("in1", 10, 80000.0), ("in2", 20, 79000.0), ("in3", 30, 81000.0), ("in4", 40, 78000.0)] {
            arrive(&mut entries, id, minute, weight, Some("7KX2210"));
        }
        // Before: in2 and in3 paired with each other for 2,000 lb.
        assert_eq!(loads(&entries), ["in1+tare=49000", "in2+tare=48000", "in3+tare=50000", "in4+tare=47000"]);
    }

    #[test]
    fn a_truck_never_weighed_empty_has_no_net() {
        let mut entries = Vec::new();
        for (id, minute, weight) in [("in1", 0, 80000.0), ("in2", 20, 79000.0)] {
            arrive(&mut entries, id, minute, weight, Some("7KX2210"));
        }
        assert!(loads(&entries).is_empty());
    }
}
