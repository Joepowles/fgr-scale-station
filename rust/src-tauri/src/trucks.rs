//! Names the operator gives trucks ("Hoffman/Steve"), kept in trucks.json
//! beside the history. The history is dropped after its retention period;
//! the names outlast it, so a truck back next month is still known.
//!
//! A plate has one name; a name may have several plates (a misread, a driver
//! in another truck). A plate one character off a named one is only a guess:
//! the weighing shows the name with a question mark until someone confirms.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::history::Entry;
use crate::plate_choice::normalize;

pub const FILE: &str = "trucks.json";
/// Longest name kept, in characters.
const MAX_NAME: usize = 60;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    /// Normalised plate -> name.
    plates: BTreeMap<String, String>,
    /// Every name given, plates or not, for the suggestions as one types.
    names: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Book {
    path: PathBuf,
    saved: Saved,
}

/// Trimmed, single-spaced, at most MAX_NAME characters.
pub fn clean(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_NAME).collect()
}

fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// One substitution, insertion or deletion apart ("226958C" and "2269580").
fn one_off(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a == b {
        return false;
    }
    match a.len() as i64 - b.len() as i64 {
        0 => a.iter().zip(&b).filter(|(x, y)| x != y).count() == 1,
        1 | -1 => {
            let (long, short) = if a.len() > b.len() { (&a, &b) } else { (&b, &a) };
            (0..long.len()).any(|skip| long.iter().enumerate().filter(|(i, _)| *i != skip).map(|(_, c)| c).eq(short.iter()))
        }
        _ => false,
    }
}

pub fn plate_of(entry: &Entry) -> Option<String> {
    entry.get("plate")?.get("text")?.as_str().map(normalize).filter(|p| !p.is_empty())
}

/// The name on an entry, however it got there.
pub fn name_of(entry: &Entry) -> Option<&str> {
    entry.get("nickname")?.get("name")?.as_str()
}

/// Was it given or confirmed by the operator, rather than guessed?
pub fn given(entry: &Entry) -> bool {
    entry.get("nickname").and_then(|n| n.get("by")).and_then(Value::as_str) == Some("operator")
}

/// Given by the operator or found by the exact plate: not a guess.
pub fn sure(entry: &Entry) -> bool {
    matches!(entry.get("nickname").and_then(|n| n.get("by")).and_then(Value::as_str), Some("operator" | "plate"))
}

impl Book {
    /// The book in `folder`; empty when there is none or it does not parse.
    pub fn load(folder: &Path) -> Self {
        let path = folder.join(FILE);
        let saved = fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Self { path, saved }
    }

    fn save(&self) -> std::io::Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&self.saved)?)?;
        fs::rename(tmp, &self.path)
    }

    pub fn names(&self) -> &[String] {
        &self.saved.names
    }

    /// The name for a plate, and whether it is only a guess from a plate
    /// one character off (and then only when a single name fits).
    pub fn lookup(&self, plate: &str) -> Option<(String, bool)> {
        let plate = normalize(plate);
        if let Some(name) = self.saved.plates.get(&plate) {
            return Some((name.clone(), false));
        }
        let mut near = self.saved.plates.iter().filter(|(p, _)| one_off(p, &plate)).map(|(_, n)| n);
        let first = near.next()?;
        near.all(|n| same_name(n, first)).then(|| (first.clone(), true))
    }

    /// Gives `plate` (when there is one) the name; an empty name takes the
    /// plate's name away. Returns the cleaned name.
    pub fn set(&mut self, plate: Option<&str>, name: &str) -> std::io::Result<String> {
        let name = clean(name);
        if let Some(plate) = plate.map(normalize).filter(|p| !p.is_empty()) {
            if name.is_empty() {
                self.saved.plates.remove(&plate);
            } else {
                self.saved.plates.insert(plate, name.clone());
            }
        }
        if !name.is_empty() {
            self.saved.names.retain(|n| !same_name(n, &name));
            self.saved.names.insert(0, name.clone());
        }
        self.save()?;
        Ok(name)
    }

    /// Puts the plate's name on an entry, unless the operator named it by
    /// hand. Returns whether the entry changed.
    pub fn stamp(&self, entry: &mut Entry) -> bool {
        if given(entry) {
            return false;
        }
        let want =
            plate_of(entry).and_then(|p| self.lookup(&p)).map(|(name, guess)| json!({ "name": name, "by": if guess { "similar plate" } else { "plate" } }));
        let now = entry.get("nickname").filter(|v| !v.is_null()).cloned();
        if want == now {
            return false;
        }
        match want {
            Some(v) => entry.insert("nickname".into(), v),
            None => entry.remove("nickname"),
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("station-trucks-{}-{}", std::process::id(), fastrand::u64(..)));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(v: Value) -> Entry {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn one_character_off() {
        assert!(one_off("226958C", "2269580"));
        assert!(one_off("519295D", "519295"));
        assert!(one_off("ROOF8F", "ROF8F"));
        assert!(!one_off("ABC123", "ABC123"));
        assert!(!one_off("ABC123", "ABD124"));
        assert!(!one_off("ABC123", "ABC12345"));
    }

    #[test]
    fn names_by_plate_survive_a_reload() {
        let dir = scratch();
        let mut book = Book::load(&dir);
        assert_eq!(book.set(Some("p86-5012"), "  Hoffman /  Steve ").unwrap(), "Hoffman / Steve");
        book.set(None, "Smith Haulage").unwrap();
        let book = Book::load(&dir);
        assert_eq!(book.lookup("P865012"), Some(("Hoffman / Steve".into(), false)));
        assert_eq!(book.lookup("P865O12"), Some(("Hoffman / Steve".into(), true)));
        assert_eq!(book.lookup("XYZ999"), None);
        assert_eq!(book.names(), ["Smith Haulage", "Hoffman / Steve"]);
    }

    #[test]
    fn a_plate_has_one_name_and_a_name_many_plates() {
        let mut book = Book::load(&scratch());
        book.set(Some("AAA111"), "Hoffman/Steve").unwrap();
        book.set(Some("AAA112"), "Hoffman/Steve").unwrap();
        book.set(Some("AAA111"), "Hoffman/Dave").unwrap();
        assert_eq!(book.lookup("AAA111").unwrap().0, "Hoffman/Dave");
        assert_eq!(book.lookup("AAA112").unwrap().0, "Hoffman/Steve");
        // AAA113 is one off both, and they have different names: no guess.
        assert_eq!(book.lookup("AAA113"), None);
        book.set(Some("AAA111"), "").unwrap();
        assert_eq!(book.lookup("AAA111"), Some(("Hoffman/Steve".into(), true)));
        assert_eq!(book.names(), ["Hoffman/Dave", "Hoffman/Steve"]);
    }

    #[test]
    fn stamping_leaves_a_hand_given_name() {
        let mut book = Book::load(&scratch());
        book.set(Some("AAA111"), "Hoffman/Steve").unwrap();
        let mut e = entry(json!({ "plate": { "text": "AAA111" } }));
        assert!(book.stamp(&mut e));
        assert_eq!(e["nickname"], json!({ "name": "Hoffman/Steve", "by": "plate" }));
        assert!(!book.stamp(&mut e));
        let mut mine = entry(json!({ "plate": { "text": "AAA111" }, "nickname": { "name": "Other", "by": "operator" } }));
        assert!(!book.stamp(&mut mine));
        let mut unplated = entry(json!({ "plate": null, "nickname": { "name": "Hoffman/Steve", "by": "plate" } }));
        assert!(book.stamp(&mut unplated));
        assert!(unplated.get("nickname").is_none());
    }
}
