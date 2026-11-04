//! Decoding a video with the Media Foundation Source Reader. NV12 when the decoder gives it, which
//! leaves the colour conversion to `convert` with the matrix and range the stream states: the converter
//! the Source Reader inserts for RGB ignored the BT.709 of the file and shifted red by 8 dB of PSNR
//! against the same frames from ffmpeg. RGB32 only for sources that have no NV12.

use std::path::Path;

use shotik_core::{Error, Result};
use windows::core::{Interface, GUID, HSTRING};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFAttributes, IMFMediaType, IMFSample, IMFSourceReader, MFCreateAttributes, MFCreateMediaType,
    MFCreateSourceReaderFromURL, MFMediaType_Video, MFNominalRange_0_255, MFVideoArea, MFVideoFormat_NV12, MFVideoFormat_RGB32,
    MFVideoTransferMatrix_BT601, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE,
    MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX, MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
};

use super::com::{hr_error, Apartment, Context, MediaFoundation};
use crate::clock::TICKS_PER_SEC;
use crate::convert::{nv12_to_bgrx, Nv12Planes, YuvMatrix};
use crate::gif_export::{GifBuilder, Wanted};

/// What the decoder hands over, and the visible part of its surfaces: H.264 pads to whole macroblocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    left: usize,
    top: usize,
    width: usize,
    height: usize,
    frame_height: usize,
    /// Bytes between rows of a plain buffer, negative when it is bottom-up.
    stride: i32,
    /// None: RGB32.
    yuv: Option<(YuvMatrix, bool)>,
}

fn layout_of(media_type: &IMFMediaType, nv12: bool) -> Result<Layout> {
    // SAFETY: a query on a live object.
    let size = unsafe { media_type.GetUINT64(&MF_MT_FRAME_SIZE) }.ctx("MF_MT_FRAME_SIZE")?;
    let (frame_w, frame_h) = ((size >> 32) as usize, (size & 0xffff_ffff) as usize);
    // SAFETY: as above. RGB without a stated stride is bottom-up by the Media Foundation convention.
    let stride = unsafe { media_type.GetUINT32(&MF_MT_DEFAULT_STRIDE) }.map_or(
        if nv12 { frame_w as i32 } else { -((frame_w * 4) as i32) },
        |s| s as i32,
    );
    let yuv = nv12.then(|| {
        // SAFETY: as above. Without a statement BT.709 limited range: what recordings here always are.
        let matrix = unsafe { media_type.GetUINT32(&MF_MT_YUV_MATRIX) }.unwrap_or(0);
        let range = unsafe { media_type.GetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE) }.unwrap_or(0);
        let matrix = if matrix as i32 == MFVideoTransferMatrix_BT601.0 { YuvMatrix::Bt601 } else { YuvMatrix::Bt709 };
        (matrix, range as i32 == MFNominalRange_0_255.0)
    });
    let mut area = MFVideoArea::default();
    let area_size = std::mem::size_of::<MFVideoArea>();
    // SAFETY: the buffer is a local of the size passed: MFVideoArea is plain data.
    let aperture = unsafe {
        let bytes = std::slice::from_raw_parts_mut(std::ptr::from_mut(&mut area).cast::<u8>(), area_size);
        media_type.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, None)
    };
    if aperture.is_ok() && area.Area.cx > 0 && area.Area.cy > 0 {
        let (left, top) = (area.OffsetX.value.max(0) as usize, area.OffsetY.value.max(0) as usize);
        return Ok(Layout {
            left,
            top,
            width: (area.Area.cx as usize).min(frame_w.saturating_sub(left)),
            height: (area.Area.cy as usize).min(frame_h.saturating_sub(top)),
            frame_height: frame_h,
            stride,
            yuv,
        });
    }
    Ok(Layout { left: 0, top: 0, width: frame_w, height: frame_h, frame_height: frame_h, stride, yuv })
}

/// Copies the visible rows of an RGB32 surface, given its first row and the signed distance between rows.
///
/// # Safety
/// `scanline0` must point into a readable surface of `layout.frame_height` rows `pitch` apart, each
/// at least `layout.left + layout.width` pixels long.
unsafe fn copy_rgb_rows(scanline0: *const u8, pitch: i32, layout: &Layout, out: &mut Vec<u8>) {
    let row_bytes = layout.width * 4;
    out.resize(row_bytes * layout.height, 0);
    for (y, row) in out.chunks_exact_mut(row_bytes).enumerate() {
        let offset = (y + layout.top) as isize * pitch as isize + layout.left as isize * 4;
        // SAFETY: the layout was checked against the frame size, so the row lies inside the surface
        // whichever way it runs.
        unsafe { std::ptr::copy_nonoverlapping(scanline0.offset(offset), row.as_mut_ptr(), row_bytes) };
    }
}

/// Converts the visible part of an NV12 surface: the chroma plane follows `frame_height` rows of luma.
///
/// # Safety
/// `scanline0` must point to a readable surface of that layout, `pitch` bytes per row.
unsafe fn convert_nv12(scanline0: *const u8, pitch: usize, layout: &Layout, out: &mut Vec<u8>) {
    let (matrix, full_range) = layout.yuv.unwrap_or((YuvMatrix::Bt709, false));
    let total = pitch * layout.frame_height;
    // SAFETY: by the contract of this function: luma rows, then half as many rows of chroma.
    let (luma, chroma) = unsafe {
        (std::slice::from_raw_parts(scanline0, total), std::slice::from_raw_parts(scanline0.add(total), total / 2))
    };
    // the crop goes to whole chroma pairs: an odd offset does not occur for codecs with 4:2:0
    let (left, top) = (layout.left & !1, layout.top & !1);
    let planes = Nv12Planes {
        luma: &luma[top * pitch + left..],
        chroma: &chroma[(top / 2) * pitch + left..],
        pitch,
        width: layout.width,
        height: layout.height,
    };
    nv12_to_bgrx(&planes, matrix, full_range, out);
}

/// The visible rows of a decoded sample as tight top-down BGRX.
fn copy_frame(sample: &IMFSample, layout: &Layout, out: &mut Vec<u8>) -> Result<()> {
    // SAFETY: a call on a live object, returning its own reference.
    let buffer = unsafe { sample.ConvertToContiguousBuffer() }.ctx("ConvertToContiguousBuffer")?;
    if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
        let (mut scanline0, mut pitch) = (std::ptr::null_mut(), 0_i32);
        // SAFETY: the out values are locals and stay valid until Unlock2D. Lock2D describes a surface of
        // the frame size; NV12 is never bottom-up, RGB follows the sign of the pitch.
        unsafe {
            two_d.Lock2D(&mut scanline0, &mut pitch).ctx("IMF2DBuffer::Lock2D")?;
            if layout.yuv.is_some() {
                convert_nv12(scanline0, pitch.unsigned_abs() as usize, layout, out);
            } else {
                copy_rgb_rows(scanline0, pitch, layout, out);
            }
            return two_d.Unlock2D().ctx("IMF2DBuffer::Unlock2D");
        }
    }
    let (mut data, mut length) = (std::ptr::null_mut(), 0_u32);
    // SAFETY: the out values are locals and stay valid until Unlock.
    unsafe { buffer.Lock(&mut data, None, Some(&mut length)) }.ctx("IMFMediaBuffer::Lock")?;
    let row = layout.stride.unsigned_abs() as usize;
    let needed = if layout.yuv.is_some() { row * layout.frame_height * 3 / 2 } else { row * layout.frame_height };
    let result = if needed > length as usize {
        Err(Error::msg(format!("A decoded frame of {length} bytes is smaller than its format says")))
    } else {
        // SAFETY: the check above: the buffer holds all rows. A negative stride means the last row in
        // memory is the top of the picture.
        unsafe {
            if layout.yuv.is_some() {
                convert_nv12(data, row, layout, out);
            } else {
                let first = if layout.stride < 0 { data.add((layout.frame_height - 1) * row) } else { data };
                copy_rgb_rows(first, layout.stride, layout, out);
            }
        }
        Ok(())
    };
    // SAFETY: locked above.
    unsafe { buffer.Unlock() }.ctx("IMFMediaBuffer::Unlock")?;
    result
}

fn ask_for(reader: &IMFSourceReader, subtype: &GUID) -> windows::core::Result<()> {
    let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    // SAFETY: calls on a live reader, with a media type this function owns.
    unsafe {
        let wanted = MFCreateMediaType()?;
        wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        wanted.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        reader.SetCurrentMediaType(video, None, &wanted)
    }
}

/// Feeds the frames of a video to the builder until it has enough or the video ends.
pub(crate) fn decode_frames(path: &Path, gif: &mut GifBuilder) -> Result<()> {
    let _apartment = Apartment::enter();
    let _media = MediaFoundation::start()?;
    let describe = |e: windows::core::Error| hr_error(&format!("Cannot read {}", path.display()), &e);

    let mut attributes: Option<IMFAttributes> = None;
    // SAFETY: the out pointer is a local.
    unsafe { MFCreateAttributes(&mut attributes, 1) }.ctx("MFCreateAttributes")?;
    let attributes = attributes.ok_or_else(|| Error::msg("MFCreateAttributes returned nothing"))?;
    // SAFETY: a setter on an object this function owns.
    unsafe { attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1) }.ctx("video processing")?;
    // SAFETY: the path string lives during the call.
    let reader = unsafe { MFCreateSourceReaderFromURL(&HSTRING::from(path), &attributes) }.map_err(describe)?;

    let all = MF_SOURCE_READER_ALL_STREAMS.0 as u32;
    let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    // SAFETY: calls on a live reader.
    unsafe {
        reader.SetStreamSelection(all, false).map_err(describe)?;
        reader.SetStreamSelection(video, true).map_err(describe)?;
    }
    let nv12 = ask_for(&reader, &MFVideoFormat_NV12).is_ok();
    if !nv12 {
        ask_for(&reader, &MFVideoFormat_RGB32).map_err(describe)?;
    }
    // SAFETY: as above.
    let mut layout = layout_of(&unsafe { reader.GetCurrentMediaType(video) }.map_err(describe)?, nv12)?;
    let mut pixels = Vec::new();
    loop {
        let (mut flags, mut stamp) = (0_u32, 0_i64);
        let mut sample: Option<IMFSample> = None;
        // SAFETY: the out pointers are locals; synchronous mode because no callback was set.
        unsafe { reader.ReadSample(video, 0, None, Some(&mut flags), Some(&mut stamp), Some(&mut sample)) }.map_err(describe)?;
        if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
            // SAFETY: as above.
            layout = layout_of(&unsafe { reader.GetCurrentMediaType(video) }.map_err(describe)?, nv12)?;
        }
        if let Some(sample) = sample {
            match gif.wants(stamp as f64 / TICKS_PER_SEC as f64) {
                Wanted::Frame => {
                    copy_frame(&sample, &layout, &mut pixels)?;
                    gif.take(layout.width as u32, layout.height as u32, &pixels);
                }
                Wanted::Skip => {}
                Wanted::Stop => return Ok(()),
            }
        }
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            return Ok(());
        }
    }
}
