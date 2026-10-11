//! Licence plate reading with the open models from the fast-alpr project, on
//! ONNX Runtime and nothing else: no Python, no OpenCV.
//!
//! Two models. A YOLO v9 finds plates in the whole picture (it does its own
//! non-maximum suppression, so each output row is a plate), and a CCT
//! character reader reads each plate it found. This reproduces what
//! fast-alpr 0.4 does with them, step for step, so the reads are the ones
//! the Python worker gave:
//!
//! - detector (open-image-models 0.6, yolo-v9-t-384-license-plate-end2end):
//!   letterbox to 384x384 with grey (114) bars, RGB, scaled to 0..1, NCHW.
//!   Rows below 0.4 are dropped; boxes are mapped back through the
//!   letterbox and truncated to whole pixels.
//! - reader (fast-plate-ocr 1.1, cct-xs-v1-global-model): the crop squashed
//!   to 128x64 RGB, bytes as they are (the model scales them itself). Nine
//!   slots of 37 classes; each slot's best class, the pad character '_'
//!   trimmed off the end.
//!
//! Both resizes are OpenCV's INTER_LINEAR, which is plain bilinear sampling
//! and does not blur first when shrinking - unlike the `image` crate's
//! filters. A 4K frame shrunk to 384 looks different the two ways, and the
//! detector was trained on the OpenCV way, so `resize_linear` does that.

use std::path::Path;
use std::sync::OnceLock;

use image::RgbImage;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;
use serde::Serialize;

pub const DETECTOR_FILE: &str = "yolo-v9-t-384-license-plates-end2end.onnx";
pub const READER_FILE: &str = "cct_xs_v1_global.onnx";

const DETECTOR_SIZE: u32 = 384;
const DETECTOR_THRESHOLD: f32 = 0.4;
const LETTERBOX_GREY: u8 = 114;

// From cct_xs_v1_global_plate_config.yaml.
const READER_WIDTH: u32 = 128;
const READER_HEIGHT: u32 = 64;
const SLOTS: usize = 9;
const ALPHABET: &[u8; 37] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ_";
const PAD: char = '_';

/// One plate read, as the Python worker reported it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plate {
    pub text: String,
    /// The weakest character: one wrong character is a wrong plate.
    pub confidence: f32,
    /// Every slot's confidence, the trimmed padding slots included.
    pub char_confidence: Vec<f32>,
    pub det_confidence: f32,
    /// x1, y1, x2, y2 in pixels of the image given.
    pub r#box: [i32; 4],
}

/// A plate the detector found, before it is read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    pub confidence: f32,
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

pub struct Reader {
    detector: Session,
    reader: Session,
}

impl Reader {
    /// Loads both models from `dir` (see DETECTOR_FILE and READER_FILE).
    pub fn load(dir: &Path, threads: usize) -> Result<Self, String> {
        let open = |file: &str| -> Result<Session, String> {
            let path = dir.join(file);
            if !path.exists() {
                return Err(format!("model not found: {}", path.display()));
            }
            let fail = |e: &dyn std::fmt::Display| format!("could not load {file}: {e}");
            Session::builder()
                .map_err(|e| fail(&e))?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(|e| fail(&e))?
                // Set explicitly, so ONNX Runtime does not try to pin threads
                // to cores (which containers refuse, noisily).
                .with_intra_threads(threads)
                .map_err(|e| fail(&e))?
                .commit_from_file(&path)
                .map_err(|e| fail(&e))
        };
        Ok(Self { detector: open(DETECTOR_FILE)?, reader: open(READER_FILE)? })
    }

    /// Every plate in the picture that reads as something, as fast-alpr's
    /// ALPR.predict followed by the worker's filtering.
    pub fn read(&mut self, image: &RgbImage) -> Result<Vec<Plate>, String> {
        let mut plates = Vec::new();
        for found in self.detect(image)? {
            let Some(crop) = crop(image, &found) else { continue };
            let (text, chars) = self.read_crop(&crop)?;
            if text.is_empty() {
                continue;
            }
            plates.push(Plate {
                text,
                confidence: chars.iter().copied().fold(f32::INFINITY, f32::min),
                char_confidence: chars,
                det_confidence: found.confidence,
                r#box: [found.x1, found.y1, found.x2, found.y2],
            });
        }
        Ok(plates)
    }

    pub fn detect(&mut self, image: &RgbImage) -> Result<Vec<Detection>, String> {
        let boxed = letterbox(image, DETECTOR_SIZE);
        let side = DETECTOR_SIZE as usize;
        let mut chw = vec![0f32; 3 * side * side];
        for (i, pixel) in boxed.image.pixels().enumerate() {
            for c in 0..3 {
                chw[c * side * side + i] = (f64::from(pixel[c]) / 255.0) as f32;
            }
        }
        let input = Tensor::from_array(([1usize, 3, side, side], chw)).map_err(|e| e.to_string())?;
        let outputs = self.detector.run(ort::inputs!["images" => input]).map_err(|e| format!("plate detector failed: {e}"))?;
        let (shape, rows) = outputs[0].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        let width = shape.last().copied().unwrap_or(0) as usize;
        if width < 7 {
            return Err(format!("unexpected detector output shape {shape:?}"));
        }
        Ok(rows.chunks_exact(width).filter_map(|row| boxed.unmap(row)).collect())
    }

    /// The text of one plate crop and each slot's confidence.
    pub fn read_crop(&mut self, crop: &RgbImage) -> Result<(String, Vec<f32>), String> {
        let squashed = resize_linear(crop, READER_WIDTH, READER_HEIGHT);
        let shape = [1usize, READER_HEIGHT as usize, READER_WIDTH as usize, 3];
        let input = Tensor::from_array((shape, squashed.into_raw())).map_err(|e| e.to_string())?;
        let outputs = self.reader.run(ort::inputs!["input" => input]).map_err(|e| format!("plate reader failed: {e}"))?;
        let (_, probs) = outputs[0].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        decode(probs)
    }
}

/// Nine slots of 37 scores each into text and per-slot confidence.
pub fn decode(probs: &[f32]) -> Result<(String, Vec<f32>), String> {
    if probs.len() != SLOTS * ALPHABET.len() {
        return Err(format!("unexpected reader output of {} values", probs.len()));
    }
    let mut text = String::with_capacity(SLOTS);
    let mut chars = Vec::with_capacity(SLOTS);
    for slot in probs.as_chunks::<{ ALPHABET.len() }>().0 {
        // The first of equal scores wins, as numpy's argmax.
        let (best, score) = slot.iter().enumerate().fold((0, f32::NEG_INFINITY), |acc, (i, &p)| if p > acc.1 { (i, p) } else { acc });
        text.push(ALPHABET[best] as char);
        chars.push(score);
    }
    Ok((text.trim_end_matches(PAD).to_string(), chars))
}

/// The picture shrunk (or grown) to fit a square, centred on grey bars,
/// and how to map a box on it back.
pub struct Letterbox {
    pub image: RgbImage,
    ratio: f64,
    dw: f64,
    dh: f64,
}

impl Letterbox {
    /// A detector row [batch, x1, y1, x2, y2, class, score] as a box on the
    /// original picture, if it scores high enough. The arithmetic follows
    /// open-image-models: in f64, stored back as f32, truncated toward zero.
    fn unmap(&self, row: &[f32]) -> Option<Detection> {
        let score = row[6];
        if score < DETECTOR_THRESHOLD {
            return None;
        }
        let x = |v: f32| ((f64::from(v) - self.dw) / self.ratio) as f32 as i32;
        let y = |v: f32| ((f64::from(v) - self.dh) / self.ratio) as f32 as i32;
        Some(Detection { confidence: score, x1: x(row[1]), y1: y(row[2]), x2: x(row[3]), y2: y(row[4]) })
    }
}

/// open-image-models' letterbox: Python's round() is round-half-even.
pub fn letterbox(image: &RgbImage, side: u32) -> Letterbox {
    let (w, h) = (f64::from(image.width()), f64::from(image.height()));
    let ratio = (f64::from(side) / h).min(f64::from(side) / w);
    let new_w = (w * ratio).round_ties_even() as u32;
    let new_h = (h * ratio).round_ties_even() as u32;
    let dw = f64::from(side - new_w) / 2.0;
    let dh = f64::from(side - new_h) / 2.0;
    let resized = if (new_w, new_h) == image.dimensions() { image.clone() } else { resize_linear(image, new_w, new_h) };
    let top = (dh - 0.1).round_ties_even() as u32;
    let left = (dw - 0.1).round_ties_even() as u32;
    let mut out = RgbImage::from_pixel(side, side, image::Rgb([LETTERBOX_GREY; 3]));
    image::imageops::replace(&mut out, &resized, i64::from(left), i64::from(top));
    Letterbox { image: out, ratio, dw, dh }
}

/// The plate's pixels, the box clamped to the picture as fast-alpr does.
/// None for a box with nothing inside it.
pub fn crop(image: &RgbImage, d: &Detection) -> Option<RgbImage> {
    let (w, h) = (image.width() as i32, image.height() as i32);
    let (x1, y1, x2, y2) = (d.x1.max(0), d.y1.max(0), d.x2.min(w), d.y2.min(h));
    if x2 <= x1 || y2 <= y1 {
        return None;
    }
    Some(image::imageops::crop_imm(image, x1 as u32, y1 as u32, (x2 - x1) as u32, (y2 - y1) as u32).to_image())
}

/// OpenCV's cv2.resize(..., INTER_LINEAR) for 8-bit pictures: each output
/// pixel samples the two nearest source pixels each way, pixel centres
/// aligned, weights in 11-bit fixed point as OpenCV keeps them. One case is
/// special in OpenCV and here: halving exactly is a 2x2 average instead.
pub fn resize_linear(src: &RgbImage, dst_w: u32, dst_h: u32) -> RgbImage {
    let (src_w, src_h) = src.dimensions();
    if src_w == 2 * dst_w && src_h == 2 * dst_h {
        return halve(src);
    }
    let xs = taps(src_w, dst_w);
    let ys = taps(src_h, dst_h);
    let raw = src.as_raw();
    let stride = src_w as usize * 3;
    let mut out = RgbImage::new(dst_w, dst_h);
    let mut row0 = vec![0i32; dst_w as usize * 3];
    let mut row1 = vec![0i32; dst_w as usize * 3];
    let horizontal = |y: usize, row: &mut [i32]| {
        let line = &raw[y * stride..(y + 1) * stride];
        for (dx, &(sx, a0, a1)) in xs.iter().enumerate() {
            let next = (sx + 1).min(src_w as usize - 1);
            for c in 0..3 {
                row[dx * 3 + c] = i32::from(line[sx * 3 + c]) * a0 + i32::from(line[next * 3 + c]) * a1;
            }
        }
    };
    for (dy, &(sy, b0, b1)) in ys.iter().enumerate() {
        horizontal(sy, &mut row0);
        horizontal((sy + 1).min(src_h as usize - 1), &mut row1);
        let line = &mut out.as_mut()[dy * dst_w as usize * 3..(dy + 1) * dst_w as usize * 3];
        for (i, v) in line.iter_mut().enumerate() {
            let sum = i64::from(row0[i]) * i64::from(b0) + i64::from(row1[i]) * i64::from(b1);
            *v = ((sum + (1 << 21)) >> 22).clamp(0, 255) as u8;
        }
    }
    out
}

/// For each output column (or row): the first source index and the two
/// weights out of 2048, as OpenCV computes them (in f32, rounded each).
fn taps(src: u32, dst: u32) -> Vec<(usize, i32, i32)> {
    let scale = f64::from(src) / f64::from(dst);
    (0..dst)
        .map(|d| {
            let f = ((f64::from(d) + 0.5) * scale - 0.5) as f32;
            let mut s = f.floor() as i64;
            let mut frac = f - s as f32;
            if s < 0 {
                s = 0;
                frac = 0.0;
            }
            if s >= i64::from(src) - 1 {
                s = i64::from(src) - 1;
                frac = 0.0;
            }
            (s as usize, ((1.0 - frac) * 2048.0).round_ties_even() as i32, (frac * 2048.0).round_ties_even() as i32)
        })
        .collect()
}

fn halve(src: &RgbImage) -> RgbImage {
    RgbImage::from_fn(src.width() / 2, src.height() / 2, |x, y| {
        let p = |dx, dy| src.get_pixel(2 * x + dx, 2 * y + dy).0;
        let (a, b, c, d) = (p(0, 0), p(1, 0), p(0, 1), p(1, 1));
        image::Rgb(std::array::from_fn(|i| ((u32::from(a[i]) + u32::from(b[i]) + u32::from(c[i]) + u32::from(d[i]) + 2) >> 2) as u8))
    })
}

/// Opens a picture the way cv2.imread does for these: decoded to RGB, turned
/// upright by its EXIF orientation if it has one.
pub fn open(path: &Path) -> Result<RgbImage, String> {
    use image::ImageDecoder;
    let fail = |e: &dyn std::fmt::Display| format!("Failed to load image from path: {} ({e})", path.display());
    let mut decoder =
        image::ImageReader::open(path).map_err(|e| fail(&e))?.with_guessed_format().map_err(|e| fail(&e))?.into_decoder().map_err(|e| fail(&e))?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut picture = image::DynamicImage::from_decoder(decoder).map_err(|e| fail(&e))?;
    picture.apply_orientation(orientation);
    Ok(picture.into_rgb8())
}

/// ONNX Runtime threads for each model: the PC's cores, up to four. The
/// models are small; more threads than that only add overhead.
pub fn default_threads() -> usize {
    static THREADS: OnceLock<usize> = OnceLock::new();
    *THREADS.get_or_init(|| std::thread::available_parallelism().map_or(2, |n| n.get().min(4)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| image::Rgb([(x * 7 % 256) as u8, (y * 5 % 256) as u8, ((x + y) % 256) as u8]))
    }

    #[test]
    fn decoding_takes_each_slots_best_and_trims_the_padding() {
        let mut probs = vec![0.0; SLOTS * 37];
        for (slot, ch) in "5AU5341__".chars().enumerate() {
            let class = ALPHABET.iter().position(|&a| a as char == ch).unwrap();
            probs[slot * 37 + class] = 0.9 - slot as f32 * 0.01;
        }
        let (text, chars) = decode(&probs).unwrap();
        assert_eq!(text, "5AU5341");
        assert_eq!(chars.len(), 9);
        assert!((chars[8] - 0.82).abs() < 1e-6);
    }

    #[test]
    fn a_pad_inside_the_text_stays() {
        let mut probs = vec![0.0; SLOTS * 37];
        for (slot, ch) in "AB_C_____".chars().enumerate() {
            probs[slot * 37 + ALPHABET.iter().position(|&a| a as char == ch).unwrap()] = 1.0;
        }
        assert_eq!(decode(&probs).unwrap().0, "AB_C");
    }

    #[test]
    fn letterboxing_a_4k_frame_matches_open_image_models() {
        let boxed = letterbox(&gradient(3840, 2160), 384);
        // r = 0.1, 384x216 centred: 84 rows of grey above and below.
        assert_eq!((boxed.ratio, boxed.dw, boxed.dh), (0.1, 0.0, 84.0));
        assert_eq!(boxed.image.get_pixel(0, 83).0, [114; 3]);
        assert_ne!(boxed.image.get_pixel(0, 84).0, [114; 3]);
        assert_eq!(boxed.image.get_pixel(0, 300).0, [114; 3]);
        // A box at the middle of the letterbox is the middle of the frame.
        let d = boxed.unmap(&[0.0, 182.0, 182.0, 202.0, 202.0, 0.0, 0.9]).unwrap();
        assert_eq!((d.x1, d.y1, d.x2, d.y2), (1820, 980, 2020, 1180));
        assert!(boxed.unmap(&[0.0, 1.0, 1.0, 2.0, 2.0, 0.0, 0.39]).is_none());
    }

    #[test]
    fn odd_padding_splits_the_way_python_rounds() {
        // 704x480 -> r = 0.5454..., 384x262: 61 above and 61 below.
        let boxed = letterbox(&gradient(704, 480), 384);
        assert_eq!(boxed.dh, 61.0);
        assert_eq!(boxed.image.get_pixel(0, 60).0, [114; 3]);
        assert_ne!(boxed.image.get_pixel(0, 61).0, [114; 3]);
        assert_ne!(boxed.image.get_pixel(0, 322).0, [114; 3]);
        assert_eq!(boxed.image.get_pixel(0, 323).0, [114; 3]);
    }

    #[test]
    fn a_box_hanging_off_the_picture_is_clamped_and_an_empty_one_skipped() {
        let image = gradient(100, 50);
        let d = |x1, y1, x2, y2| Detection { confidence: 0.9, x1, y1, x2, y2 };
        assert_eq!(crop(&image, &d(-5, -5, 20, 10)).unwrap().dimensions(), (20, 10));
        assert_eq!(crop(&image, &d(90, 40, 130, 70)).unwrap().dimensions(), (10, 10));
        assert!(crop(&image, &d(120, 0, 130, 10)).is_none());
    }

    #[test]
    fn resizing_to_the_same_size_changes_nothing() {
        let image = gradient(37, 23);
        assert_eq!(resize_linear(&image, 37, 23), image);
    }

    #[test]
    fn shrinking_samples_without_blurring_first() {
        // Stripes one pixel wide, shrunk 4x: OpenCV samples between two
        // stripes and gets their average, never a blur over all four.
        let stripes = RgbImage::from_fn(16, 4, |x, _| image::Rgb([if x % 2 == 0 { 0 } else { 200 }; 3]));
        let small = resize_linear(&stripes, 4, 1);
        assert!(small.pixels().all(|p| p.0 == [100; 3]), "{:?}", small.pixels().collect::<Vec<_>>());
    }

    #[test]
    fn halving_exactly_averages_each_2x2() {
        let image = RgbImage::from_fn(4, 2, |x, y| image::Rgb([(x * 10 + y) as u8; 3]));
        let half = resize_linear(&image, 2, 1);
        // (0 + 10 + 1 + 11 + 2) / 4 = 6, (20 + 30 + 21 + 31 + 2) / 4 = 26
        assert_eq!((half.get_pixel(0, 0).0[0], half.get_pixel(1, 0).0[0]), (6, 26));
    }
}
