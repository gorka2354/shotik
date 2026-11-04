//! Pin geometry and input rules, independent of Slint and Win32.
use shotik_core::Rect;

#[derive(Debug)]
pub struct State {
    pub bounds: Rect,
    pub base: (i32, i32),
    pub zoom: f64,
    pub opacity: u8,
    pub ready: bool,
}

// Math.round in JS rounds half towards positive infinity, including negative coordinates.
fn round(value: f64) -> i32 {
    (value + 0.5).floor() as i32
}

impl State {
    pub fn new(bounds: Rect) -> Self {
        Self {
            bounds: Rect::new(bounds.x, bounds.y, bounds.width.max(40), bounds.height.max(40)),
            base: (bounds.width, bounds.height),
            zoom: 1.0,
            opacity: 100,
            ready: false,
        }
    }

    pub fn zoom(&mut self, direction: i32) {
        let next = (self.zoom * if direction > 0 { 1.1 } else { 1.0 / 1.1 }).clamp(0.2, 5.0);
        if (next - self.zoom).abs() < 0.001 {
            return;
        }
        let width = round(f64::from(self.base.0) * next).max(40);
        let height = round(f64::from(self.base.1) * next).max(40);
        self.bounds = Rect::new(
            round(f64::from(self.bounds.x) + f64::from(self.bounds.width - width) / 2.0),
            round(f64::from(self.bounds.y) + f64::from(self.bounds.height - height) / 2.0),
            width,
            height,
        );
        self.zoom = next;
    }

    pub fn reset(&mut self) {
        self.bounds.width = self.base.0;
        self.bounds.height = self.base.1;
        self.zoom = 1.0;
    }

    pub fn set_opacity(&mut self, percent: u8) {
        if [100, 85, 70, 50].contains(&percent) {
            self.opacity = percent;
        }
    }

    pub fn move_to(&mut self, x: i32, y: i32) {
        self.bounds.x = x;
        self.bounds.y = y;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_is_centered_clamped_and_keeps_minimum_size() {
        let mut s = State::new(Rect::new(-101, 10, 101, 80));
        s.zoom(1);
        assert_eq!(s.bounds, Rect::new(-106, 6, 111, 88));
        for _ in 0..100 {
            s.zoom(1);
        }
        assert_eq!(s.zoom, 5.0);
        assert_eq!(s.bounds.width, 505);
        for _ in 0..100 {
            s.zoom(-1);
        }
        assert_eq!(s.zoom, 0.2);
        assert_eq!((s.bounds.width, s.bounds.height), (40, 40));
    }

    #[test]
    fn reset_keeps_the_current_top_left_not_the_original_center() {
        let mut s = State::new(Rect::new(20, 30, 300, 100));
        s.zoom(1);
        s.move_to(-50, 300);
        s.reset();
        assert_eq!(s.bounds, Rect::new(-50, 300, 300, 100));
        assert_eq!(s.zoom, 1.0);
    }

    #[test]
    fn menu_opacity_has_exactly_the_four_reference_values() {
        let mut s = State::new(Rect::default());
        for op in [100, 85, 70, 50] {
            s.set_opacity(op);
            assert_eq!(s.opacity, op);
        }
        s.set_opacity(0);
        assert_eq!(s.opacity, 50);
        assert!(!s.ready);
    }
}
