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
}

fn area(read: &PlateRead) -> i64 {
    let [x1, y1, x2, y2] = read.bounds;
    (x2 - x1).max(0) * (y2 - y1).max(0)
}

impl Truck {
    pub fn frames(&self) -> u32 {
        self.frames
    }

    fn leader(&self) -> Option<&Votes> {
        self.votes.values().max_by(|a, b| a.total.total_cmp(&b.total).then(a.frames.cmp(&b.frames)))
    }

    pub fn plate(&self) -> Option<Plate> {
        self.leader().map(|v| Plate { text: v.best.text.clone(), confidence: v.best.confidence, frames: v.frames })
    }

    /// The picture to keep now.
    pub fn photo(&self) -> Option<&Photo> {
        self.leader().map(|v| &v.photo).or(self.settled_frame.as_ref())
    }

    /// Adds a frame and its chosen plate (None: no plate read well enough).
    /// `settled`: the truck has settled on the scale by now.
    pub fn add(&mut self, jpeg: Vec<u8>, taken_at: DateTime<Local>, read: Option<PlateRead>, settled: bool) -> Change {
        let before = (self.plate().map(|p| p.text), self.photo().map(|p| p.id));
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

    #[test]
    fn keeps_the_frame_where_the_plate_was_largest_while_still_readable() {
        let mut t = Truck::default();
        let now = Local::now();
        // Rolling on: the plate grows as the truck comes closer...
        t.add(frame(1), now, read("P865012", 0.7, 60), false);
        t.add(frame(2), now, read("P865012", 0.85, 120), false);
        let change = t.add(frame(3), now, read("P865012", 0.92, 200), false);
        assert_eq!(change, Change { plate: false, photo: true });
        // ...then it pulls on too far and the plate is gone from the picture.
        let change = t.add(frame(4), now, None, true);
        assert_eq!(change, Change::default());
        assert_eq!(t.photo().unwrap().jpeg, frame(3));
        let plate = t.plate().unwrap();
        assert_eq!((plate.text.as_str(), plate.confidence, plate.frames), ("P865012", 0.92, 3));
    }

    #[test]
    fn a_plate_read_alike_in_several_frames_beats_a_one_off_misread() {
        let mut t = Truck::default();
        let now = Local::now();
        t.add(frame(1), now, read("P865012", 0.7, 80), false);
        t.add(frame(2), now, read("P865O12", 0.95, 200), false); // a 0 read as O, once, close up
        t.add(frame(3), now, read("P865012", 0.72, 90), false);
        assert_eq!(t.plate().unwrap().text, "P865012");
        // The picture goes with the plate decided on, not the misread's.
        assert_eq!(t.photo().unwrap().jpeg, frame(3));
    }

    #[test]
    fn the_same_plate_written_differently_is_one_plate() {
        let mut t = Truck::default();
        t.add(frame(1), Local::now(), read("P86-5012", 0.8, 80), false);
        t.add(frame(2), Local::now(), read("p865012", 0.9, 90), false);
        assert_eq!(t.plate().unwrap().frames, 2);
    }

    #[test]
    fn with_no_plate_read_the_first_settled_frame_is_the_picture() {
        let mut t = Truck::default();
        let now = Local::now();
        assert_eq!(t.add(frame(1), now, None, false), Change::default()); // rolling on: not kept
        assert!(t.photo().is_none());
        assert_eq!(t.add(frame(2), now, None, true), Change { plate: false, photo: true });
        t.add(frame(3), now, None, true);
        assert_eq!(t.photo().unwrap().jpeg, frame(2));
        assert!(t.plate().is_none());
        // A plate read later takes over the picture.
        assert_eq!(t.add(frame(4), now, read("P865012", 0.9, 100), true), Change { plate: true, photo: true });
        assert_eq!(t.photo().unwrap().jpeg, frame(4));
        assert_eq!(t.frames(), 4);
    }
}
