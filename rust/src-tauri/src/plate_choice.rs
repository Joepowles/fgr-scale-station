//! Which of the reader's finds is the truck's plate.
//!
//! The reader is run on the whole frame, so it hands back everything
//! plate-shaped in the yard: the truck on the deck, a truck parked behind it,
//! a dealer placard in the plate holder. Two steps pick the one wanted:
//!
//! 1. Signage is dropped (as the Electron app's plateFilter.js does):
//!    - a commercial plate has at least one digit; a word has none
//!    - a plate is 4 to 8 characters once spaces and dashes are gone
//!    - words on the yard's own ignore list are signage
//!    - the same text found in two places in one picture is signage: a truck
//!      carries one rear plate, but a placard is on both mud flaps
//!
//! 2. Of what is left and read well enough, the biggest plate wins: the
//!    truck on the scale is the one nearest the camera. The scale camera
//!    looks along the deck from one side, so plates toward that side of the
//!    picture can be favoured too - a plate at the favoured edge counts as up
//!    to `favorPercent` bigger than the same plate at the other edge.
//!    Picking the most confident read instead is a setting, for a camera
//!    where size says nothing.

use serde::Deserialize;

use crate::plates::PlateRead;

const MIN_LENGTH: usize = 4;
const MAX_LENGTH: usize = 8;

/// The plates settings this needs, read from the settings' "plates" section.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Rules {
    pub require_digit: bool,
    pub ignore_words: String,
    pub min_confidence: f64,
    /// "largest" or "confident".
    pub pick: String,
    /// "right", "left" or "none".
    pub favor_side: String,
    pub favor_percent: f64,
}

impl Default for Rules {
    fn default() -> Self {
        Self { require_digit: true, ignore_words: String::new(), min_confidence: 0.6, pick: "largest".into(), favor_side: "right".into(), favor_percent: 50.0 }
    }
}

/// What came of one picture: the plate taken, and when nothing was, the best
/// plate-like read that fell under the confidence floor (for the log).
#[derive(Debug, Default)]
pub struct Choice {
    pub plate: Option<PlateRead>,
    pub under_floor: Option<PlateRead>,
}

pub fn normalize(text: &str) -> String {
    text.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_uppercase()).collect()
}

fn ignore_list(words: &str) -> Vec<String> {
    words.split(|c: char| c == ',' || c.is_whitespace()).map(normalize).filter(|w| !w.is_empty()).collect()
}

/// Does this read look like a plate at all?
pub fn looks_like_plate(text: &str, rules: &Rules) -> bool {
    let t = normalize(text);
    if t.len() < MIN_LENGTH || t.len() > MAX_LENGTH {
        return false;
    }
    if rules.require_digit && !t.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    !ignore_list(&rules.ignore_words).contains(&t)
}

/// How much a plate's place across the picture adds to it: 1 at the edge
/// not favoured, up to 1 + favorPercent/100 at the favoured one.
fn side_weight(plate: &PlateRead, width: f64, rules: &Rules) -> f64 {
    if width <= 0.0 {
        return 1.0;
    }
    let [x1, _, x2, _] = plate.bounds;
    let across = ((x1 + x2) as f64 / 2.0 / width).clamp(0.0, 1.0);
    let toward = match rules.favor_side.as_str() {
        "right" => across,
        "left" => 1.0 - across,
        _ => return 1.0,
    };
    1.0 + rules.favor_percent.clamp(0.0, 1000.0) / 100.0 * toward
}

fn area(plate: &PlateRead) -> f64 {
    let [x1, y1, x2, y2] = plate.bounds;
    ((x2 - x1).max(0) * (y2 - y1).max(0)) as f64
}

/// How strongly this plate is wanted, by the rule chosen.
pub fn score(plate: &PlateRead, width: f64, rules: &Rules) -> f64 {
    let size_or_confidence = if rules.pick == "confident" { plate.confidence } else { area(plate) };
    size_or_confidence * side_weight(plate, width, rules)
}

/// The truck's plate among the reader's finds in a picture `width` pixels wide.
pub fn choose(plates: &[PlateRead], width: f64, rules: &Rules) -> Choice {
    let candidates: Vec<&PlateRead> = plates.iter().filter(|p| looks_like_plate(&p.text, rules)).collect();
    let single: Vec<&PlateRead> =
        candidates.iter().copied().filter(|p| candidates.iter().filter(|q| normalize(&q.text) == normalize(&p.text)).count() == 1).collect();
    let best = |list: Vec<&PlateRead>| list.into_iter().max_by(|a, b| score(a, width, rules).total_cmp(&score(b, width, rules))).cloned();
    let (good, weak): (Vec<&PlateRead>, Vec<&PlateRead>) = single.into_iter().partition(|p| p.confidence >= rules.min_confidence);
    let plate = best(good);
    Choice { under_floor: if plate.is_none() { best(weak) } else { None }, plate }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str, confidence: f64, bounds: [i64; 4]) -> PlateRead {
        PlateRead { text: text.into(), confidence, char_confidence: vec![], det_confidence: 0.8, bounds }
    }

    const WIDTH: f64 = 2560.0;

    #[test]
    fn the_biggest_plate_wins_over_a_more_confident_small_one() {
        // The truck on the deck, near the camera; a parked truck far behind it.
        let on_deck = read("P865012", 0.81, [1500, 900, 1740, 960]);
        let parked = read("4KD2210", 0.97, [300, 700, 360, 715]);
        let choice = choose(&[parked, on_deck], WIDTH, &Rules::default());
        assert_eq!(choice.plate.unwrap().text, "P865012");
    }

    #[test]
    fn the_favoured_side_tips_two_plates_of_a_size() {
        let left = read("LEFT123", 0.9, [100, 500, 300, 550]);
        let right = read("RIGHT12", 0.9, [2200, 500, 2380, 550]); // a tenth smaller
        assert_eq!(choose(&[left.clone(), right.clone()], WIDTH, &Rules::default()).plate.unwrap().text, "RIGHT12");
        let no_side = Rules { favor_side: "none".into(), ..Rules::default() };
        assert_eq!(choose(&[left.clone(), right.clone()], WIDTH, &no_side).plate.unwrap().text, "LEFT123");
        let left_side = Rules { favor_side: "left".into(), ..Rules::default() };
        assert_eq!(choose(&[left, right], WIDTH, &left_side).plate.unwrap().text, "LEFT123");
    }

    #[test]
    fn a_much_bigger_plate_wins_even_on_the_side_not_favoured() {
        let big_left = read("BIG1234", 0.9, [100, 500, 500, 600]);
        let small_right = read("SML1234", 0.9, [2300, 500, 2400, 525]);
        assert_eq!(choose(&[big_left, small_right], WIDTH, &Rules::default()).plate.unwrap().text, "BIG1234");
    }

    #[test]
    fn most_confident_is_a_choice() {
        let big = read("BIG1234", 0.7, [100, 500, 500, 600]);
        let sure = read("SURE123", 0.95, [1000, 500, 1050, 515]);
        let rules = Rules { pick: "confident".into(), favor_side: "none".into(), ..Rules::default() };
        assert_eq!(choose(&[big, sure], WIDTH, &rules).plate.unwrap().text, "SURE123");
    }

    #[test]
    fn a_read_under_the_floor_is_not_taken_but_is_reported() {
        let weak = read("P865012", 0.4, [1500, 900, 1740, 960]);
        let choice = choose(&[weak], WIDTH, &Rules::default());
        assert!(choice.plate.is_none());
        assert_eq!(choice.under_floor.unwrap().text, "P865012");
    }

    #[test]
    fn a_big_weak_read_does_not_hide_a_smaller_good_one() {
        let weak_big = read("P865012", 0.4, [1500, 900, 1740, 960]);
        let good_small = read("4KD2210", 0.9, [300, 700, 360, 715]);
        assert_eq!(choose(&[weak_big, good_small], WIDTH, &Rules::default()).plate.unwrap().text, "4KD2210");
    }

    #[test]
    fn signage_is_never_the_plate() {
        let rules = Rules { ignore_words: "KENWORTH, ruble".into(), ..Rules::default() };
        assert!(!looks_like_plate("RUBLE", &rules)); // listed, and no digit either
        assert!(!looks_like_plate("SALES", &rules)); // no digit
        assert!(!looks_like_plate("AB1", &rules)); // too short
        assert!(!looks_like_plate("AB12345678", &rules)); // too long
        assert!(looks_like_plate("p86-5012", &rules));
        assert!(looks_like_plate("SALES", &Rules { require_digit: false, ..Rules::default() }));
        // The same text on both mud flaps is a placard, whatever its size.
        let flaps = [read("TX4455", 0.95, [1000, 900, 1400, 1000]), read("TX4455", 0.95, [1800, 900, 2200, 1000])];
        let plate = read("P865012", 0.8, [1500, 950, 1600, 975]);
        let mut all = flaps.to_vec();
        all.push(plate);
        assert_eq!(choose(&all, WIDTH, &rules).plate.unwrap().text, "P865012");
    }

    #[test]
    fn settings_missing_from_an_older_file_take_the_defaults() {
        let rules: Rules = serde_json::from_value(serde_json::json!({ "minConfidence": 0.7, "enabled": true })).unwrap();
        assert_eq!(rules.min_confidence, 0.7);
        assert_eq!(rules.pick, "largest");
        assert_eq!(rules.favor_side, "right");
        assert!(rules.require_digit);
    }
}
