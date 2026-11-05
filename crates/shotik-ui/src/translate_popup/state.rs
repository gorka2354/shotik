//! Popup content, sizing and the hover/arrival races from windows.js.
use serde_json::{json, Value};
use shotik_core::{ports::TranslatePopup, PointF, Rect};

#[derive(Default, Debug)]
pub struct State {
    pub content: TranslatePopup,
    pub displayed: String,
    pub copy_text: String,
    pub visible: bool,
    pub hovering: bool,
    pub deadline: Option<u64>,
    pub bounds: Rect,
    pub ttl: u64,
}

impl State {
    pub fn new(ttl: u64) -> Self {
        Self { ttl, ..Self::default() }
    }

    fn content(&mut self, content: TranslatePopup, loading_text: &str) {
        self.displayed = if content.loading {
            content
                .loading_text
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| loading_text.into())
        } else if let Some(error) = content.error.as_ref().filter(|s| !s.is_empty()) {
            error.clone()
        } else {
            self.copy_text = content.translated.clone().unwrap_or_default();
            if self.copy_text.is_empty() {
                "—".into()
            } else {
                self.copy_text.clone()
            }
        };
        self.content = content;
    }

    pub fn show(&mut self, content: TranslatePopup, loading_text: &str, now: u64) {
        self.deadline = (!content.loading).then_some(now + self.ttl);
        self.visible = true;
        self.content(content, loading_text);
    }

    pub fn update(&mut self, content: TranslatePopup, loading_text: &str, now: u64) {
        // The JS update path does not reopen a closed popup.
        if !self.hovering {
            self.deadline = Some(now + self.ttl);
        }
        self.content(content, loading_text);
    }

    pub fn hover(&mut self, over: bool, now: u64) {
        self.hovering = over;
        self.deadline = (!over).then_some(now + self.ttl);
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.hovering = false;
        self.deadline = None;
    }

    pub fn tick(&mut self, now: u64) {
        if self.deadline.is_some_and(|at| now >= at) {
            self.hide();
        }
    }

    pub fn resize(&mut self, height: f64) {
        self.bounds.height = ((height + 0.5).floor() as i32).clamp(120, 520);
    }

    pub fn snapshot(&self) -> Value {
        json!({ "original": self.content.original, "translated": self.displayed, "loading": self.content.loading })
    }

    pub fn meta(&self) -> String {
        if self.content.loading || self.content.error.as_ref().is_some_and(|error| !error.is_empty()) {
            return String::new();
        }
        self.content
            .source
            .as_deref()
            .filter(|source| !source.is_empty() && *source != "auto")
            .map(|source| format!("{source} → {}", self.content.target.as_deref().unwrap_or("")))
            .unwrap_or_default()
    }
}

pub fn placement(at: PointF, area: Rect) -> Rect {
    let mut x = at.x + 12.0;
    let mut y = at.y + 16.0;
    if x + 380.0 > f64::from(area.x + area.width) {
        x = f64::from(area.x + area.width - 380 - 8);
    }
    if x < f64::from(area.x) {
        x = f64::from(area.x + 8);
    }
    if y > f64::from(area.y + area.height - 160) {
        y = f64::from(area.y + area.height - 240);
    }
    Rect::new((x + 0.5).floor() as i32, (y + 0.5).floor() as i32, 380, 220)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::test_clock::FakeClock;
    fn loading() -> TranslatePopup {
        TranslatePopup {
            original: "hello".into(),
            loading: true,
            ..Default::default()
        }
    }
    fn result() -> TranslatePopup {
        TranslatePopup {
            original: "hello".into(),
            translated: Some("привет".into()),
            ..Default::default()
        }
    }

    #[test]
    fn loading_has_no_initial_timeout_and_result_starts_a_fresh_ttl() {
        let c = FakeClock::default();
        let mut s = State::new(300);
        s.show(loading(), "Translating…", c.now());
        s.tick(c.advance(1000));
        assert!(s.visible);
        assert_eq!(
            s.snapshot(),
            json!({ "original":"hello", "translated":"Translating…", "loading":true })
        );
        s.update(result(), "…", c.now());
        s.tick(c.advance(299));
        assert!(s.visible);
        s.tick(c.advance(1));
        assert!(!s.visible);
    }

    #[test]
    fn result_under_hover_does_not_rearm_and_leave_restarts_full_ttl() {
        let c = FakeClock::default();
        let mut s = State::new(400);
        s.show(loading(), "…", c.now());
        s.hover(true, c.now());
        s.update(result(), "…", c.advance(200));
        s.tick(c.advance(1000));
        assert!(s.visible);
        assert_eq!(s.deadline, None);
        s.hover(false, c.now());
        s.tick(c.advance(399));
        assert!(s.visible);
        s.tick(c.advance(1));
        assert!(!s.visible);
        assert!(!s.hovering);
    }

    #[test]
    fn explicit_hide_clears_hover_and_late_results_do_not_reopen() {
        let mut s = State::new(300);
        s.show(loading(), "…", 0);
        s.hover(true, 0);
        s.hide();
        s.update(result(), "…", 1);
        assert!(!s.visible);
        s.show(result(), "…", 2);
        assert_eq!(s.deadline, Some(302));
        assert_eq!(
            s.snapshot(),
            json!({"original":"hello", "translated":"привет", "loading":false})
        );
    }

    #[test]
    fn copy_content_error_and_loading_follow_the_reference_renderer() {
        let mut s = State::new(9000);
        s.show(result(), "…", 0);
        s.update(
            TranslatePopup {
                error: Some("offline".into()),
                ..Default::default()
            },
            "…",
            1,
        );
        assert_eq!(s.displayed, "offline");
        assert_eq!(s.copy_text, "привет");
        s.update(loading(), "…", 2);
        assert_eq!(s.copy_text, "привет");
    }

    #[test]
    fn bounds_flip_near_work_area_edge_and_resize_is_clamped() {
        let mut s = State::new(9000);
        s.bounds = placement(PointF { x: 990.0, y: 790.0 }, Rect::new(0, 0, 1000, 800));
        assert_eq!(s.bounds, Rect::new(612, 560, 380, 220));
        s.resize(5000.0);
        assert_eq!(s.bounds.height, 520);
        s.resize(1.0);
        assert_eq!(s.bounds.height, 120);
        s.resize(181.6);
        assert_eq!(s.bounds.height, 182);
    }

    #[test]
    fn language_pair_is_only_shown_for_a_non_auto_result() {
        let mut state = State::new(9000);
        let mut content = result();
        content.source = Some("en".into());
        content.target = Some("ru".into());
        state.show(content.clone(), "…", 0);
        assert_eq!(state.meta(), "en → ru");
        content.error = Some(String::new());
        state.update(content.clone(), "…", 1);
        assert_eq!(state.meta(), "en → ru");
        content.error = Some("offline".into());
        state.update(content.clone(), "…", 2);
        assert_eq!(state.meta(), "");
        content.error = None;
        content.loading = true;
        state.update(content.clone(), "…", 3);
        assert_eq!(state.meta(), "");
        content.loading = false;
        content.source = Some("auto".into());
        state.update(content, "…", 4);
        assert_eq!(state.meta(), "");
    }
}
