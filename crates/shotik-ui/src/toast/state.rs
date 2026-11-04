//! `windows.js`: deadlines are milliseconds supplied by the caller, never toolkit timers.
use shotik_core::{ports::Toast, Rect};

#[derive(Default, Debug)]
pub struct State {
    pub visible: bool,
    pub hovering: bool,
    pub deadline: Option<u64>,
    pub last: Option<Toast>,
    ttl: u64,
}

impl State {
    pub fn show(&mut self, content: Toast, now: u64) {
        self.ttl = match content.kind.as_str() {
            "video" | "gif" => 10_000,
            _ => 4_200,
        };
        self.last = Some(content);
        self.visible = true;
        // A parked cursor does not send another mouse-enter on a re-trigger.
        self.deadline = (!self.hovering).then_some(now + self.ttl);
    }

    pub fn hover(&mut self, over: bool, now: u64) {
        if self.visible {
            self.hovering = over;
            self.deadline = (!over).then_some(now + self.ttl.min(2_500));
        }
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.hovering = false; // No mouse-leave when hidden under the cursor.
        self.deadline = None;
    }

    pub fn tick(&mut self, now: u64) {
        if self.deadline.is_some_and(|at| now >= at) {
            self.hide();
        }
    }
}

pub fn placement(area: Rect) -> Rect {
    Rect::new(area.x + area.width - 380 - 16, area.y + area.height - 92 - 16, 380, 92)
}

/// Shared by native input and test routes; malformed/unknown actions are errors.
pub fn hover_action(action: &serde_json::Value) -> shotik_core::Result<bool> {
    if action.get("type").and_then(serde_json::Value::as_str) != Some("hover") {
        return Err(shotik_core::Error::msg("unknown small-window action"));
    }
    action
        .get("over")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| shotik_core::Error::msg("hover needs boolean over"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::test_clock::FakeClock;

    fn toast(kind: &str) -> Toast {
        Toast {
            kind: kind.into(),
            title: String::new(),
            body: String::new(),
            thumb: None,
            file: None,
            color: None,
        }
    }

    #[test]
    fn every_kind_has_the_reference_ttl() {
        for (kind, ttl) in [
            ("image", 4200),
            ("claude", 4200),
            ("text", 4200),
            ("repeat", 4200),
            ("audio", 4200),
            ("color", 4200),
            ("video", 10000),
            ("gif", 10000),
        ] {
            let clock = FakeClock::default();
            let mut state = State::default();
            state.show(toast(kind), clock.now());
            state.tick(clock.advance(ttl - 1));
            assert!(state.visible, "{kind}");
            state.tick(clock.advance(1));
            assert!(!state.visible, "{kind}");
        }
    }

    #[test]
    fn retrigger_under_parked_cursor_stays_paused_and_leave_restarts_short_ttl() {
        let clock = FakeClock::default();
        let mut state = State::default();
        state.show(toast("text"), clock.now());
        state.hover(true, clock.advance(100));
        state.show(toast("video"), clock.advance(100));
        state.tick(clock.advance(20_000));
        assert!(state.visible);
        assert_eq!(state.deadline, None);
        state.hover(false, clock.now());
        state.tick(clock.advance(2499));
        assert!(state.visible);
        state.tick(clock.advance(1));
        assert!(!state.visible);
    }

    #[test]
    fn hide_resets_hover_and_cancels_old_timer() {
        let clock = FakeClock::default();
        let mut state = State::default();
        state.show(toast("image"), clock.now());
        state.hover(true, clock.now());
        state.hide();
        state.hover(true, clock.now()); // Hidden hover is ignored.
        state.show(toast("text"), clock.advance(1));
        state.tick(clock.advance(4200));
        assert!(!state.visible);
        assert!(state.last.is_some());
    }

    #[test]
    fn bottom_right_uses_work_area_in_global_dip() {
        assert_eq!(
            placement(Rect::new(-2560, 279, 2560, 1400)),
            Rect::new(-396, 1571, 380, 92)
        );
    }

    #[test]
    fn hover_route_accepts_only_the_documented_boolean_action() {
        assert!(hover_action(&serde_json::json!({"type":"hover","over":true})).unwrap());
        assert!(!hover_action(&serde_json::json!({"type":"hover","over":false})).unwrap());
        assert!(hover_action(&serde_json::json!({"type":"hover","over":"true"})).is_err());
        assert!(hover_action(&serde_json::json!({"type":"click"})).is_err());
    }
}
