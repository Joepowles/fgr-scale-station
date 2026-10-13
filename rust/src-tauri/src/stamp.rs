//! Writing the weighing onto its photo: the plate and the weight as chips in
//! the corners the settings choose, and this PC's clock in the bottom right.
//!
//! The cameras burn in their own clocks, and those cannot be relied on (one
//! reads 1970 after every reboot; another runs an hour out). So every photo
//! saved carries the time this PC took it, labelled, in a fixed corner; where
//! the two disagree, ours is the one to trust.
//!
//! Drawn with the font compiled into the app (DejaVu Sans Mono Bold, see
//! assets/), so it looks the same on any PC whatever fonts it has.

use std::sync::LazyLock;

use ab_glyph::{FontRef, PxScale};
use chrono::{DateTime, Local};
use image::codecs::jpeg::JpegEncoder;
use image::{Rgb, RgbImage};
use imageproc::drawing::{draw_filled_rect_mut, draw_text_mut, text_size};
use imageproc::rect::Rect;

static FONT: LazyLock<FontRef<'static>> =
    LazyLock::new(|| FontRef::try_from_slice(include_bytes!("../assets/DejaVuSansMono-Bold.ttf")).expect("the bundled font"));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    /// From the settings' "top-left" and so on; anything else is bottom-left.
    pub fn named(name: &str) -> Self {
        match name {
            "top-left" => Corner::TopLeft,
            "top-right" => Corner::TopRight,
            "bottom-right" => Corner::BottomRight,
            _ => Corner::BottomLeft,
        }
    }

    pub fn opposite(self) -> Self {
        match self {
            Corner::TopLeft => Corner::TopRight,
            Corner::TopRight => Corner::TopLeft,
            Corner::BottomLeft => Corner::BottomRight,
            Corner::BottomRight => Corner::BottomLeft,
        }
    }

    fn right(self) -> bool {
        matches!(self, Corner::TopRight | Corner::BottomRight)
    }

    fn bottom(self) -> bool {
        matches!(self, Corner::BottomLeft | Corner::BottomRight)
    }
}

/// What goes on the photo.
#[derive(Debug, Clone)]
pub struct Stamp {
    pub taken_at: DateTime<Local>,
    pub plate: Option<(String, Corner)>,
    pub weight: Option<(String, Corner)>,
}

/// "2026-10-09 20:07:09 -05:00": this PC's local time, with its offset.
pub fn clock_text(at: DateTime<Local>) -> String {
    format!("server {}", at.format("%Y-%m-%d %H:%M:%S %:z"))
}

/// The JPEG with the stamp drawn on, or the error that stopped it.
/// `quality`: the history's photo quality setting (85 by default).
pub fn stamp_jpeg(jpeg: &[u8], stamp: &Stamp, quality: u8) -> Result<Vec<u8>, String> {
    let mut image = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).map_err(|e| e.to_string())?.to_rgb8();
    draw(&mut image, stamp);
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality).encode_image(&image).map_err(|e| e.to_string())?;
    Ok(out)
}

pub fn draw(image: &mut RgbImage, stamp: &Stamp) {
    let (width, height) = (image.width() as i32, image.height() as i32);
    clock(image, width, height, &clock_text(stamp.taken_at));
    let plate_corner = stamp.plate.as_ref().map(|(_, c)| *c);
    if let Some((text, corner)) = &stamp.plate {
        chip(image, width, height, text, *corner, 0);
    }
    if let Some((text, corner)) = &stamp.weight {
        // Sharing a corner with the plate, the weight sits beside it.
        let stack = i32::from(plate_corner == Some(*corner));
        chip(image, width, height, text, *corner, stack);
    }
}

/// Darkens a rectangle towards `color` by `alpha`, for a see-through backing.
fn shade(image: &mut RgbImage, x: i32, y: i32, w: i32, h: i32, color: [u8; 3], alpha: f32) {
    for py in y.max(0)..(y + h).min(image.height() as i32) {
        for px in x.max(0)..(x + w).min(image.width() as i32) {
            let pixel = image.get_pixel_mut(px as u32, py as u32);
            for (channel, target) in pixel.0.iter_mut().zip(color) {
                *channel = (*channel as f32 * (1.0 - alpha) + target as f32 * alpha).round() as u8;
            }
        }
    }
}

/// This PC's clock, small, bottom right: the one corner the cameras leave
/// free (their clock is along the top, their name bottom left).
fn clock(image: &mut RgbImage, width: i32, height: i32, text: &str) {
    let size = (width / 70).max(11);
    let (pad_x, pad_y) = ((size as f32 * 0.5).round() as i32, (size as f32 * 0.35).round() as i32);
    let scale = PxScale::from(size as f32);
    let (text_w, _) = text_size(scale, &*FONT, text);
    let (box_w, box_h) = (text_w as i32 + pad_x * 2, size + pad_y * 2);
    let margin = (width / 120).max(6);
    let (x, y) = ((width - box_w - margin).max(0), (height - box_h - margin).max(0));
    shade(image, x, y, box_w, box_h, [17, 17, 17], 0.72);
    draw_text_mut(image, Rgb([249, 250, 251]), x + pad_x, y + pad_y, scale, &*FONT, text);
}

/// A plate or a weight: large white text on black with a white border, as
/// the window shows them over the live picture.
fn chip(image: &mut RgbImage, width: i32, height: i32, text: &str, corner: Corner, stack: i32) {
    let size = (width / 22).max(24);
    let (pad_x, pad_y) = ((size as f32 * 0.45).round() as i32, (size as f32 * 0.25).round() as i32);
    let border = (size / 12).max(2);
    let scale = PxScale::from(size as f32);
    let (text_w, _) = text_size(scale, &*FONT, text);
    let (box_w, box_h) = (text_w as i32 + pad_x * 2, size + pad_y * 2);
    let margin = (width / 80).max(8);
    // Bottom right already carries the clock, so there the chip sits above it.
    let clock_clearance = if corner == Corner::BottomRight { (width / 70) * 2 + margin } else { 0 };
    let shift = stack * (box_h + margin / 2);
    let x = if corner.right() { (width - box_w - margin).max(0) } else { margin };
    let y = if corner.bottom() { (height - box_h - margin - clock_clearance - shift).max(0) } else { margin + shift };
    draw_filled_rect_mut(image, Rect::at(x, y).of_size(box_w as u32, box_h as u32), Rgb([255, 255, 255]));
    let inner = Rect::at(x + border, y + border).of_size((box_w - 2 * border).max(1) as u32, (box_h - 2 * border).max(1) as u32);
    draw_filled_rect_mut(image, inner, Rgb([11, 11, 11]));
    draw_text_mut(image, Rgb([255, 255, 255]), x + pad_x, y + pad_y, scale, &*FONT, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey(width: u32, height: u32) -> RgbImage {
        RgbImage::from_pixel(width, height, Rgb([128, 128, 128]))
    }

    fn stamp() -> Stamp {
        Stamp { taken_at: Local::now(), plate: Some(("5AU5341".into(), Corner::TopLeft)), weight: Some(("48,320 lb".into(), Corner::BottomLeft)) }
    }

    #[test]
    fn chips_land_in_their_corners_and_the_clock_bottom_right() {
        let mut image = grey(960, 480);
        draw(&mut image, &stamp());
        // A chip's white border is just inside the margin of its corner.
        let margin = 12;
        assert_eq!(image.get_pixel(margin + 1, margin + 1), &Rgb([255, 255, 255])); // plate, top left
        assert_eq!(image.get_pixel(margin + 1, 480 - margin - 2), &Rgb([255, 255, 255])); // weight, bottom left
        assert_ne!(image.get_pixel(950, 470), &Rgb([128, 128, 128])); // clock, bottom right
        assert_eq!(image.get_pixel(950, 10), &Rgb([128, 128, 128])); // top right untouched
    }

    #[test]
    fn a_weight_sharing_the_plates_corner_sits_beside_it() {
        let mut image = grey(960, 480);
        let s = Stamp { weight: Some(("48,320 lb".into(), Corner::TopLeft)), ..stamp() };
        draw(&mut image, &s);
        // Plate box: size 43, height 43 + 2*11 = 65, from y 12. The weight's starts below it.
        assert_eq!(image.get_pixel(13, 12 + 65 + 6 + 1), &Rgb([255, 255, 255]));
    }

    #[test]
    fn stamps_a_jpeg_and_keeps_its_size() {
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 90).encode_image(&grey(640, 360)).unwrap();
        let stamped = stamp_jpeg(&jpeg, &stamp(), 85).unwrap();
        assert_eq!(crate::camera::snapshot::jpeg_size(&stamped), Some((640, 360)));
        assert!(stamp_jpeg(b"not a jpeg", &stamp(), 85).is_err());
    }

    #[test]
    fn corners_read_from_the_settings() {
        assert_eq!(Corner::named("top-right"), Corner::TopRight);
        assert_eq!(Corner::named("nonsense"), Corner::BottomLeft);
        assert_eq!(Corner::TopLeft.opposite(), Corner::TopRight);
    }
}
