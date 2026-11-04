//! Pure transition rules from `src/main/selection-watch.ps1`.
use shotik_core::ports::SelectionEvent;

pub const POLL_MS: u64 = 55;
pub const IDLE_TICKS: u32 = 6;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Position {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Default)]
pub struct SelectionRead {
    pub text: String,
    pub has_pattern: bool,
    /// Invariant, case-insensitive sort key of the trimmed full text, as used by
    /// PowerShell's -ne. Computing it is an adapter effect, not a state transition.
    pub comparison_key: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct Tick {
    pub events: Vec<SelectionEvent>,
    pub read_selection: bool,
    released: bool,
}

#[derive(Debug, Default)]
pub struct WatchState {
    was_down: bool,
    down_at: Position,
    last_press: Option<(u64, Position)>,
    double: bool,
    last_emitted: String,
    last_comparison: Vec<u8>,
    idle: u32,
    empty_streak: u32,
}

impl WatchState {
    /// Called once per 55ms tick. Reading UIA is an effect, performed only when requested.
    pub fn tick(&mut self, now_ms: u64, down: bool, cursor: Position) -> Tick {
        let pressed = !self.was_down && down;
        let released = self.was_down && !down;
        self.was_down = down;
        let mut tick = Tick {
            released,
            ..Tick::default()
        };
        if pressed {
            self.down_at = cursor;
            tick.events.push(SelectionEvent::MouseDown {
                x: cursor.x,
                y: cursor.y,
            });
            self.double = self.last_press.is_some_and(|(time, pos)| {
                now_ms.saturating_sub(time) < 450
                    && (i64::from(cursor.x) - i64::from(pos.x)).abs() <= 8
                    && (i64::from(cursor.y) - i64::from(pos.y)).abs() <= 8
            });
            self.last_press = Some((now_ms, cursor));
        }
        if down {
            self.idle = 0;
            return tick;
        }
        self.idle = self.idle.wrapping_add(1);
        tick.read_selection = released || self.idle % IDLE_TICKS == 0;
        tick
    }

    /// Supplies the UIA read and physical cursor coordinates for the corresponding tick.
    pub fn selection(
        &mut self,
        tick: &Tick,
        info: SelectionRead,
        cursor: Position,
    ) -> Vec<SelectionEvent> {
        if !tick.read_selection {
            return Vec::new();
        }
        let selected = info.text.trim();
        if selected.is_empty() {
            if tick.released && !info.has_pattern {
                let dx = (i64::from(cursor.x) - i64::from(self.down_at.x)).abs();
                let dy = (i64::from(cursor.y) - i64::from(self.down_at.y)).abs();
                if self.double && dx < 25 {
                    self.double = false;
                    self.empty_streak = 0;
                    return vec![SelectionEvent::Gesture {
                        x1: cursor.x,
                        y1: cursor.y,
                        x2: cursor.x,
                        y2: cursor.y,
                        double: true,
                    }];
                }
                if dx >= 25 && dx >= dy {
                    self.empty_streak = 0;
                    return vec![SelectionEvent::Gesture {
                        x1: self.down_at.x,
                        y1: self.down_at.y,
                        x2: cursor.x,
                        y2: cursor.y,
                        double: false,
                    }];
                }
            }
            self.empty_streak = self.empty_streak.saturating_add(1);
            if !self.last_emitted.is_empty() && self.empty_streak >= 2 {
                self.last_emitted.clear();
                self.last_comparison.clear();
                return vec![SelectionEvent::Cleared];
            }
            return Vec::new();
        }
        self.empty_streak = 0;
        if !self.last_emitted.is_empty() && info.comparison_key == self.last_comparison {
            return Vec::new();
        }
        self.last_emitted = selected.to_owned();
        self.last_comparison = info.comparison_key;
        // The script caps .NET strings at 5000 UTF-16 code units. Avoid emitting
        // an unpaired surrogate at the boundary (Rust strings are valid Unicode).
        let mut units = 0;
        let text = selected
            .chars()
            .take_while(|c| {
                units += c.len_utf16();
                units <= 5000
            })
            .collect();
        vec![SelectionEvent::Selected {
            text,
            x: cursor.x,
            y: cursor.y,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(state: &mut WatchState, text: &str, has_pattern: bool) -> Vec<SelectionEvent> {
        let tick = Tick {
            read_selection: true,
            ..Tick::default()
        };
        state.selection(
            &tick,
            SelectionRead {
                text: text.into(),
                has_pattern,
                comparison_key: text.trim().to_lowercase().into_bytes(),
            },
            Position::default(),
        )
    }

    #[test]
    fn reads_every_330ms_and_immediately_on_release_but_never_while_dragging() {
        let mut state = WatchState::default();
        for i in 1..=18 {
            assert_eq!(
                state
                    .tick(i * POLL_MS, false, Position::default())
                    .read_selection,
                i % 6 == 0
            );
        }
        let at = Position { x: -2500, y: 694 };
        assert_eq!(
            state.tick(1100, true, at).events,
            vec![SelectionEvent::MouseDown { x: -2500, y: 694 }]
        );
        for i in 1..=20 {
            assert!(!state.tick(1100 + i * POLL_MS, true, at).read_selection);
        }
        assert!(state.tick(2300, false, at).read_selection);
        for i in 1..5 {
            assert!(!state.tick(2300 + i * POLL_MS, false, at).read_selection);
        }
        assert!(state.tick(2575, false, at).read_selection);
    }

    #[test]
    fn trims_deduplicates_and_debounces_transient_empty_read() {
        let mut state = WatchState::default();
        assert_eq!(
            read(&mut state, "  hello\n", true),
            vec![SelectionEvent::Selected {
                text: "hello".into(),
                x: 0,
                y: 0
            }]
        );
        assert!(read(&mut state, "hello", true).is_empty());
        assert!(read(&mut state, "HELLO", true).is_empty());
        assert!(read(&mut state, "", false).is_empty());
        assert!(read(&mut state, "hello", true).is_empty());
        assert!(read(&mut state, " ", true).is_empty());
        assert_eq!(read(&mut state, "", true), vec![SelectionEvent::Cleared]);
        assert!(read(&mut state, "", true).is_empty());
        assert_eq!(read(&mut state, "hello", true).len(), 1);
        assert_eq!(read(&mut state, "ПРИВЕТ", true).len(), 1);
        assert!(read(&mut state, "привет", true).is_empty());
    }

    fn drag(dx: i32, dy: i32, has_pattern: bool) -> Vec<SelectionEvent> {
        let mut state = WatchState::default();
        state.tick(1000, true, Position { x: -100, y: 700 });
        let end = Position {
            x: -100 + dx,
            y: 700 + dy,
        };
        let tick = state.tick(1055, false, end);
        state.selection(
            &tick,
            SelectionRead {
                has_pattern,
                ..SelectionRead::default()
            },
            end,
        )
    }

    #[test]
    fn only_horizontal_spans_of_at_least_25px_in_custom_controls_are_gestures() {
        assert!(drag(24, 0, false).is_empty());
        assert!(drag(25, 26, false).is_empty());
        assert!(drag(150, 0, true).is_empty());
        assert_eq!(
            drag(-25, 25, false),
            vec![SelectionEvent::Gesture {
                x1: -100,
                y1: 700,
                x2: -125,
                y2: 725,
                double: false,
            }]
        );
    }

    fn double_click(delta_ms: u64, offset: i32, release_dx: i32) -> Vec<SelectionEvent> {
        let mut state = WatchState::default();
        let at = Position { x: 900, y: 500 };
        state.tick(1000, true, at);
        state.tick(1055, false, at);
        let next = Position {
            x: 900 + offset,
            y: 500,
        };
        state.tick(1000 + delta_ms, true, next);
        let end = Position {
            x: next.x + release_dx,
            y: next.y,
        };
        let tick = state.tick(1055 + delta_ms, false, end);
        state.selection(&tick, SelectionRead::default(), end)
    }

    #[test]
    fn double_click_uses_450ms_8px_and_a_release_span_under_25px() {
        assert!(matches!(
            double_click(449, 8, 24).as_slice(),
            [SelectionEvent::Gesture { double: true, .. }]
        ));
        assert!(double_click(450, 0, 0).is_empty());
        assert!(double_click(200, 9, 0).is_empty());
        assert!(matches!(
            double_click(200, 0, 25).as_slice(),
            [SelectionEvent::Gesture { double: false, .. }]
        ));
    }

    #[test]
    fn gesture_resets_empty_debounce_and_does_not_clear_a_pending_selection() {
        let mut state = WatchState::default();
        read(&mut state, "pending", true);
        read(&mut state, "", false);
        state.tick(1000, true, Position::default());
        let end = Position { x: 100, y: 0 };
        let tick = state.tick(1055, false, end);
        assert!(matches!(
            state
                .selection(&tick, SelectionRead::default(), end)
                .as_slice(),
            [SelectionEvent::Gesture { .. }]
        ));
        assert!(read(&mut state, "", false).is_empty());
        assert_eq!(read(&mut state, "", false), vec![SelectionEvent::Cleared]);
    }

    #[test]
    fn truncates_output_but_compares_the_full_selection() {
        let mut state = WatchState::default();
        let text = "я".repeat(5001);
        let result = read(&mut state, &text, true);
        assert!(
            matches!(&result[0], SelectionEvent::Selected { text, .. } if text.encode_utf16().count() == 5000)
        );
        assert!(read(&mut state, &text, true).is_empty());
        assert_eq!(read(&mut state, &("я".repeat(5000) + "a"), true).len(), 1);
    }
}
