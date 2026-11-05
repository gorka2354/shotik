//! In-memory WinRT OCR, equivalent to `src/main/ocr.ps1` (no child process or temp file).
//! Run these synchronous methods on a blocking thread, as required by the port.

use shotik_core::error::{Error, Result};
use shotik_core::ports::{Ocr, OcrBoxes, OcrLine, OcrWord};
use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::BitmapDecoder;
use windows::Media::Ocr::{OcrEngine, OcrResult};
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
use windows::Win32::Foundation::{E_FAIL, RPC_E_CHANGED_MODE};
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};

#[derive(Debug, Default)]
pub struct WindowsOcr;

// RoInitialize is per-thread. Balance S_OK and S_FALSE, but do not uninitialize an
// apartment owned by the caller when it was already initialized in another mode.
struct Apartment(bool);

impl Apartment {
    fn enter() -> windows::core::Result<Self> {
        match unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
            Ok(()) => Ok(Self(true)),
            Err(e) if e.code() == RPC_E_CHANGED_MODE => Ok(Self(false)),
            Err(e) => Err(e),
        }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { RoUninitialize() };
        }
    }
}

fn ocr_error(e: windows::core::Error) -> Error {
    Error::msg(format!("Windows OCR: {e}"))
}

fn engine(lang: Option<&str>) -> windows::core::Result<OcrEngine> {
    // windows-core 0.62 projects a successful null interface as Error::empty()
    // (code 0). Only that means "language not installed"; invalid tags and other
    // failures must remain errors, just as in the PowerShell script.
    let selected = match lang.filter(|tag| !tag.is_empty()) {
        Some(tag) => {
            OcrEngine::TryCreateFromLanguage(&Language::CreateLanguage(&HSTRING::from(tag))?)
        }
        None => OcrEngine::TryCreateFromUserProfileLanguages(),
    };
    match selected {
        Err(e) if e.code().0 == 0 => {
            OcrEngine::TryCreateFromLanguage(&Language::CreateLanguage(&HSTRING::from("en-US"))?)
                .map_err(|e| {
                    if e.code().0 == 0 {
                        windows::core::Error::new(E_FAIL, "No OCR language available")
                    } else {
                        e
                    }
                })
        }
        result => result,
    }
}

fn recognize(png: &[u8], lang: Option<&str>) -> windows::core::Result<OcrResult> {
    let writer = DataWriter::new()?;
    writer.WriteBytes(png)?;
    let buffer = writer.DetachBuffer()?;
    let stream = InMemoryRandomAccessStream::new()?;
    stream.WriteAsync(&buffer)?.join()?;
    stream.Seek(0)?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?.join()?;
    let bitmap = decoder.GetSoftwareBitmapAsync()?.join()?;
    let result = engine(lang)?.RecognizeAsync(&bitmap)?.join();
    // Close also on an OCR error; retain the actual recognition failure first.
    let closed = bitmap.Close().and_then(|()| stream.Close());
    let result = result?;
    closed?;
    Ok(result)
}

impl Ocr for WindowsOcr {
    fn recognize(&self, png: &[u8], lang: Option<&str>) -> Result<String> {
        let _apartment = Apartment::enter().map_err(ocr_error)?;
        let result = recognize(png, lang).map_err(ocr_error)?;
        let mut lines = Vec::new();
        for line in result.Lines().map_err(ocr_error)? {
            lines.push(line.Text().map_err(ocr_error)?.to_string());
        }
        // ocr.js normalizes the script's output to LF and trims it.
        Ok(lines.join("\n").trim().to_owned())
    }

    fn recognize_boxes(&self, png: &[u8], lang: Option<&str>) -> Result<OcrBoxes> {
        let _apartment = Apartment::enter().map_err(ocr_error)?;
        let result = recognize(png, lang).map_err(ocr_error)?;
        let mut lines = Vec::new();
        for line in result.Lines().map_err(ocr_error)? {
            let mut words = Vec::new();
            for word in line.Words().map_err(ocr_error)? {
                let rect = word.BoundingRect().map_err(ocr_error)?;
                words.push(OcrWord {
                    text: word.Text().map_err(ocr_error)?.to_string(),
                    // .NET Math.Round uses ties-to-even, unlike f64::round.
                    x: f64::from(rect.X).round_ties_even(),
                    y: f64::from(rect.Y).round_ties_even(),
                    w: f64::from(rect.Width).round_ties_even(),
                    h: f64::from(rect.Height).round_ties_even(),
                });
            }
            lines.push(OcrLine {
                text: line.Text().map_err(ocr_error)?.to_string(),
                words,
            });
        }
        Ok(OcrBoxes { lines })
    }
}
