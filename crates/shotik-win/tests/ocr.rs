#![cfg(windows)]
//! Native OCR acceptance tests. All pixels are generated in a memory DC or come
//! from the checked-in fake screen; these tests never capture a real display.
use std::time::Instant;

use shotik_core::frame::Frame;
use shotik_core::ports::Ocr;
use shotik_win::ocr::WindowsOcr;
use windows::core::w;
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Graphics::Gdi::*;

struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    font: HFONT,
    previous_bitmap: HGDIOBJ,
    previous_font: HGDIOBJ,
    bits: *mut core::ffi::c_void,
    width: u32,
    height: u32,
}

impl Canvas {
    fn new(width: u32, height: u32, font_height: i32) -> Self {
        let dc = unsafe { CreateCompatibleDC(None) };
        assert!(!dc.is_invalid());
        let mut canvas = Self {
            dc,
            bitmap: HBITMAP::default(),
            font: HFONT::default(),
            previous_bitmap: HGDIOBJ::default(),
            previous_font: HGDIOBJ::default(),
            bits: std::ptr::null_mut(),
            width,
            height,
        };
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..BITMAPINFOHEADER::default()
            },
            ..BITMAPINFO::default()
        };
        canvas.bitmap =
            unsafe { CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut canvas.bits, None, 0) }
                .unwrap();
        canvas.previous_bitmap = unsafe { SelectObject(dc, HGDIOBJ(canvas.bitmap.0)) };
        assert!(!canvas.previous_bitmap.is_invalid());
        unsafe {
            std::ptr::write_bytes(canvas.bits, 255, (width * height * 4) as usize);
        }
        canvas.font = unsafe {
            CreateFontW(
                -font_height,
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                u32::from(DEFAULT_PITCH.0 | FF_DONTCARE.0),
                w!("Segoe UI"),
            )
        };
        assert!(!canvas.font.is_invalid());
        canvas.previous_font = unsafe { SelectObject(dc, HGDIOBJ(canvas.font.0)) };
        assert!(!canvas.previous_font.is_invalid());
        unsafe {
            SetBkColor(dc, COLORREF(0xffffff));
            SetTextColor(dc, COLORREF(0));
        }
        canvas
    }

    fn text(&self, x: i32, y: i32, text: &str) {
        let text: Vec<_> = text.encode_utf16().collect();
        assert!(unsafe { TextOutW(self.dc, x, y, &text) }.as_bool());
    }

    fn png(&self) -> Vec<u8> {
        assert!(unsafe { GdiFlush() }.as_bool());
        let mut bgra = unsafe {
            std::slice::from_raw_parts(
                self.bits.cast::<u8>(),
                (self.width * self.height * 4) as usize,
            )
        }
        .to_vec();
        for pixel in bgra.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        Frame::new(self.width, self.height, bgra)
            .unwrap()
            .to_png()
            .unwrap()
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            if !self.previous_font.is_invalid() {
                SelectObject(self.dc, self.previous_font);
            }
            if !self.previous_bitmap.is_invalid() {
                SelectObject(self.dc, self.previous_bitmap);
            }
            if !self.font.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.font.0));
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            }
            let _ = DeleteDC(self.dc);
        }
    }
}

fn fixture(text: &str) -> Vec<u8> {
    let canvas = Canvas::new(1200, 100, 32);
    canvas.text(40, 20, text);
    canvas.png()
}

fn normalized(text: &str) -> String {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn russian_and_english_generated_fixtures_have_text_and_pixel_boxes() {
    let ocr = WindowsOcr;
    for (language, text) in [
        ("en-US", "Good morning, please review the pull request"),
        ("ru", "Доброе утро, посмотрите пожалуйста запрос"),
    ] {
        // A build server's image has no Russian OCR language.
        if language == "ru" && std::env::var_os("CI").is_some() {
            continue;
        }
        let png = fixture(text);
        assert_eq!(
            normalized(&ocr.recognize(&png, Some(language)).unwrap()),
            normalized(text),
            "{language}"
        );
        let boxes = ocr.recognize_boxes(&png, Some(language)).unwrap();
        assert_eq!(
            normalized(
                &boxes
                    .lines
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            normalized(text)
        );
        let json = serde_json::to_value(&boxes).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 1);
        assert!(json["lines"].is_array());
        let words: Vec<_> = boxes.lines.iter().flat_map(|line| &line.words).collect();
        assert!(words.len() >= 5);
        for word in &words {
            assert!(word.x >= 0.0 && word.y >= 0.0 && word.w > 0.0 && word.h > 0.0);
            assert!(word.x + word.w <= 1200.0 && word.y + word.h <= 100.0);
            for coord in [word.x, word.y, word.w, word.h] {
                assert_eq!(coord.fract(), 0.0);
            }
        }
        // These are physical image pixels, unaffected by the desktop DPI.
        assert!((35.0..65.0).contains(&words[0].x));
        assert!((15.0..50.0).contains(&words[0].y));
        let first = &json["lines"][0];
        assert_eq!(first.as_object().unwrap().len(), 2);
        let word = first["words"][0].as_object().unwrap();
        assert_eq!(word.len(), 5);
        for key in ["text", "x", "y", "w", "h"] {
            assert!(word.contains_key(key));
        }
    }
}

#[test]
fn missing_language_falls_back_to_english_and_profile_language_is_accepted() {
    let png = fixture("Good morning please review the request");
    let ocr = WindowsOcr;
    let english = ocr.recognize(&png, Some("en-US")).unwrap();
    // Esperanto is a valid BCP-47 tag with no Windows OCR engine.
    assert_eq!(ocr.recognize(&png, Some("eo")).unwrap(), english);
    assert!(!ocr.recognize(&png, None).unwrap().is_empty());
    assert_eq!(
        ocr.recognize(&png, Some("")).unwrap(),
        ocr.recognize(&png, None).unwrap()
    );
    assert!(ocr
        .recognize(&png, Some("this is not a language tag"))
        .is_err());
}

#[test]
fn boxes_keep_multiple_lines_and_image_pixel_coordinates() {
    let canvas = Canvas::new(1200, 180, 32);
    canvas.text(40, 20, "Good morning");
    canvas.text(40, 100, "Please review the request");
    let png = canvas.png();
    let ocr = WindowsOcr;
    let boxes = ocr.recognize_boxes(&png, Some("en-US")).unwrap();
    assert_eq!(boxes.lines.len(), 2);
    assert_eq!(normalized(&boxes.lines[0].text), "good morning");
    assert_eq!(
        normalized(&boxes.lines[1].text),
        "please review the request"
    );
    assert_eq!(boxes.lines[0].words.len(), 2);
    assert_eq!(boxes.lines[1].words.len(), 4);
    assert!(boxes.lines[0].words[0].y < 60.0);
    assert!(boxes.lines[1].words[0].y >= 100.0);
    let text = ocr.recognize(&png, Some("en-US")).unwrap();
    assert_eq!(text.lines().count(), 2);
}

#[test]
fn fake_screen_and_bad_input_do_not_require_screen_access() {
    let ocr = WindowsOcr;
    let boxes = ocr
        .recognize_boxes(include_bytes!("../../../test/fake-screen.png"), None)
        .unwrap();
    assert!(!boxes.lines.is_empty());
    assert!(ocr.recognize(b"not a PNG", Some("en-US")).is_err());
    let empty = Canvas::new(240, 80, 24).png();
    assert!(ocr.recognize(&empty, Some("en-US")).unwrap().is_empty());
    assert!(ocr
        .recognize_boxes(&empty, Some("en-US"))
        .unwrap()
        .lines
        .is_empty());
}

#[test]
fn native_ocr_median_latency() {
    let ocr = WindowsOcr;
    let en = fixture("Good morning, please review the pull request");
    let ru = fixture("Доброе утро, посмотрите пожалуйста запрос");
    for (label, png, language) in [("EN", &en, "en-US"), ("RU", &ru, "ru")] {
        let cold = Instant::now();
        assert!(!ocr.recognize(png, Some(language)).unwrap().is_empty());
        let first_ms = cold.elapsed().as_secs_f64() * 1000.0;
        let mut samples = Vec::new();
        for _ in 0..9 {
            let started = Instant::now();
            assert!(!ocr.recognize(png, Some(language)).unwrap().is_empty());
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        samples.sort_by(f64::total_cmp);
        eprintln!("native OCR {label}: first={first_ms:.2} ms; median(n=9)={:.2} ms; fixture=1200x100 PNG; raw single-engine port", samples[4]);
    }
}
