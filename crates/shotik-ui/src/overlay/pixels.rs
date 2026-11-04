//! Pixel loops on the way to the first frame. A 4K frame goes through here on every capture, so the
//! conversion has an AVX2 path; the scalar path defines the result and tests hold both to it.
//!
//! The frozen frame is kept twice, as RGB because that is Slint's cheapest image to draw: once as it is and
//! once under the veil. Baking the veil (`rgba(6, 8, 13, 0.45)` in `overlay.js`) instead of drawing it as
//! rectangles keeps a selection drag from repainting the whole window: only the hole moves.

/// Veil weight out of 256. Frame weight is the rest.
const VEIL_ALPHA: u32 = 115;
const FRAME_ALPHA: u32 = 256 - VEIL_ALPHA;
/// Veil colour.
const VEIL_RGB: [u32; 3] = [6, 8, 13];

#[inline]
fn veiled(value: u8, channel: usize) -> u8 {
    ((u32::from(value) * FRAME_ALPHA + VEIL_RGB[channel] * VEIL_ALPHA + 128) >> 8) as u8
}

/// Bytes of an RGB image of this size.
pub fn rgb_len(width: u32, height: u32) -> usize {
    width as usize * height as usize * 3
}

/// Converts tightly packed BGRA to RGB, and fills `dim` with the same picture under the veil.
pub fn bgra_to_rgb_and_veil(bgra: &[u8], rgb: &mut [u8], dim: &mut [u8]) {
    let pixels = bgra.len() / 4;
    assert_eq!(rgb.len(), pixels * 3, "RGB buffer size");
    assert_eq!(dim.len(), pixels * 3, "veiled buffer size");
    let mut done = 0;
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the check above found AVX2; the sizes were asserted.
        done = unsafe { avx2::convert(bgra, rgb, dim) };
    }
    convert_scalar(&bgra[done * 4..], &mut rgb[done * 3..], &mut dim[done * 3..]);
}

fn convert_scalar(bgra: &[u8], rgb: &mut [u8], dim: &mut [u8]) {
    for ((src, out), veil) in bgra.chunks_exact(4).zip(rgb.chunks_exact_mut(3)).zip(dim.chunks_exact_mut(3)) {
        out[0] = src[2];
        out[1] = src[1];
        out[2] = src[0];
        veil[0] = veiled(src[2], 0);
        veil[1] = veiled(src[1], 1);
        veil[2] = veiled(src[0], 2);
    }
}

/// `dst` is `src` under the veil. Both RGB.
pub fn veil_rgb(src: &[u8], dst: &mut [u8]) {
    for (s, d) in src.chunks_exact(3).zip(dst.chunks_exact_mut(3)) {
        d[0] = veiled(s[0], 0);
        d[1] = veiled(s[1], 1);
        d[2] = veiled(s[2], 2);
    }
}

/// An RGB image borrowed from wherever it lives.
#[derive(Clone, Copy)]
pub struct RgbImage<'a> {
    pub data: &'a [u8],
    pub width: i32,
    pub height: i32,
}

impl RgbImage<'_> {
    /// One pixel. The coordinates must be inside.
    pub fn pixel(&self, x: i32, y: i32) -> [u8; 3] {
        let at = (y as usize * self.width as usize + x as usize) * 3;
        [self.data[at], self.data[at + 1], self.data[at + 2]]
    }

    /// `#RRGGBB` of one pixel, as the magnifier and the colour picker show it.
    pub fn hex_at(&self, x: i32, y: i32) -> String {
        let [r, g, b] = self.pixel(x, y);
        format!("#{r:02X}{g:02X}{b:02X}")
    }
}

/// The magnifier's picture at device resolution: 13 by 13 source pixels around a point, grid and centre
/// marker, rounded corners. RGBA premultiplied. Pixels outside the frame stay transparent, like the JS canvas.
pub fn magnifier(image: RgbImage<'_>, cx: i32, cy: i32, side: usize, scale: f64, accent: [u8; 3], out: &mut [u8]) {
    const CELLS: i32 = 13;
    debug_assert_eq!(out.len(), side * side * 4);
    let cell = side as f64 / f64::from(CELLS);
    // Grid lines and marker edges are in device pixels, one and two CSS pixels wide.
    let line = scale.round().max(1.0) as usize;
    let marker = (2.0 * scale).round().max(1.0) as usize;
    let edge = |index: i32| (f64::from(index) * cell).round() as usize;
    let marker_start = edge(6);
    let marker_end = edge(7);

    let radius = 8.0 * scale;
    for y in 0..side {
        let row_cell = (y as f64 / cell) as i32;
        let on_row_line = (1..CELLS).any(|i| {
            let e = edge(i);
            y >= e && y < e + line
        });
        for x in 0..side {
            let column_cell = (x as f64 / cell) as i32;
            let at = (y * side + x) * 4;
            let (sx, sy) = (cx - CELLS / 2 + column_cell, cy - CELLS / 2 + row_cell);
            if sx < 0 || sy < 0 || sx >= image.width || sy >= image.height {
                out[at..at + 4].fill(0);
            } else {
                let [mut r, mut g, mut b] = image.pixel(sx, sy);
                let on_column_line = (1..CELLS).any(|i| {
                    let e = edge(i);
                    x >= e && x < e + line
                });
                if on_row_line || on_column_line {
                    // rgba(255,255,255,0.10)
                    r = r + ((255 - u32::from(r)) * 26 / 255) as u8;
                    g = g + ((255 - u32::from(g)) * 26 / 255) as u8;
                    b = b + ((255 - u32::from(b)) * 26 / 255) as u8;
                }
                out[at] = r;
                out[at + 1] = g;
                out[at + 2] = b;
                out[at + 3] = 255;
            }
            let in_marker_box = x + marker / 2 >= marker_start
                && x < marker_end + marker / 2
                && y + marker / 2 >= marker_start
                && y < marker_end + marker / 2;
            let in_marker_hole = x >= marker_start + marker / 2
                && x + marker / 2 < marker_end
                && y >= marker_start + marker / 2
                && y + marker / 2 < marker_end;
            if in_marker_box && !in_marker_hole {
                out[at] = accent[0];
                out[at + 1] = accent[1];
                out[at + 2] = accent[2];
                out[at + 3] = 255;
            }
            // Rounded corners: coverage from the distance to the corner's centre.
            let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
            let far = side as f64;
            let dx = (radius - fx).max(fx - (far - radius));
            let dy = (radius - fy).max(fy - (far - radius));
            if dx > 0.0 && dy > 0.0 {
                let coverage = (radius - dx.hypot(dy) + 0.5).clamp(0.0, 1.0);
                if coverage < 1.0 {
                    for channel in &mut out[at..at + 4] {
                        *channel = (f64::from(*channel) * coverage) as u8;
                    }
                }
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod avx2 {
    use std::arch::x86_64::*;

    /// Returns how many pixels it converted. It leaves a tail: every store writes eight bytes more than the
    /// six pixels' worth it keeps, which must stay inside the output.
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn convert(bgra: &[u8], rgb: &mut [u8], dim: &mut [u8]) -> usize {
        let pixels = bgra.len() / 4;
        if pixels < 12 {
            return 0;
        }
        let full = pixels - 4;
        let mut done = 0;

        // Each 128-bit lane: B G R A x4 -> R G B x4, then four unused bytes.
        let shuffle = _mm256_setr_epi8(
            2, 1, 0, 6, 5, 4, 10, 9, 8, 14, 13, 12, -1, -1, -1, -1, //
            2, 1, 0, 6, 5, 4, 10, 9, 8, 14, 13, 12, -1, -1, -1, -1,
        );
        // Closes the gap between the lanes.
        let compact = _mm256_setr_epi32(0, 1, 2, 4, 5, 6, 7, 7);

        let frame = _mm256_set1_epi16(super::FRAME_ALPHA as i16);
        let [r, g, b] = super::VEIL_RGB.map(|c| (c * super::VEIL_ALPHA + 128) as i16);
        // 16-bit lanes of a pixel, low byte first: B R after masking, G A after shifting.
        let add_blue_red = _mm256_set1_epi32((i32::from(r) << 16) | i32::from(b));
        let add_green_alpha = _mm256_set1_epi32(i32::from(g));
        let low_bytes = _mm256_set1_epi16(0x00FF);

        while done + 8 <= full {
            // SAFETY: at least 8 pixels remain in the input and 12 in each output.
            unsafe {
                let px = _mm256_loadu_si256(bgra.as_ptr().add(done * 4).cast());

                let out = _mm256_permutevar8x32_epi32(_mm256_shuffle_epi8(px, shuffle), compact);
                _mm256_storeu_si256(rgb.as_mut_ptr().add(done * 3).cast(), out);

                let blue_red = _mm256_and_si256(px, low_bytes);
                let green_alpha = _mm256_srli_epi16(px, 8);
                let blue_red = _mm256_srli_epi16(_mm256_add_epi16(_mm256_mullo_epi16(blue_red, frame), add_blue_red), 8);
                let green_alpha = _mm256_srli_epi16(_mm256_add_epi16(_mm256_mullo_epi16(green_alpha, frame), add_green_alpha), 8);
                let veiled = _mm256_or_si256(blue_red, _mm256_slli_epi16(green_alpha, 8));

                let out = _mm256_permutevar8x32_epi32(_mm256_shuffle_epi8(veiled, shuffle), compact);
                _mm256_storeu_si256(dim.as_mut_ptr().add(done * 3).cast(), out);
            }
            done += 8;
        }
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_bgra(pixels: usize) -> Vec<u8> {
        (0..pixels * 4).map(|i| (i * 37 % 251) as u8).collect()
    }

    #[test]
    fn swaps_channels_and_ignores_alpha() {
        let bgra = [10, 20, 30, 0, 40, 50, 60, 255];
        let (mut rgb, mut dim) = (vec![0; 6], vec![0; 6]);
        bgra_to_rgb_and_veil(&bgra, &mut rgb, &mut dim);
        assert_eq!(rgb, [30, 20, 10, 60, 50, 40]);
    }

    #[test]
    fn the_veil_is_the_css_colour_at_45_percent() {
        let expected = |value: f64, veil: f64| (value * 0.55 + veil * 0.45).round() as i32;
        for (value, channel, veil) in [(255u8, 0usize, 6.0), (0, 1, 8.0), (128, 2, 13.0), (255, 2, 13.0), (0, 0, 6.0)] {
            let got = i32::from(veiled(value, channel));
            assert!((got - expected(f64::from(value), veil)).abs() <= 1, "{value} {channel}: {got}");
        }
    }

    #[test]
    fn the_fast_path_agrees_with_the_scalar_path_for_every_length() {
        for pixels in [0, 1, 3, 11, 12, 13, 15, 16, 17, 31, 32, 33, 100, 1027] {
            let bgra = sample_bgra(pixels);
            let (mut rgb, mut dim) = (vec![0; pixels * 3], vec![0; pixels * 3]);
            bgra_to_rgb_and_veil(&bgra, &mut rgb, &mut dim);
            let (mut rgb_scalar, mut dim_scalar) = (vec![0; pixels * 3], vec![0; pixels * 3]);
            convert_scalar(&bgra, &mut rgb_scalar, &mut dim_scalar);
            assert_eq!(rgb, rgb_scalar, "RGB, {pixels} pixels");
            assert_eq!(dim, dim_scalar, "veil, {pixels} pixels");
        }
    }

    #[test]
    fn every_value_on_every_channel_agrees() {
        let bgra: Vec<u8> = (0..=255u8).flat_map(|v| [v, v, v, 0]).collect();
        let (mut rgb, mut dim) = (vec![0; 256 * 3], vec![0; 256 * 3]);
        bgra_to_rgb_and_veil(&bgra, &mut rgb, &mut dim);
        let (mut rgb_scalar, mut dim_scalar) = (vec![0; 256 * 3], vec![0; 256 * 3]);
        convert_scalar(&bgra, &mut rgb_scalar, &mut dim_scalar);
        assert_eq!(dim, dim_scalar);
    }

    #[test]
    fn veiling_rgb_matches_the_conversion() {
        let bgra = sample_bgra(50);
        let (mut rgb, mut dim) = (vec![0; 150], vec![0; 150]);
        bgra_to_rgb_and_veil(&bgra, &mut rgb, &mut dim);
        let mut again = vec![0; 150];
        veil_rgb(&rgb, &mut again);
        assert_eq!(again, dim);
    }

    #[test]
    fn hex_reads_the_right_pixel() {
        let data = [0, 0, 0, 0x3E, 0x9E, 0xFF, 1, 2, 3, 4, 5, 6];
        let image = RgbImage { data: &data, width: 2, height: 2 };
        assert_eq!(image.hex_at(1, 0), "#3E9EFF");
        assert_eq!(image.hex_at(0, 1), "#010203");
    }

    fn checker(width: i32, height: i32) -> Vec<u8> {
        let mut data = Vec::new();
        for y in 0..height {
            for x in 0..width {
                data.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, 40]);
            }
        }
        data
    }

    #[test]
    fn the_magnifier_shows_the_cursor_pixel_in_the_centre_cell() {
        let data = checker(100, 100);
        let image = RgbImage { data: &data, width: 100, height: 100 };
        let side = 143;
        let mut out = vec![0; side * side * 4];
        magnifier(image, 50, 60, side, 1.0, [1, 2, 3], &mut out);
        // Centre of the centre cell, away from grid and marker.
        let at = (71 * side + 71) * 4;
        assert_eq!(&out[at..at + 4], &[50, 60, 40, 255]);
        // Centre of the top-left cell: six source pixels up and left.
        let at = (5 * side + 5) * 4;
        assert_eq!(&out[at..at + 4], &[44, 54, 40, 255]);
        // On the centre cell's outline: the accent marker.
        let at = (66 * side + 71) * 4;
        assert_eq!(&out[at..at + 4], &[1, 2, 3, 255]);
        // The corner is rounded away.
        assert_eq!(&out[..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn the_magnifier_leaves_pixels_outside_the_frame_transparent() {
        let data = checker(20, 20);
        let image = RgbImage { data: &data, width: 20, height: 20 };
        let side = 143;
        let mut out = vec![9; side * side * 4];
        magnifier(image, 0, 0, side, 1.0, [1, 2, 3], &mut out);
        let at = (20 * side + 20) * 4;
        assert_eq!(out[at + 3], 0, "up and left of the frame");
        let at = (71 * side + 71) * 4;
        assert_eq!(&out[at..at + 4], &[0, 0, 40, 255]);
    }
}
