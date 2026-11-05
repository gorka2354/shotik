//! OCR language selection, small-text upscaling and word picking (`src/main/ocr.js`).

use crate::ports::Ocr;
use crate::text::{is_js_whitespace, trim};
use crate::{Frame, Result};

pub fn cyr(text: &str) -> usize {
    text.chars()
        .filter(|c| ('\u{0400}'..='\u{04ff}').contains(c))
        .count()
}
pub fn lat(text: &str) -> usize {
    text.bytes().filter(u8::is_ascii_alphabetic).count()
}
pub fn digit_mix(text: &str) -> usize {
    text.split(is_js_whitespace)
        .filter(|token| {
            token.bytes().any(|c| c.is_ascii_alphabetic())
                && token.bytes().any(|c| c.is_ascii_digit())
        })
        .count()
}

fn upscale(png: &[u8]) -> (Vec<u8>, f64) {
    // JS keeps the original image if decoding or resizing fails.
    let scaled = Frame::from_png(png).and_then(|frame| {
        if frame.width() > 0 && frame.width() < 1400 {
            frame
                .resize(frame.width() * 2, frame.height() * 2)?
                .to_png()
                .map(|png| (png, 2.0))
        } else {
            Ok((png.to_vec(), 1.0))
        }
    });
    scaled.unwrap_or_else(|_| (png.to_vec(), 1.0))
}

pub fn recognize_for_selection(ocr: &dyn Ocr, png: &[u8], platform: &str) -> Result<String> {
    if platform != "win32" {
        return Ok(trim(&ocr.recognize(png, None).unwrap_or_default()).into());
    }
    let (png, _) = upscale(png);
    // Failed probes count as empty text in the reference's selection policy. A successful
    // second language can still recover the text; both empty probes give an empty selection.
    let ru = trim(&ocr.recognize(&png, Some("ru")).unwrap_or_default()).to_owned();
    let en = trim(&ocr.recognize(&png, Some("en-US")).unwrap_or_default()).to_owned();
    let rc = cyr(&ru);
    if rc > 0 && rc >= lat(&ru) {
        if !en.is_empty() && lat(&en) >= rc && digit_mix(&en) == 0 {
            return Ok(en);
        }
        return Ok(ru);
    }
    Ok(if en.is_empty() { ru } else { en })
}

pub fn recognize_word_at(
    ocr: &dyn Ocr,
    png: &[u8],
    rel_x: f64,
    rel_y: f64,
    platform: &str,
) -> Result<String> {
    if platform != "win32" {
        return Ok(trim(&ocr.recognize(png, None).unwrap_or_default()).into());
    }
    let (png, scale) = upscale(png);
    let probe = trim(&ocr.recognize(&png, Some("ru")).unwrap_or_default()).to_owned();
    let lang = if cyr(&probe) > 0 && cyr(&probe) >= lat(&probe) {
        "ru"
    } else {
        "en-US"
    };
    let boxes = ocr.recognize_boxes(&png, Some(lang)).unwrap_or_default();
    let (cx, cy) = (rel_x * scale, rel_y * scale);
    let (mut best, mut best_distance) = (None, f64::INFINITY);
    for line in &boxes.lines {
        for word in &line.words {
            if cx >= word.x && cx <= word.x + word.w && cy >= word.y && cy <= word.y + word.h {
                return Ok(trim(&word.text).into());
            }
            let distance = (word.x + word.w / 2.0 - cx).abs() + (word.y + word.h / 2.0 - cy).abs();
            if distance < best_distance {
                best_distance = distance;
                best = Some(word.text.as_str());
            }
        }
    }
    Ok(trim(best.filter(|s| !s.is_empty()).unwrap_or(&probe)).into())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;
    use crate::ports::{OcrBoxes, OcrLine, OcrWord};
    use crate::Error;

    #[derive(Debug, PartialEq)]
    struct Call {
        boxes: bool,
        lang: Option<String>,
        size: Option<(u32, u32)>,
    }

    struct FakeOcr {
        texts: Mutex<VecDeque<Result<String>>>,
        boxes: Mutex<Option<Result<OcrBoxes>>>,
        calls: Mutex<Vec<Call>>,
    }

    impl FakeOcr {
        fn new(texts: &[&str], boxes: OcrBoxes) -> Self {
            Self {
                texts: Mutex::new(texts.iter().map(|s| Ok(s.to_string())).collect()),
                boxes: Mutex::new(Some(Ok(boxes))),
                calls: Mutex::new(Vec::new()),
            }
        }
        fn record(&self, png: &[u8], lang: Option<&str>, boxes: bool) {
            self.calls.lock().unwrap().push(Call {
                boxes,
                lang: lang.map(str::to_owned),
                size: Frame::from_png(png).ok().map(|f| (f.width(), f.height())),
            });
        }
    }

    impl Ocr for FakeOcr {
        fn recognize(&self, png: &[u8], lang: Option<&str>) -> Result<String> {
            self.record(png, lang, false);
            self.texts
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected recognize call")
        }
        fn recognize_boxes(&self, png: &[u8], lang: Option<&str>) -> Result<OcrBoxes> {
            self.record(png, lang, true);
            self.boxes
                .lock()
                .unwrap()
                .take()
                .expect("unexpected recognize_boxes call")
        }
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        Frame::new(
            width,
            height,
            vec![255; width as usize * height as usize * 4],
        )
        .unwrap()
        .to_png()
        .unwrap()
    }

    fn words(entries: &[(&str, f64, f64, f64, f64)]) -> OcrBoxes {
        OcrBoxes {
            lines: vec![OcrLine {
                text: String::new(),
                words: entries
                    .iter()
                    .map(|(text, x, y, w, h)| OcrWord {
                        text: text.to_string(),
                        x: *x,
                        y: *y,
                        w: *w,
                        h: *h,
                    })
                    .collect(),
            }],
        }
    }

    #[test]
    fn script_counters_and_mixed_digit_tokens_match_js_ranges() {
        assert_eq!(cyr("ЀЁАяӿԀ a"), 5);
        assert_eq!(lat("AZaz ÉЯ０"), 4);
        assert_eq!(digit_mix("a06poe 3anpoc 123 abc А1 a1b2\nA9"), 4);
        assert_eq!(digit_mix("a\u{feff}1"), 0);
        assert_eq!(digit_mix("a\u{0085}1"), 1);
    }

    #[test]
    fn language_choice_handles_cyrillic_lookalikes_uppercase_and_digit_garbage() {
        let png = png(200, 20);
        for (ru, en, expected) in [
            (" рип ", " pull ", "pull"),
            ("ТАТЮ", "STATION", "STATION"),
            ("Доброе утро", "a06poe yTpo", "Доброе утро"),
            ("запрос", "3anpoc", "запрос"),
            ("запрос", "ask", "запрос"),
            ("abc Я", "clean", "clean"),
            ("abc", "", "abc"),
            ("", " text ", "text"),
            ("", "", ""),
        ] {
            let fake = FakeOcr::new(&[ru, en], OcrBoxes::default());
            assert_eq!(
                recognize_for_selection(&fake, &png, "win32").unwrap(),
                expected
            );
            let calls = fake.calls.lock().unwrap();
            assert_eq!(
                calls.iter().map(|c| c.lang.as_deref()).collect::<Vec<_>>(),
                [Some("ru"), Some("en-US")]
            );
            assert!(calls.iter().all(|c| c.size == Some((400, 40)) && !c.boxes));
        }
    }

    #[test]
    fn ghost_case_phrases_are_selected_with_a_fake_engine() {
        let cases: serde_json::Value =
            serde_json::from_str(include_str!("../../../test/ocr-cases.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let text = case["t"].as_str().unwrap();
            let texts = if case["n"].as_str().unwrap().starts_with("ru_") {
                [text, "a06poe 3anpoc"]
            } else {
                ["рип", text]
            };
            let fake = FakeOcr::new(&texts, OcrBoxes::default());
            assert_eq!(
                recognize_for_selection(&fake, &png(300, 25), "win32").unwrap(),
                text
            );
        }
    }

    #[test]
    fn upscale_boundary_is_strictly_below_1400_and_bad_png_keeps_original() {
        for (width, expected) in [(1399, 2798), (1400, 1400), (1401, 1401)] {
            let fake = FakeOcr::new(&["", "ok"], OcrBoxes::default());
            recognize_for_selection(&fake, &png(width, 2), "win32").unwrap();
            assert_eq!(
                fake.calls.lock().unwrap()[0].size,
                Some((expected, if width < 1400 { 4 } else { 2 }))
            );
        }
        let fake = FakeOcr::new(&["", "ok"], OcrBoxes::default());
        assert_eq!(
            recognize_for_selection(&fake, b"bad PNG", "win32").unwrap(),
            "ok"
        );
        assert!(fake.calls.lock().unwrap().iter().all(|c| c.size.is_none()));
    }

    #[test]
    fn non_windows_uses_the_profile_once_without_upscaling() {
        for word in [false, true] {
            let fake = FakeOcr::new(&[" text \n"], OcrBoxes::default());
            let png = png(200, 20);
            let text = if word {
                recognize_word_at(&fake, &png, 0.0, 0.0, "darwin")
            } else {
                recognize_for_selection(&fake, &png, "darwin")
            };
            assert_eq!(text.unwrap(), "text");
            assert_eq!(
                *fake.calls.lock().unwrap(),
                [Call {
                    boxes: false,
                    lang: None,
                    size: Some((200, 20))
                }]
            );
        }
    }

    #[test]
    fn word_at_scales_coordinates_and_uses_inclusive_bounds() {
        let boxes = words(&[
            (" Good ", 10.0, 10.0, 20.0, 10.0),
            ("morning", 40.0, 10.0, 60.0, 10.0),
        ]);
        let fake = FakeOcr::new(&["Good morning"], boxes);
        assert_eq!(
            recognize_word_at(&fake, &png(300, 20), 15.0, 10.0, "win32").unwrap(),
            "Good"
        );
        assert_eq!(
            *fake.calls.lock().unwrap(),
            [
                Call {
                    boxes: false,
                    lang: Some("ru".into()),
                    size: Some((600, 40))
                },
                Call {
                    boxes: true,
                    lang: Some("en-US".into()),
                    size: Some((600, 40))
                },
            ]
        );
    }

    #[test]
    fn word_at_picks_nearest_center_by_manhattan_distance_and_first_tie() {
        // No containing box. (4,4) is closer Euclidean, (0,7) is closer Manhattan.
        let fake = FakeOcr::new(
            &["слова"],
            words(&[
                ("first", 4.0, 4.0, 0.0, 0.0),
                ("nearest", 0.0, 7.0, 0.0, 0.0),
                ("tie", 7.0, 0.0, 0.0, 0.0),
            ]),
        );
        assert_eq!(
            recognize_word_at(&fake, &png(1400, 20), 0.0, 0.0, "win32").unwrap(),
            "nearest"
        );
        assert_eq!(fake.calls.lock().unwrap()[1].lang.as_deref(), Some("ru"));
    }

    #[test]
    fn word_at_without_words_returns_probe_and_probe_failure_can_use_english_boxes() {
        let fake = FakeOcr::new(&[" probe "], OcrBoxes::default());
        assert_eq!(
            recognize_word_at(&fake, &png(200, 20), 10.0, 10.0, "win32").unwrap(),
            "probe"
        );
        let fake = FakeOcr::new(&[], words(&[("word", 0.0, 0.0, 20.0, 20.0)]));
        fake.texts
            .lock()
            .unwrap()
            .push_back(Err(Error::msg("OCR unavailable")));
        assert_eq!(
            recognize_word_at(&fake, &png(200, 20), 1.0, 1.0, "win32").unwrap(),
            "word"
        );
        assert_eq!(fake.calls.lock().unwrap()[1].lang.as_deref(), Some("en-US"));
    }

    #[test]
    fn engine_failures_follow_reference_empty_probe_policy() {
        let fake = FakeOcr::new(&[], OcrBoxes::default());
        *fake.texts.lock().unwrap() =
            VecDeque::from([Err(Error::msg("ru unavailable")), Ok("English".into())]);
        assert_eq!(
            recognize_for_selection(&fake, &png(200, 20), "win32").unwrap(),
            "English"
        );
        let fake = FakeOcr::new(&[], OcrBoxes::default());
        *fake.texts.lock().unwrap() = VecDeque::from([
            Err(Error::msg("ru unavailable")),
            Err(Error::msg("en unavailable")),
        ]);
        assert_eq!(
            recognize_for_selection(&fake, &png(200, 20), "win32").unwrap(),
            ""
        );
        let fake = FakeOcr::new(&["probe"], OcrBoxes::default());
        *fake.boxes.lock().unwrap() = Some(Err(Error::msg("boxes unavailable")));
        assert_eq!(
            recognize_word_at(&fake, &png(200, 20), 0.0, 0.0, "win32").unwrap(),
            "probe"
        );
    }
}
