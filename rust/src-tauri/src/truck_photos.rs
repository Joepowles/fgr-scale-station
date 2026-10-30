//! What the frames taken of one truck add up to: its plate, and the picture
//! to keep.
//!
//! Frames are taken from when the truck rolls onto the deck until it leaves
//! (see pipeline.rs), and each one's plate is chosen by plate_choice. Then:
//!
//! - The plate is the text read most surely across all the frames: each
//!   frame's read votes for its text with its confidence. A plate read the
//!   same way in five frames beats a one-off misread, however sure the
//!   reader was of it.
//! - The picture is the frame in which that plate was largest: the truck as
//!   close to the camera as it came while its plate could still be read. A
//!   truck that pulls on so far that its plate leaves the picture, or turns
//!   unreadable, keeps the frame from before that.
//! - With no plate read at all, the picture is the first frame taken once
//!   the truck settled.
//! - A plate that sits still from the truck's first frames on is a vehicle
//!   parked in view, not the truck on the deck (Joe, 2026-10-30: a pickup
//!   parked beside the scale was stamped on a Kenworth). The truck on the
//!   deck is rolling on in its first frames, so its plate moves; a parked
//!   plate is read in the same place in those frames and every one after.
//!   Every plate-like read in a frame is followed, not only the one chosen,
//!   so a parked plate is found even while the truck's plate wins frames.
//!   A parked plate is never chosen, and the votes it took are dropped.

use std::collections::HashMap;

use chrono::{DateTime, Local};

use crate::plate_choice::normalize;
use crate::plates::PlateRead;

/// A frame worth keeping, as a JPEG, with when it was taken.
#[derive(Debug, Clone)]
pub struct Photo {
    /// Counts up per truck: a new id is a different picture.
    pub id: u32,
    pub jpeg: Vec<u8>,
    pub taken_at: DateTime<Local>,
}

/// The plate decided on so far.
#[derive(Debug, Clone, PartialEq)]
pub struct Plate {
    pub text: String,
    /// The best single read of that text.
    pub confidence: f64,
    /// How many frames read it.
    pub frames: u32,
}

/// Everything read as one text.
#[derive(Debug)]
struct Votes {
    total: f64,
    frames: u32,
    best: PlateRead,
    /// The frame where this plate was largest, and how large.
    photo: Photo,
    area: i64,
}

/// A parked plate is first read within this many of the truck's read
/// frames (the reader may not be running for the very first)...
const PARKED_WITHIN: u32 = 3;
/// ...and is still read in the same place this many read frames later.
const PARKED_AFTER: u32 = 4;
/// "The same place": its middle within half its width of where it was first
/// read, or this many pixels for a tiny plate.
const PARKED_MIN_SLACK: f64 = 8.0;

/// Where one text has been read in this truck's frames.
#[derive(Debug)]
struct Sighting {
    first_frame: u32,
    last_frame: u32,
    first_middle: (f64, f64),
    slack: f64,
    moved: bool,
}

impl Sighting {
    fn parked(&self) -> bool {
        !self.moved && self.first_frame <= PARKED_WITHIN && self.last_frame >= self.first_frame + PARKED_AFTER
    }
}

/// What a new frame changed.
#[derive(Debug, Default, PartialEq)]
pub struct Change {
    pub plate: bool,
    pub photo: bool,
}

#[derive(Debug, Default)]
pub struct Truck {
    votes: HashMap<String, Votes>,
    /// The first frame once the truck settled, for when no plate is read.
    settled_frame: Option<Photo>,
    frames: u32,
    next_id: u32,
    /// Every plate-like text read, wherever it was, by frames the reader read.
    sightings: HashMap<String, Sighting>,
    read_frames: u32,
}

fn middle(read: &PlateRead) -> (f64, f64) {
    let [x1, y1, x2, y2] = read.bounds;
    ((x1 + x2) as f64 / 2.0, (y1 + y2) as f64 / 2.0)
}

fn area(read: &PlateRead) -> i64 {
    let [x1, y1, x2, y2] = read.bounds;
    (x2 - x1).max(0) * (y2 - y1).max(0)
}

impl Truck {
    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// Is this text a vehicle parked in view?
    pub fn is_parked(&self, text: &str) -> bool {
        self.sightings.get(&normalize(text)).is_some_and(Sighting::parked)
    }

    /// The parked plates found so far, in no particular order.
    pub fn parked(&self) -> Vec<String> {
        self.sightings.iter().filter(|(_, s)| s.parked()).map(|(t, _)| t.clone()).collect()
    }

    /// Notes where each plate in a frame the reader read was.
    fn see(&mut self, plates: &[PlateRead]) {
        self.read_frames += 1;
        let frame = self.read_frames;
        let mut texts = std::collections::HashSet::new();
        // The same text twice in one picture is signage; its first is enough.
        for read in plates.iter().filter(|p| texts.insert(normalize(&p.text))) {
            let here = middle(read);
            let width = (read.bounds[2] - read.bounds[0]).max(0) as f64;
            let s = self.sightings.entry(normalize(&read.text)).or_insert_with(|| Sighting {
                first_frame: frame,
                last_frame: frame,
                first_middle: here,
                slack: (width / 2.0).max(PARKED_MIN_SLACK),
                moved: false,
            });
            s.last_frame = frame;
            let (dx, dy) = (here.0 - s.first_middle.0, here.1 - s.first_middle.1);
            if dx.hypot(dy) > s.slack {
                s.moved = true;
            }
        }
    }

    fn leader(&self) -> Option<&Votes> {
        self.votes
            .iter()
            .filter(|(text, _)| !self.sightings.get(*text).is_some_and(Sighting::parked))
            .map(|(_, v)| v)
            .max_by(|a, b| a.total.total_cmp(&b.total).then(a.frames.cmp(&b.frames)))
    }

    pub fn plate(&self) -> Option<Plate> {
        self.leader().map(|v| Plate { text: v.best.text.clone(), confidence: v.best.confidence, frames: v.frames })
    }

    /// The picture to keep now.
    pub fn photo(&self) -> Option<&Photo> {
        self.leader().map(|v| &v.photo).or(self.settled_frame.as_ref())
    }

    /// Adds a frame. `seen`: every plate-like read in it, or None when the
    /// reader did not read it. `choose` picks the truck's plate from the ones
    /// not parked (None: no plate read well enough). `settled`: the truck has
    /// settled on the scale by now.
    pub fn add(
        &mut self,
        jpeg: Vec<u8>,
        taken_at: DateTime<Local>,
        seen: Option<&[PlateRead]>,
        choose: impl FnOnce(&[PlateRead]) -> Option<PlateRead>,
        settled: bool,
    ) -> Change {
        let before = (self.plate().map(|p| p.text), self.photo().map(|p| p.id));
        let read = seen.and_then(|plates| {
            self.see(plates);
            let moving: Vec<PlateRead> = plates.iter().filter(|p| !self.is_parked(&p.text)).cloned().collect();
            choose(&moving)
        });
        self.frames += 1;
        self.next_id += 1;
        let photo = Photo { id: self.next_id, jpeg, taken_at };
        match read {
            Some(read) => {
                let key = normalize(&read.text);
                let size = area(&read);
                match self.votes.get_mut(&key) {
                    Some(v) => {
                        v.total += read.confidence;
                        v.frames += 1;
                        if size > v.area {
                            v.area = size;
                            v.photo = photo;
                        }
                        if read.confidence > v.best.confidence {
                            v.best = read;
                        }
                    }
                    None => {
                        self.votes.insert(key, Votes { total: read.confidence, frames: 1, best: read, photo, area: size });
                    }
                }
            }
            None if settled && self.settled_frame.is_none() => self.settled_frame = Some(photo),
            None => {}
        }
        let after = (self.plate().map(|p| p.text), self.photo().map(|p| p.id));
        Change { plate: before.0 != after.0, photo: before.1 != after.1 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str, confidence: f64, width: i64) -> Option<PlateRead> {
        Some(PlateRead { text: text.into(), confidence, char_confidence: vec![], det_confidence: 0.8, bounds: [100, 100, 100 + width, 100 + width / 4] })
    }

    fn frame(n: u8) -> Vec<u8> {
        vec![n]
    }

    /// A frame whose only plate-like read is `read` (None: nothing read).
    fn add(t: &mut Truck, jpeg: Vec<u8>, now: DateTime<Local>, read: Option<PlateRead>, settled: bool) -> Change {
        let seen: Vec<PlateRead> = read.into_iter().collect();
        t.add(jpeg, now, Some(&seen), |plates| plates.first().cloned(), settled)
    }

    #[test]
    fn keeps_the_frame_where_the_plate_was_largest_while_still_readable() {
        let mut t = Truck::default();
        let now = Local::now();
        // Rolling on: the plate grows as the truck comes closer...
        add(&mut t, frame(1), now, read("P865012", 0.7, 60), false);
        add(&mut t, frame(2), now, read("P865012", 0.85, 120), false);
        let change = add(&mut t, frame(3), now, read("P865012", 0.92, 200), false);
        assert_eq!(change, Change { plate: false, photo: true });
        // ...then it pulls on too far and the plate is gone from the picture.
        let change = add(&mut t, frame(4), now, None, true);
        assert_eq!(change, Change::default());
        assert_eq!(t.photo().unwrap().jpeg, frame(3));
        let plate = t.plate().unwrap();
        assert_eq!((plate.text.as_str(), plate.confidence, plate.frames), ("P865012", 0.92, 3));
    }

    #[test]
    fn a_plate_read_alike_in_several_frames_beats_a_one_off_misread() {
        let mut t = Truck::default();
        let now = Local::now();
        add(&mut t, frame(1), now, read("P865012", 0.7, 80), false);
        add(&mut t, frame(2), now, read("P865O12", 0.95, 200), false); // a 0 read as O, once, close up
        add(&mut t, frame(3), now, read("P865012", 0.72, 90), false);
        assert_eq!(t.plate().unwrap().text, "P865012");
        // The picture goes with the plate decided on, not the misread's.
        assert_eq!(t.photo().unwrap().jpeg, frame(3));
    }

    #[test]
    fn the_same_plate_written_differently_is_one_plate() {
        let mut t = Truck::default();
        add(&mut t, frame(1), Local::now(), read("P86-5012", 0.8, 80), false);
        add(&mut t, frame(2), Local::now(), read("p865012", 0.9, 90), false);
        assert_eq!(t.plate().unwrap().frames, 2);
    }

    #[test]
    fn with_no_plate_read_the_first_settled_frame_is_the_picture() {
        let mut t = Truck::default();
        let now = Local::now();
        assert_eq!(add(&mut t, frame(1), now, None, false), Change::default()); // rolling on: not kept
        assert!(t.photo().is_none());
        assert_eq!(add(&mut t, frame(2), now, None, true), Change { plate: false, photo: true });
        add(&mut t, frame(3), now, None, true);
        assert_eq!(t.photo().unwrap().jpeg, frame(2));
        assert!(t.plate().is_none());
        // A plate read later takes over the picture.
        assert_eq!(add(&mut t, frame(4), now, read("P865012", 0.9, 100), true), Change { plate: true, photo: true });
        assert_eq!(t.photo().unwrap().jpeg, frame(4));
        assert_eq!(t.frames(), 4);
    }

    fn at(text: &str, confidence: f64, bounds: [i64; 4]) -> PlateRead {
        PlateRead { text: text.into(), confidence, char_confidence: vec![], det_confidence: 0.8, bounds }
    }

    #[test]
    fn a_pickup_parked_beside_the_scale_is_not_the_truck_on_it() {
        // Joe, 2026-10-30, 3840 wide: a GMC parked left of the deck, plate
        // read surely in every frame; the Kenworth's plate grows as it rolls
        // on from the right and is read less surely than the pickup's.
        let rules = crate::plate_choice::Rules::default();
        let choose = |plates: &[PlateRead]| crate::plate_choice::choose(plates, 3840.0, &rules).plate;
        let pickup = at("3863882B", 0.97, [442, 1469, 547, 1536]);
        let mut t = Truck::default();
        let now = Local::now();
        // Rolling on: only the pickup's plate can be read yet.
        for n in 1..=3 {
            t.add(frame(n), now, Some(std::slice::from_ref(&pickup)), choose, false);
        }
        assert_eq!(t.plate().unwrap().text, "3863882B"); // not known to be parked yet
        let kenworth = [[3300, 900, 3390, 945], [3150, 950, 3300, 1020], [2950, 1000, 3150, 1100], [2928, 1008, 3139, 1110]];
        for (n, bounds) in kenworth.into_iter().enumerate() {
            let seen = [pickup.clone(), at("P1090037", 0.7, bounds)];
            t.add(frame(4 + n as u8), now, Some(&seen), choose, n >= 3);
        }
        assert_eq!(t.parked(), vec!["3863882B".to_string()]);
        let plate = t.plate().unwrap();
        // Its first, far-off read lost frame 4 to the pickup, still unknown.
        assert_eq!((plate.text.as_str(), plate.frames), ("P1090037", 3));
        assert_eq!(t.photo().unwrap().jpeg, frame(7)); // its plate largest
                                                       // Settled, the pickup's plate is never taken, however sure.
        let seen = [pickup.clone(), at("P1090037", 0.5, [2928, 1008, 3139, 1110])];
        t.add(frame(8), now, Some(&seen), choose, true);
        assert_eq!(t.plate().unwrap().text, "P1090037");
    }

    #[test]
    fn a_parked_plate_alone_gives_no_plate() {
        let mut t = Truck::default();
        let now = Local::now();
        for n in 1..=4 {
            add(&mut t, frame(n), now, Some(at("3863882B", 0.97, [442, 1469, 547, 1536])), n == 4);
        }
        assert_eq!(t.plate().unwrap().text, "3863882B"); // four frames: not sure yet
        add(&mut t, frame(5), now, Some(at("3863882B", 0.97, [444, 1470, 549, 1537])), true); // a jiggle is not a move
        assert!(t.plate().is_none());
        assert!(t.is_parked("3863882b"));
        assert_eq!(t.photo().unwrap().jpeg, frame(5)); // the first settled frame with no plate taken
    }

    #[test]
    fn the_truck_that_rolled_on_and_then_stood_is_not_parked() {
        let mut t = Truck::default();
        let now = Local::now();
        add(&mut t, frame(1), now, Some(at("P865012", 0.8, [2000, 900, 2100, 940])), false);
        for n in 2..=20 {
            add(&mut t, frame(n), now, Some(at("P865012", 0.9, [1500, 900, 1740, 960])), n > 5);
        }
        assert!(t.parked().is_empty());
        assert_eq!(t.plate().unwrap().text, "P865012");
    }

    #[test]
    fn a_plate_first_read_once_the_truck_is_on_is_not_parked() {
        // Read only from the fifth frame, already still: the truck on the
        // deck, whose plate could not be read while it rolled on.
        let mut t = Truck::default();
        let now = Local::now();
        for n in 1..=4 {
            add(&mut t, frame(n), now, None, false);
        }
        for n in 5..=15 {
            add(&mut t, frame(n), now, Some(at("P865012", 0.9, [1500, 900, 1740, 960])), true);
        }
        assert_eq!(t.plate().unwrap().text, "P865012");
    }

    #[test]
    fn a_frame_the_reader_did_not_read_does_not_count() {
        let mut t = Truck::default();
        let now = Local::now();
        // The reader was starting for the first frames.
        for n in 1..=3 {
            t.add(frame(n), now, None, |_| None, false);
        }
        for n in 4..=8 {
            add(&mut t, frame(n), now, Some(at("3863882B", 0.97, [442, 1469, 547, 1536])), true);
        }
        assert!(t.is_parked("3863882B"));
    }
}
