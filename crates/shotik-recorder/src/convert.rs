//! Pixel conversion for the paths that run on the CPU: the software encoder and the thumbnail.

/// BT.709 limited range, the coefficients times 65536.
const YR: i32 = 11966;
const YG: i32 = 40254;
const YB: i32 = 4064;
const UR: i32 = -6596;
const UG: i32 = -22189;
const UB: i32 = 28784;
const VR: i32 = 28784;
const VG: i32 = -26145;
const VB: i32 = -2639;

/// Number of bytes of an NV12 picture without padding.
pub fn nv12_len(width: u32, height: u32) -> usize {
    width as usize * height as usize * 3 / 2
}

/// Top-down BGRA to NV12 with a stride equal to the width. Width and height must be even. Chroma is
/// the average of each 2x2 block.
pub fn bgra_to_nv12(bgra: &[u8], stride: usize, width: u32, height: u32, nv12: &mut [u8]) {
    let (w, h) = (width as usize, height as usize);
    debug_assert!(w % 2 == 0 && h % 2 == 0);
    let (luma, chroma) = nv12.split_at_mut(w * h);
    for (row, y_row) in luma.chunks_exact_mut(w).enumerate() {
        let src = &bgra[row * stride..row * stride + w * 4];
        for (px, y) in src.chunks_exact(4).zip(y_row.iter_mut()) {
            let (b, g, r) = (i32::from(px[0]), i32::from(px[1]), i32::from(px[2]));
            *y = (((YR * r + YG * g + YB * b + 32768) >> 16) + 16) as u8;
        }
    }
    for (row, uv_row) in chroma.chunks_exact_mut(w).enumerate() {
        let top = &bgra[row * 2 * stride..row * 2 * stride + w * 4];
        let bottom = &bgra[(row * 2 + 1) * stride..(row * 2 + 1) * stride + w * 4];
        for ((t, b), uv) in top.chunks_exact(8).zip(bottom.chunks_exact(8)).zip(uv_row.chunks_exact_mut(2)) {
            let avg = |i: usize| (i32::from(t[i]) + i32::from(t[i + 4]) + i32::from(b[i]) + i32::from(b[i + 4]) + 2) >> 2;
            let (blue, green, red) = (avg(0), avg(1), avg(2));
            uv[0] = (((UR * red + UG * green + UB * blue + 32768) >> 16) + 128) as u8;
            uv[1] = (((VR * red + VG * green + VB * blue + 32768) >> 16) + 128) as u8;
        }
    }
}

/// How a YUV picture encodes colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YuvMatrix {
    Bt709,
    Bt601,
}

/// Integer coefficients, times 1024: luma scale, then V to red, U and V to green, U to blue.
fn yuv_coefficients(matrix: YuvMatrix, full_range: bool) -> (i32, i32, i32, i32, i32, i32) {
    match (matrix, full_range) {
        (YuvMatrix::Bt709, false) => (1192, 1836, 218, 546, 2163, 16),
        (YuvMatrix::Bt709, true) => (1024, 1613, 192, 479, 1900, 0),
        (YuvMatrix::Bt601, false) => (1192, 1634, 401, 832, 2066, 16),
        (YuvMatrix::Bt601, true) => (1024, 1436, 352, 731, 1815, 0),
    }
}

/// NV12 to tightly packed top-down BGRX. `luma` and `chroma` start at the first visible row, `pitch`
/// bytes between rows of either plane; a chroma pair covers 2x2 pixels. `chroma_x` and `chroma_y` say
/// how the visible area starts relative to the chroma grid, for a crop on odd coordinates.
pub struct Nv12Planes<'a> {
    pub luma: &'a [u8],
    pub chroma: &'a [u8],
    pub pitch: usize,
    pub width: usize,
    pub height: usize,
}

pub fn nv12_to_bgrx(planes: &Nv12Planes<'_>, matrix: YuvMatrix, full_range: bool, out: &mut Vec<u8>) {
    let (scale, rv, gu, gv, bu, black) = yuv_coefficients(matrix, full_range);
    let (w, h, pitch) = (planes.width, planes.height, planes.pitch);
    out.resize(w * h * 4, 0);
    for (row, target) in out.chunks_exact_mut(w * 4).enumerate() {
        let luma = &planes.luma[row * pitch..row * pitch + w];
        let chroma_row = (row / 2) * pitch;
        let chroma = &planes.chroma[chroma_row..chroma_row + w.div_ceil(2) * 2];
        for (x, (px, y)) in target.chunks_exact_mut(4).zip(luma).enumerate() {
            let pair = (x / 2) * 2;
            let u = i32::from(chroma[pair]) - 128;
            let v = i32::from(chroma[pair + 1]) - 128;
            let luma_term = (i32::from(*y) - black) * scale + 512;
            px[0] = ((luma_term + bu * u) >> 10).clamp(0, 255) as u8;
            px[1] = ((luma_term - gu * u - gv * v) >> 10).clamp(0, 255) as u8;
            px[2] = ((luma_term + rv * v) >> 10).clamp(0, 255) as u8;
            px[3] = 255;
        }
    }
}

/// JPEG of a top-down BGRA picture, at the quality `recorder.js` passes to `toBlob`.
pub fn bgra_to_jpeg(bgra: &[u8], stride: usize, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for row in bgra.chunks_exact(stride).take(height as usize) {
        for px in row[..width as usize * 4].chunks_exact(4) {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82)
        .encode(&rgb, width, height, image::ExtendedColorType::Rgb8)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, b: u8, g: u8, r: u8) -> Vec<u8> {
        std::iter::repeat_n([b, g, r, 255], (width * height) as usize).flatten().collect()
    }

    fn convert(b: u8, g: u8, r: u8) -> (u8, u8, u8) {
        let bgra = solid(4, 2, b, g, r);
        let mut nv12 = vec![0; nv12_len(4, 2)];
        bgra_to_nv12(&bgra, 16, 4, 2, &mut nv12);
        assert!(nv12[..8].iter().all(|&y| y == nv12[0]));
        (nv12[0], nv12[8], nv12[9])
    }

    #[test]
    fn known_colours_map_to_bt709_limited_range() {
        assert_eq!(convert(0, 0, 0), (16, 128, 128));
        assert_eq!(convert(255, 255, 255), (235, 128, 128));
        assert_eq!(convert(0, 0, 255), (63, 102, 240), "red");
        assert_eq!(convert(0, 255, 0), (173, 42, 26), "green");
        assert_eq!(convert(255, 0, 0), (32, 240, 118), "blue");
    }

    #[test]
    fn chroma_averages_the_block_and_luma_keeps_each_pixel() {
        let mut bgra = solid(2, 2, 0, 0, 0);
        bgra[..4].copy_from_slice(&[255, 255, 255, 255]);
        let mut nv12 = vec![0; nv12_len(2, 2)];
        bgra_to_nv12(&bgra, 8, 2, 2, &mut nv12);
        assert_eq!(&nv12[..4], &[235, 16, 16, 16]);
        assert_eq!(&nv12[4..], &[128, 128]);
    }

    #[test]
    fn a_stride_wider_than_the_picture_is_respected() {
        let mut bgra = vec![255_u8; 64];
        for row in bgra.chunks_exact_mut(32) {
            row[..8].copy_from_slice(&[0, 0, 0, 255, 0, 0, 0, 255]);
        }
        let mut nv12 = vec![0; nv12_len(2, 2)];
        bgra_to_nv12(&bgra, 32, 2, 2, &mut nv12);
        assert_eq!(&nv12[..4], &[16, 16, 16, 16]);
    }

    fn back_to_rgb(y: u8, u: u8, v: u8, matrix: YuvMatrix, full: bool) -> (u8, u8, u8) {
        let (luma, chroma) = ([y, y, y, y], [u, v]);
        let planes = Nv12Planes { luma: &luma, chroma: &chroma, pitch: 2, width: 2, height: 2 };
        let mut out = Vec::new();
        nv12_to_bgrx(&planes, matrix, full, &mut out);
        assert_eq!(out.len(), 16);
        (out[2], out[1], out[0])
    }

    fn close(a: (u8, u8, u8), b: (u8, u8, u8)) -> bool {
        a.0.abs_diff(b.0) <= 2 && a.1.abs_diff(b.1) <= 2 && a.2.abs_diff(b.2) <= 2
    }

    #[test]
    fn nv12_comes_back_to_the_colours_it_was_made_from() {
        for (b, g, r) in [(0, 0, 0), (255, 255, 255), (0, 0, 255), (0, 255, 0), (255, 0, 0), (0x1c, 0x14, 0x12), (0xff, 0x5c, 0x7c)] {
            let (y, u, v) = convert(b, g, r);
            let back = back_to_rgb(y, u, v, YuvMatrix::Bt709, false);
            assert!(close(back, (r, g, b)), "{:?} became {back:?}", (r, g, b));
        }
    }

    #[test]
    fn the_matrix_and_the_range_change_the_colour() {
        // red in BT.601 limited range and in BT.709 full range
        assert!(close(back_to_rgb(81, 90, 240, YuvMatrix::Bt601, false), (255, 0, 0)));
        assert!(close(back_to_rgb(54, 99, 255, YuvMatrix::Bt709, true), (255, 0, 0)));
        assert!(close(back_to_rgb(76, 85, 255, YuvMatrix::Bt601, true), (255, 0, 0)));
        assert!(!close(back_to_rgb(81, 90, 240, YuvMatrix::Bt709, false), (255, 0, 0)));
    }

    #[test]
    fn jpeg_is_a_decodable_picture_of_the_input() {
        let bgra = solid(16, 8, 0x1c, 0x14, 0x12);
        let jpeg = bgra_to_jpeg(&bgra, 64, 16, 8).unwrap();
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
        let decoded = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (16, 8));
        let px = decoded.get_pixel(8, 4);
        assert!(px[0].abs_diff(0x12) < 3 && px[1].abs_diff(0x14) < 3 && px[2].abs_diff(0x1c) < 3, "{px:?}");
    }
}
