//! People's pictures (a contact's avatar): an uploaded PNG, JPEG or WebP is turned the way a phone
//! took it, cut to its centre square, made at most [`SIDE`] pixels wide and stored again as a small
//! JPEG (PNG when it has transparency) among the attachments (`files.rs`). The contact keeps only the
//! file's id (`avatar: {"file": …}`); the picture is served by `GET /api/files/:id`.

use std::io::Cursor;

use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};

use crate::backend::{BResult, BackendError};
use crate::files;

/// The largest upload taken (the stored picture is far smaller).
pub const MAX_UPLOAD: usize = 10 * 1024 * 1024;
/// The side of the stored square.
pub const SIDE: u32 = 256;
/// A picture wider or taller than this is refused before it is decoded.
const MAX_DIM: u32 = 12_000;

fn refuse(status: u16, message: String) -> BackendError {
    BackendError { status, message }
}

/// The stored picture: its bytes and their extension (`jpg` or `png`). `415` for any type but PNG,
/// JPEG and WebP (by the bytes, not the name), `400` for bytes that do not decode, `413` too large.
pub fn square(b: &[u8]) -> BResult<(Vec<u8>, &'static str)> {
    if b.len() > MAX_UPLOAD {
        return Err(refuse(
            413,
            steve_i18n::t!("hub.avatar.too_large", mb = MAX_UPLOAD / 1024 / 1024),
        ));
    }
    let format = match files::sniff(b).map(|k| k.mime) {
        Some("image/png") => ImageFormat::Png,
        Some("image/jpeg") => ImageFormat::Jpeg,
        Some("image/webp") => ImageFormat::WebP,
        _ => return Err(refuse(415, steve_i18n::t!("hub.avatar.not_image"))),
    };
    let unreadable = |_| refuse(400, steve_i18n::t!("hub.avatar.unreadable"));
    let mut reader = ImageReader::with_format(Cursor::new(b), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIM);
    limits.max_image_height = Some(MAX_DIM);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(unreadable)?;
    let orientation = decoder.orientation().map_err(unreadable)?;
    let mut img = DynamicImage::from_decoder(decoder).map_err(unreadable)?;
    img.apply_orientation(orientation);
    let (w, h) = (img.width(), img.height());
    let s = w.min(h);
    if s == 0 {
        return Err(refuse(400, steve_i18n::t!("hub.avatar.unreadable")));
    }
    let mut img = img.crop_imm((w - s) / 2, (h - s) / 2, s, s);
    if s > SIDE {
        img = img.resize_exact(SIDE, SIDE, image::imageops::FilterType::Lanczos3);
    }
    let see_through = img.color().has_alpha() && img.to_rgba8().pixels().any(|p| p.0[3] < 255);
    let mut out = Vec::new();
    if see_through {
        DynamicImage::ImageRgba8(img.to_rgba8())
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .map_err(|e| BackendError::msg(e.to_string()))?;
        Ok((out, "png"))
    } else {
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88)
            .encode_image(&img.to_rgb8())
            .map_err(|e| BackendError::msg(e.to_string()))?;
        Ok((out, "jpg"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    fn encode(img: DynamicImage, f: ImageFormat) -> Vec<u8> {
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), f).unwrap();
        out
    }

    fn decode(b: &[u8]) -> DynamicImage {
        image::load_from_memory(b).unwrap()
    }

    #[test]
    fn a_wide_photo_becomes_its_centre_square_at_256() {
        // left third red, middle green, right third blue: the square keeps the green middle
        let img = RgbImage::from_fn(1200, 400, |x, _| match x {
            0..400 => Rgb([255, 0, 0]),
            400..800 => Rgb([0, 255, 0]),
            _ => Rgb([0, 0, 255]),
        });
        let (out, ext) = square(&encode(img.into(), ImageFormat::Png)).unwrap();
        assert_eq!(ext, "jpg");
        assert_eq!(files::sniff(&out).unwrap().mime, "image/jpeg");
        let got = decode(&out).to_rgb8();
        assert_eq!(got.dimensions(), (SIDE, SIDE));
        let c = got.get_pixel(128, 128).0;
        assert!(
            c[1] > 200 && c[0] < 60 && c[2] < 60,
            "centre is green: {c:?}"
        );
        assert!(out.len() < 60 * 1024);
    }

    #[test]
    fn a_small_picture_is_not_blown_up_and_transparency_stays_png() {
        let img = RgbaImage::from_fn(40, 60, |x, _| {
            if x < 20 {
                Rgba([0, 0, 0, 0])
            } else {
                Rgba([10, 20, 30, 255])
            }
        });
        let (out, ext) = square(&encode(img.into(), ImageFormat::Png)).unwrap();
        assert_eq!(ext, "png");
        let got = decode(&out);
        assert_eq!((got.width(), got.height()), (40, 40));
        assert_eq!(got.to_rgba8().get_pixel(0, 0).0[3], 0);
    }

    #[test]
    fn jpeg_and_webp_are_taken() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(300, 500, Rgb([200, 100, 50])));
        let (out, _) = square(&encode(img.clone(), ImageFormat::Jpeg)).unwrap();
        assert_eq!(decode(&out).width(), SIDE);
        let (out, ext) = square(&encode(img, ImageFormat::WebP)).unwrap();
        assert_eq!((decode(&out).width(), ext), (SIDE, "jpg"));
    }

    #[test]
    fn other_types_and_broken_bytes_are_refused() {
        assert_eq!(square(b"GIF89a\x01\x00\x01\x00").unwrap_err().status, 415);
        assert_eq!(square(b"<svg onload=alert(1)>").unwrap_err().status, 415);
        assert_eq!(square(b"").unwrap_err().status, 415);
        // a PNG signature with nothing after it
        let broken = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3];
        assert_eq!(square(&broken).unwrap_err().status, 400);
        let big = vec![0u8; MAX_UPLOAD + 1];
        assert_eq!(square(&big).unwrap_err().status, 413);
    }

    #[test]
    fn a_huge_canvas_is_refused_before_decoding() {
        // a valid PNG header claiming 20000×20000 pixels
        let mut png = Vec::new();
        let enc = image::codecs::png::PngEncoder::new(&mut png);
        image::ImageEncoder::write_image(enc, &vec![0u8; 3], 1, 1, image::ExtendedColorType::Rgb8)
            .unwrap();
        // patch the IHDR width/height (bytes 16..24); the CRC no longer matches, either way it is refused
        png[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        png[20..24].copy_from_slice(&20_000u32.to_be_bytes());
        assert_eq!(square(&png).unwrap_err().status, 400);
    }
}
