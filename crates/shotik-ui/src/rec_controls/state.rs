//! Recording controls: fixed-phase clock, geometry, dBFS meters and supplied diagnostics.
use serde::{Deserialize, Serialize};
use shotik_core::{
    ports::{RecLevels, RecWindows},
    Rect,
};

/// The test contract reports AAC startup, or null for another/unknown encoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioCodec {
    Aac,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    pub audio_codec: Option<AudioCodec>,
    pub indexed: bool,
    pub screen_video_tracks: Option<u32>,
    pub screen_audio_tracks: Option<u32>,
}

#[derive(Debug)]
pub struct State {
    pub bounds: Rect,
    pub inputs: Vec<String>,
    pub levels: RecLevels,
    pub elapsed: u64,
    pub paused: bool,
    next_tick: u64,
}

impl State {
    pub fn new(windows: &RecWindows, now: u64) -> Self {
        let inputs: Vec<String> = ["system", "microphone"]
            .into_iter()
            .filter(|name| windows.meters.iter().any(|input| input == name))
            .map(str::to_owned)
            .collect();
        let bounds = placement(
            windows.display.bounds,
            windows.display.work_area,
            windows.rect_dip,
            inputs.len(),
        );
        Self {
            bounds,
            inputs,
            levels: RecLevels::default(),
            elapsed: 0,
            paused: false,
            next_tick: now + 1000,
        }
    }

    pub fn tick(&mut self, now: u64) {
        if now >= self.next_tick {
            let ticks = (now - self.next_tick) / 1000 + 1;
            if !self.paused {
                self.elapsed += ticks;
            }
            self.next_tick += ticks * 1000;
        }
    }

    pub fn update(&mut self, paused: bool, levels: Option<RecLevels>, now: u64) {
        self.tick(now);
        self.paused = paused;
        if let Some(levels) = levels {
            self.levels = levels;
        }
    }

    pub fn time(&self) -> String {
        format!("{}:{:02}", self.elapsed / 60, self.elapsed % 60)
    }

    pub fn dot_opacity(&self, now: u64) -> f32 {
        if self.paused {
            return 1.0;
        }
        0.25 + 0.75 * (1.0 + ((now % 1200) as f32 / 1200.0 * std::f32::consts::TAU).cos()) / 2.0
    }
}

/// JS sets a rounded percentage; the UI lays that percentage out against the actual track width.
pub fn meter_fraction(peak: f32) -> f32 {
    if peak.is_nan() || peak <= 0.0 {
        return 0.0;
    }
    (((20.0 * peak.log10() + 54.0) / 54.0).clamp(0.0, 1.0) * 100.0).round() / 100.0
}

pub fn placement(display: Rect, work: Rect, region: Option<Rect>, meters: usize) -> Rect {
    let width = 240 + meters as i32 * 44;
    let (mut x, mut y) = match region {
        Some(rect) => {
            let x = f64::from(display.x + rect.x) + f64::from(rect.width - width) / 2.0;
            let below = display.y + rect.y + rect.height + 10;
            let y = if below + 48 > display.y + display.height - 4 {
                display.y + rect.y - 58
            } else {
                below
            };
            (x, y)
        }
        None => (
            f64::from(work.x) + f64::from(work.width - width) / 2.0,
            work.y + work.height - 60,
        ),
    };
    x = x
        .max(f64::from(display.x + 4))
        .min(f64::from(display.x + display.width - width - 4));
    y = (y.max(display.y + 4)).min(display.y + display.height - 48 - 4);
    Rect::new((x + 0.5).floor() as i32, y, width, 48)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::test_clock::FakeClock;
    use shotik_core::Display;

    fn windows() -> RecWindows {
        RecWindows {
            display: Display {
                id: 1,
                bounds: Rect::new(0, 0, 1000, 800),
                work_area: Rect::new(0, 0, 1000, 760),
                physical_bounds: Rect::new(0, 0, 1500, 1200),
                scale_factor: 1.5,
                primary: true,
            },
            rect_dip: Some(Rect::new(20, 20, 320, 180)),
            meters: vec!["microphone".into(), "system".into()],
        }
    }

    #[test]
    fn pause_skips_ticks_without_resetting_the_interval_phase() {
        let c = FakeClock::default();
        let mut s = State::new(&windows(), c.now());
        s.tick(c.advance(999));
        assert_eq!(s.time(), "0:00");
        s.tick(c.advance(1));
        assert_eq!(s.time(), "0:01");
        s.update(true, None, c.advance(500));
        s.tick(c.advance(2000));
        assert_eq!(s.time(), "0:01");
        s.update(false, None, c.now());
        s.tick(c.advance(500));
        assert_eq!(s.time(), "0:02");
        s.tick(c.advance(60_000));
        assert_eq!(s.time(), "1:02");
        assert_eq!(s.inputs, ["system", "microphone"]);
    }

    #[test]
    fn dbfs_is_clamped_rounded_and_clipping_is_separate() {
        assert_eq!(meter_fraction(0.0), 0.0);
        assert_eq!(meter_fraction(f32::NAN), 0.0);
        assert_eq!(meter_fraction(f32::INFINITY), 1.0);
        assert_eq!(meter_fraction(0.001), 0.0);
        assert_eq!(meter_fraction(0.1), 0.63);
        assert_eq!(meter_fraction(0.5), 0.89);
        assert_eq!(meter_fraction(0.98), 1.0);
        assert_eq!(meter_fraction(2.0), 1.0);
    }

    #[test]
    fn controls_flip_above_crop_and_audio_uses_work_area() {
        let display = Rect::new(2560, 462, 1920, 1080);
        let work = Rect::new(2560, 462, 1920, 1040);
        assert_eq!(
            placement(display, work, Some(Rect::new(20, 1000, 320, 70)), 2),
            Rect::new(2576, 1404, 328, 48)
        );
        assert_eq!(placement(display, work, None, 1), Rect::new(3378, 1442, 284, 48));
        assert_eq!(
            placement(display, work, Some(Rect::new(-100, -100, 10, 10)), 0),
            Rect::new(2564, 466, 240, 48)
        );
    }

    #[test]
    fn diagnostics_remain_unknown_until_supplied() {
        assert_eq!(
            serde_json::to_value(Diagnostics::default()).unwrap(),
            serde_json::json!({
            "audioCodec":null,"indexed":false,"screenVideoTracks":null,"screenAudioTracks":null })
        );
    }

    #[test]
    fn diagnostics_codec_obeys_the_route_schema() {
        let data = Diagnostics {
            audio_codec: Some(AudioCodec::Aac),
            ..Default::default()
        };
        assert_eq!(serde_json::to_value(data).unwrap()["audioCodec"], "aac");
        assert!(serde_json::from_value::<AudioCodec>(serde_json::json!("invented-codec")).is_err());
    }

    #[test]
    fn odd_region_and_work_area_centers_round_as_in_recorder_js() {
        let display = Rect::new(-1000, 0, 1000, 800);
        assert_eq!(
            placement(display, display, Some(Rect::new(100, 20, 321, 180)), 2),
            Rect::new(-903, 210, 328, 48)
        );
        assert_eq!(
            placement(display, Rect::new(-1000, 0, 999, 760), None, 1),
            Rect::new(-642, 700, 284, 48)
        );
    }
}
