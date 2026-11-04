//! The ring is strictly outside the recorded crop.
use shotik_core::Rect;

#[derive(Debug)]
pub struct State {
    pub bounds: Rect,
    pub thickness: i32,
}
impl State {
    pub fn new(display: Rect, region: Rect) -> Self {
        Self {
            bounds: Rect::new(
                display.x + region.x - 3,
                display.y + region.y - 3,
                region.width + 6,
                region.height + 6,
            ),
            thickness: 3,
        }
    }
    /// Reference pulse repeats every 1.4 seconds, halfway opacity is 0.45.
    pub fn opacity(now: u64) -> f32 {
        let t = (now % 1400) as f32 / 1400.0;
        0.45 + 0.55 * (1.0 + (t * std::f32::consts::TAU).cos()) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::test_clock::FakeClock;
    #[test]
    fn inner_edge_equals_crop_and_pulse_repeats() {
        let s = State::new(Rect::new(2560, 462, 1920, 1080), Rect::new(20, 20, 320, 180));
        assert_eq!(s.bounds, Rect::new(2577, 479, 326, 186));
        assert_eq!(s.bounds.x + s.thickness, 2580);
        let c = FakeClock::default();
        assert_eq!(State::opacity(c.now()), 1.0);
        assert!((State::opacity(c.advance(700)) - 0.45).abs() < 0.0001);
        assert_eq!(State::opacity(c.advance(700)), 1.0);
    }
}
