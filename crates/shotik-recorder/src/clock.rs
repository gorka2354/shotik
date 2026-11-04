//! Media time. Every source stamps its data with the shared clock (QPC, in 100 ns ticks) and this
//! module maps those stamps to the timeline of the file, so pause leaves neither a gap nor drift:
//! video and audio ask the same [`Timeline`].

pub const TICKS_PER_SEC: i64 = 10_000_000;

/// The part of a stamped span that belongs in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    /// Where it starts in media time.
    pub media: i64,
    /// Offset from the start of the span.
    pub offset: i64,
    pub len: i64,
}

#[derive(Clone, Debug, Default)]
pub struct Timeline {
    origin: Option<i64>,
    /// Finished pauses, oldest first: `[start, end)` on the shared clock.
    pauses: Vec<(i64, i64)>,
    open_pause: Option<i64>,
    stopped: Option<i64>,
}

impl Timeline {
    pub fn start(&mut self, now: i64) {
        self.origin = Some(now);
    }

    pub fn is_started(&self) -> bool {
        self.origin.is_some()
    }

    pub fn pause(&mut self, now: i64) {
        if self.open_pause.is_none() && self.stopped.is_none() {
            self.open_pause = Some(now);
        }
    }

    pub fn resume(&mut self, now: i64) {
        if self.stopped.is_some() {
            return;
        }
        if let Some(start) = self.open_pause.take() {
            self.pauses.push((start, now.max(start)));
        }
    }

    pub fn stop(&mut self, now: i64) {
        if self.stopped.is_none() {
            self.stopped = Some(now);
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.is_some()
    }

    /// Media time reached at `now`, frozen during a pause and after a stop. None before the start.
    pub fn position(&self, now: i64) -> Option<i64> {
        self.origin?;
        let mut t = now;
        if let Some(start) = self.open_pause {
            t = t.min(start);
        }
        if let Some(stop) = self.stopped {
            t = t.min(stop);
        }
        Some(self.media_of(t))
    }

    fn media_of(&self, t: i64) -> i64 {
        let origin = self.origin.unwrap_or(t);
        let mut media = t - origin;
        for &(start, end) in &self.pauses {
            if t >= end {
                media -= end - start;
            } else if t > start {
                media -= t - start;
            }
        }
        media.max(0)
    }

    /// Calls `piece` for each part of `[at, at + len)` that is on the timeline: what precedes the
    /// start or falls inside a pause is left out. Data after a stop still maps, the last audio
    /// frame may end slightly later than the stop.
    pub fn place(&self, at: i64, len: i64, mut piece: impl FnMut(Piece)) {
        let Some(origin) = self.origin else { return };
        let end = at + len;
        let mut cur = at.max(origin);
        for &(start, stop) in &self.pauses {
            if cur >= stop {
                continue;
            }
            if end <= start {
                break;
            }
            if cur < start {
                piece(Piece { media: self.media_of(cur), offset: cur - at, len: start - cur });
            }
            cur = stop.max(cur);
            if cur >= end {
                return;
            }
        }
        let limit = self.open_pause.map_or(end, |start| end.min(start));
        if cur < limit {
            piece(Piece { media: self.media_of(cur), offset: cur - at, len: limit - cur });
        }
    }
}

/// Constant frame rate: which frames exist at a given media time, and their stamps.
#[derive(Clone, Copy, Debug)]
pub struct FramePacer {
    fps: u32,
    written: u64,
}

impl FramePacer {
    pub fn new(fps: u32) -> Self {
        Self { fps: fps.max(1), written: 0 }
    }

    pub fn written(&self) -> u64 {
        self.written
    }

    pub fn advance(&mut self) {
        self.written += 1;
    }

    /// Stamp of frame `n`, without accumulated rounding error.
    pub fn pts(&self, n: u64) -> i64 {
        (i128::from(n) * i128::from(TICKS_PER_SEC) / i128::from(self.fps)) as i64
    }

    pub fn duration(&self, n: u64) -> i64 {
        self.pts(n + 1) - self.pts(n)
    }

    /// How many frames must exist at media time `pos`: frame `n` is due at its own stamp.
    pub fn due_count(&self, pos: i64) -> u64 {
        let pos = pos.max(0);
        let whole = (i128::from(pos) * i128::from(self.fps) / i128::from(TICKS_PER_SEC)) as u64;
        // the stamps are rounded down, so a position can reach the next stamp a tick before the division says
        if self.pts(whole + 1) <= pos {
            whole + 2
        } else {
            whole + 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = TICKS_PER_SEC / 1000;

    fn pieces(t: &Timeline, at: i64, len: i64) -> Vec<Piece> {
        let mut out = Vec::new();
        t.place(at, len, |p| out.push(p));
        out
    }

    #[test]
    fn nothing_maps_before_the_start() {
        let t = Timeline::default();
        assert_eq!(t.position(5), None);
        assert!(pieces(&t, 0, 100).is_empty());
    }

    #[test]
    fn position_is_frozen_during_a_pause_and_after_a_stop() {
        let mut t = Timeline::default();
        t.start(1000 * MS);
        assert_eq!(t.position(1500 * MS), Some(500 * MS));
        t.pause(1500 * MS);
        assert_eq!(t.position(1900 * MS), Some(500 * MS));
        t.resume(2000 * MS);
        assert_eq!(t.position(2000 * MS), Some(500 * MS), "no gap at the resume");
        assert_eq!(t.position(2100 * MS), Some(600 * MS));
        t.stop(2200 * MS);
        assert_eq!(t.position(3000 * MS), Some(700 * MS));
    }

    #[test]
    fn stopping_during_a_pause_ends_at_the_pause_point() {
        let mut t = Timeline::default();
        t.start(0);
        t.pause(100 * MS);
        t.stop(300 * MS);
        t.resume(400 * MS);
        assert_eq!(t.position(500 * MS), Some(100 * MS));
    }

    #[test]
    fn a_span_before_the_start_is_trimmed() {
        let mut t = Timeline::default();
        t.start(100 * MS);
        assert!(pieces(&t, 50 * MS, 10 * MS).is_empty());
        assert_eq!(pieces(&t, 95 * MS, 10 * MS), vec![Piece { media: 0, offset: 5 * MS, len: 5 * MS }]);
    }

    #[test]
    fn a_span_across_a_pause_is_joined_without_a_gap() {
        let mut t = Timeline::default();
        t.start(0);
        t.pause(100 * MS);
        // while paused: the part after the pause point is dropped
        assert_eq!(pieces(&t, 95 * MS, 10 * MS), vec![Piece { media: 95 * MS, offset: 0, len: 5 * MS }]);
        assert!(pieces(&t, 150 * MS, 10 * MS).is_empty());
        t.resume(200 * MS);
        let got = pieces(&t, 95 * MS, 110 * MS);
        assert_eq!(
            got,
            vec![
                Piece { media: 95 * MS, offset: 0, len: 5 * MS },
                Piece { media: 100 * MS, offset: 105 * MS, len: 5 * MS },
            ],
            "the second part starts exactly where the first ended"
        );
        assert_eq!(pieces(&t, 250 * MS, 10 * MS), vec![Piece { media: 150 * MS, offset: 0, len: 10 * MS }]);
    }

    #[test]
    fn several_pauses_add_up() {
        let mut t = Timeline::default();
        t.start(0);
        t.pause(100 * MS);
        t.resume(200 * MS);
        t.pause(300 * MS);
        t.resume(500 * MS);
        assert_eq!(t.position(600 * MS), Some(300 * MS));
        assert_eq!(pieces(&t, 550 * MS, 10 * MS)[0].media, 250 * MS);
        assert_eq!(pieces(&t, 150 * MS, 10 * MS), vec![]);
        assert_eq!(pieces(&t, 250 * MS, 10 * MS)[0].media, 150 * MS);
    }

    #[test]
    fn data_after_a_stop_still_maps() {
        let mut t = Timeline::default();
        t.start(0);
        t.stop(100 * MS);
        assert_eq!(pieces(&t, 105 * MS, 10 * MS), vec![Piece { media: 105 * MS, offset: 0, len: 10 * MS }]);
    }

    #[test]
    fn frame_stamps_do_not_accumulate_rounding() {
        let pacer = FramePacer::new(30);
        assert_eq!(pacer.pts(0), 0);
        assert_eq!(pacer.pts(30), TICKS_PER_SEC);
        assert_eq!(pacer.pts(30 * 3600), 3600 * TICKS_PER_SEC);
        let total: i64 = (0..300).map(|n| pacer.duration(n)).sum();
        assert_eq!(total, 10 * TICKS_PER_SEC);
    }

    #[test]
    fn frame_zero_is_due_at_once_and_each_next_at_its_stamp() {
        let pacer = FramePacer::new(30);
        assert_eq!(pacer.due_count(0), 1);
        assert_eq!(pacer.due_count(pacer.pts(1) - 1), 1);
        assert_eq!(pacer.due_count(pacer.pts(1)), 2);
        assert_eq!(pacer.due_count(TICKS_PER_SEC), 31);
    }
}
