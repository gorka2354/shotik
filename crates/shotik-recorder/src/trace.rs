//! The diagnostic lines of the current session, `trace` in `recorder.status()`.

use std::sync::Mutex;

/// `rec:trace` in `src/main/recorder.js` keeps the last 30.
const MAX_LINES: usize = 30;

#[derive(Debug, Default)]
pub(crate) struct Trace {
    lines: Mutex<Vec<String>>,
}

impl Trace {
    pub fn push(&self, line: impl Into<String>) {
        let mut lines = self.lines.lock().unwrap();
        lines.push(line.into());
        if lines.len() > MAX_LINES {
            lines.remove(0);
        }
    }

    pub fn reset(&self) {
        self.lines.lock().unwrap().clear();
    }

    pub fn snapshot(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_last_lines() {
        let trace = Trace::default();
        for i in 0..40 {
            trace.push(format!("{i}"));
        }
        let lines = trace.snapshot();
        assert_eq!(lines.len(), MAX_LINES);
        assert_eq!(lines[0], "10");
        assert_eq!(lines[MAX_LINES - 1], "39");
        trace.reset();
        assert!(trace.snapshot().is_empty());
    }
}
