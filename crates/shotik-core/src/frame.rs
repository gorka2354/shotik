//! A captured image in memory, and the encoding everything needs.

use std::sync::Arc;

use fast_image_resize as fr;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder};

use crate::error::{Error, Result};
use crate::geom::Rect;

/// Tightly packed BGRA8, top row first. Cloning shares the pixels.
#[derive(Clone)]
pub struct Frame {
    width: u32,
    height: u32,
    bgra: Arc<Vec<u8>>,
}

impl Frame {
    pub fn new(width: u32, height: u32, bgra: Vec<u8>) -> Result<Self> {
        let expected = u64::from(width) * u64::from(height) * 4;
        if bgra.len() as u64 != expected {
            return Err(Error::msg(format!(
                "frame {width}x{height} needs {expected} bytes, got {}",
                bgra.len()
            )));
        }
        Ok(Self { width, height, bgra: Arc::new(bgra) })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn bgra(&self) -> &[u8] {
        &self.bgra
    }

    pub fn from_png(png: &[u8]) -> Result<Self> {
        let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .map_err(|e| Error::msg(format!("cannot decode PNG: {e}")))?
            .into_rgba8();
        let (width, height) = image.dimensions();
        let mut bgra = image.into_raw();
        swap_red_blue(&mut bgra);
        Self::new(width, height, bgra)
    }

    /// Fast compression: this runs on the capture path, and a 4K frame must not take a second.
    pub fn to_png(&self) -> Result<Vec<u8>> {
        let mut rgba = self.bgra.as_ref().clone();
        swap_red_blue(&mut rgba);
        let mut out = Vec::with_capacity(rgba.len() / 4);
        PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive)
            .write_image(&rgba, self.width, self.height, ExtendedColorType::Rgba8)
            .map_err(|e| Error::msg(format!("cannot encode PNG: {e}")))?;
        Ok(out)
    }

    /// `quality`: 1 to 100. Alpha is dropped.
    pub fn to_jpeg(&self, quality: u8) -> Result<Vec<u8>> {
        let mut rgb = Vec::with_capacity(self.bgra.len() / 4 * 3);
        for px in self.bgra.chunks_exact(4) {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        }
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, quality)
            .write_image(&rgb, self.width, self.height, ExtendedColorType::Rgb8)
            .map_err(|e| Error::msg(format!("cannot encode JPEG: {e}")))?;
        Ok(out)
    }

    /// The part of the frame inside `rect`, clipped to its edges. An empty result is an error, not
    /// an empty image: a zero-size crop downstream becomes an unreadable file.
    pub fn crop(&self, rect: Rect) -> Result<Self> {
        let x = rect.x.max(0) as u32;
        let y = rect.y.max(0) as u32;
        let right = (i64::from(rect.x) + i64::from(rect.width)).clamp(0, i64::from(self.width)) as u32;
        let bottom = (i64::from(rect.y) + i64::from(rect.height)).clamp(0, i64::from(self.height)) as u32;
        if x >= right || y >= bottom {
            return Err(Error::msg(format!(
                "crop {}x{} at {},{} is outside the {}x{} frame",
                rect.width, rect.height, rect.x, rect.y, self.width, self.height
            )));
        }
        let (w, h) = (right - x, bottom - y);
        let stride = self.width as usize * 4;
        let mut out = Vec::with_capacity(w as usize * h as usize * 4);
        for row in y..bottom {
            let start = row as usize * stride + x as usize * 4;
            out.extend_from_slice(&self.bgra[start..start + w as usize * 4]);
        }
        Self::new(w, h, out)
    }

    /// High-quality resize.
    pub fn resize(&self, width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::msg(format!("cannot resize to {width}x{height}")));
        }
        let src = fr::images::ImageRef::new(self.width, self.height, &self.bgra, fr::PixelType::U8x4)
            .map_err(|e| Error::msg(format!("cannot read frame for resize: {e}")))?;
        let mut dst = fr::images::Image::new(width, height, fr::PixelType::U8x4);
        let options = fr::ResizeOptions::new()
            .resize_alg(fr::ResizeAlg::Convolution(fr::FilterType::Lanczos3))
            .use_alpha(false);
        fr::Resizer::new()
            .resize(&src, &mut dst, &options)
            .map_err(|e| Error::msg(format!("cannot resize frame: {e}")))?;
        Self::new(width, height, dst.into_vec())
    }

    /// Shrinks so the long side is at most `max_side`; never enlarges. A cheap clone when it already fits.
    pub fn fit_within(&self, max_side: u32) -> Result<Self> {
        let long = self.width.max(self.height);
        if long <= max_side {
            return Ok(self.clone());
        }
        let k = f64::from(max_side) / f64::from(long);
        let w = ((f64::from(self.width) * k).round() as u32).max(1);
        let h = ((f64::from(self.height) * k).round() as u32).max(1);
        self.resize(w, h)
    }
}

fn swap_red_blue(pixels: &mut [u8]) {
    for px in pixels.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Frame({}x{})", self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2x2: blue, green / red, white.
    fn sample() -> Frame {
        Frame::new(2, 2, vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]).unwrap()
    }

    #[test]
    fn rejects_a_buffer_of_the_wrong_size() {
        assert!(Frame::new(2, 2, vec![0; 16]).is_ok());
        assert!(Frame::new(2, 2, vec![0; 15]).is_err());
    }

    #[test]
    fn png_round_trip_keeps_colours_in_place() {
        let back = Frame::from_png(&sample().to_png().unwrap()).unwrap();
        assert_eq!(back.bgra(), sample().bgra());
    }

    #[test]
    fn crop_takes_the_right_pixels_and_clips_to_the_edge() {
        let c = sample().crop(Rect::new(1, 1, 5, 5)).unwrap();
        assert_eq!((c.width(), c.height()), (1, 1));
        assert_eq!(c.bgra(), &[255, 255, 255, 255]);
    }

    #[test]
    fn a_crop_outside_the_frame_is_an_error() {
        assert!(sample().crop(Rect::new(5, 5, 2, 2)).is_err());
        assert!(sample().crop(Rect::new(0, 0, 0, 2)).is_err());
    }

    #[test]
    fn fit_within_shrinks_the_long_side_and_never_enlarges() {
        let big = Frame::new(4000, 2000, vec![0; 4000 * 2000 * 4]).unwrap();
        let fit = big.fit_within(1600).unwrap();
        assert_eq!((fit.width(), fit.height()), (1600, 800));
        let same = fit.fit_within(1600).unwrap();
        assert_eq!((same.width(), same.height()), (1600, 800));
    }

    #[test]
    fn jpeg_is_a_jpeg() {
        assert_eq!(&sample().to_jpeg(82).unwrap()[..2], &[0xFF, 0xD8]);
    }
}
