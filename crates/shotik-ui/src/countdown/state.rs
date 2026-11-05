//! The renderer steps at 700 ms; recorder.js destroys the window at 2050 ms.
use shotik_core::Rect;

#[derive(Debug)]
pub struct State {
    pub started: u64,
    pub finished: bool,
}
impl State {
    pub fn new(now: u64) -> Self {
        Self {
            started: now,
            finished: false,
        }
    }
    pub fn number(&self, now: u64) -> u8 {
        3 - ((now.saturating_sub(self.started) / 700).min(2) as u8)
    }
    pub fn next_deadline(&self, now: u64) -> Option<u64> {
        if self.finished {
            return None;
        }
        [700, 1400, 2050]
            .into_iter()
            .map(|offset| self.started + offset)
            .find(|at| *at > now)
    }
    /// True exactly once, so the recording start callback cannot be delivered twice.
    pub fn tick(&mut self, now: u64) -> bool {
        if !self.finished && now.saturating_sub(self.started) >= 2050 {
            self.finished = true;
            true
        } else {
            false
        }
    }
    pub fn placement(display: Rect, rect: Rect) -> Rect {
        Rect::new(
            (f64::from(display.x + rect.x) + f64::from(rect.width) / 2.0 - 90.0 + 0.5).floor() as i32,
            (f64::from(display.y + rect.y) + f64::from(rect.height) / 2.0 - 90.0 + 0.5).floor() as i32,
            180,
            180,
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::test_clock::FakeClock;
    #[test]
    fn exact_reference_schedule_and_once_only_completion() {
        let c = FakeClock::default();
        let mut s = State::new(c.now());
        assert_eq!(s.number(c.now()), 3);
        assert_eq!(s.number(c.advance(699)), 3);
        assert_eq!(s.number(c.advance(1)), 2);
        assert_eq!(s.number(c.advance(700)), 1);
        assert!(!s.tick(c.advance(649)));
        assert!(s.tick(c.advance(1)));
        assert!(!s.tick(c.advance(1000)));
        assert_eq!(
            State::placement(Rect::new(-2560, 279, 2560, 1440), Rect::new(20, 20, 320, 180)),
            Rect::new(-2470, 299, 180, 180)
        );
    }

    #[test]
    fn odd_regions_use_js_half_rounding_even_at_negative_origins() {
        assert_eq!(
            State::placement(Rect::new(-2560, 279, 2560, 1440), Rect::new(20, 20, 321, 181)),
            Rect::new(-2469, 300, 180, 180)
        );
    }

    #[test]
    fn next_wake_is_a_number_change_or_the_close_deadline() {
        let c = FakeClock::default();
        let mut state = State::new(c.now());
        assert_eq!(state.next_deadline(c.now()), Some(700));
        assert_eq!(state.next_deadline(c.advance(700)), Some(1400));
        assert_eq!(state.next_deadline(c.advance(700)), Some(2050));
        assert!(state.tick(c.advance(650)));
        assert_eq!(state.next_deadline(c.now()), None);
    }
}
