//! A four-second keepalive, not an indefinitely paused hover timer.
use shotik_core::{PointF, Rect};

#[derive(Default, Debug)]
pub struct State {
    pub visible: bool,
    pub bounds: Rect,
    pub deadline: Option<u64>,
}

impl State {
    pub fn show(&mut self, at: PointF, area: Rect, now: u64) {
        let mut x = at.x + 14.0;
        let mut y = at.y + 16.0;
        if x + 60.0 > f64::from(area.x + area.width) {
            x = at.x - 68.0;
        }
        if y + 60.0 > f64::from(area.y + area.height) {
            y = at.y - 68.0;
        }
        if x < f64::from(area.x) {
            x = f64::from(area.x + 4);
        }
        if y < f64::from(area.y) {
            y = f64::from(area.y + 4);
        }
        self.bounds = Rect::new((x + 0.5).floor() as i32, (y + 0.5).floor() as i32, 60, 60);
        self.visible = true;
        self.keepalive(now);
    }

    pub fn keepalive(&mut self, now: u64) {
        self.deadline = Some(now + 4000);
    }
    pub fn hide(&mut self) {
        self.visible = false;
        self.deadline = None;
    }
    pub fn tick(&mut self, now: u64) {
        if self.deadline.is_some_and(|at| now >= at) {
            self.hide();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::test_clock::FakeClock;

    #[test]
    fn keepalive_restarts_but_does_not_pause_the_four_seconds() {
        let c = FakeClock::default();
        let mut s = State::default();
        s.show(PointF::default(), Rect::new(0, 0, 1000, 800), c.now());
        s.keepalive(c.advance(3000));
        s.tick(c.advance(3999));
        assert!(s.visible);
        s.tick(c.advance(1));
        assert!(!s.visible);
        assert_eq!(s.deadline, None);
    }

    #[test]
    fn placement_flips_and_clamps_in_global_dip() {
        let mut s = State::default();
        s.show(PointF { x: -10.0, y: 790.0 }, Rect::new(-1000, 0, 1000, 800), 0);
        assert_eq!(s.bounds, Rect::new(-78, 722, 60, 60));
        s.show(PointF { x: -1000.0, y: 0.0 }, Rect::new(-1000, 0, 1000, 800), 1);
        assert_eq!(s.bounds, Rect::new(-986, 16, 60, 60));
        s.hide();
        s.tick(10000);
        assert!(!s.visible);
    }
}
