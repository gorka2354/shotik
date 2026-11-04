//! The session state machine: one session at a time, the guard during a countdown, stop and cancel
//! at any point, and the safety timeout on finalizing. Pure: time is a parameter, effects are return
//! values. Reference: the variables `session` and `starting` in `src/main/recorder.js`.

use shotik_core::ports::{RecAudioInfo, RecLevels, RecStatus};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionId(u64);

/// What a stop or cancel asks of the rest of the recorder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Nothing,
    /// A start was waiting for its countdown; it will not go on.
    StartCalledOff,
    Stop(SessionId),
    Cancel(SessionId),
}

/// The worker did not report back in time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Expired {
    pub id: SessionId,
    pub canceled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    pub stop_ms: u64,
    pub cancel_ms: u64,
}

#[derive(Debug)]
struct Active {
    id: SessionId,
    audio_only: bool,
    paused: bool,
    stopping: bool,
    canceled: bool,
    deadline_ms: Option<u64>,
    audio: Option<RecAudioInfo>,
    levels: Option<RecLevels>,
    start_ts: i64,
}

#[derive(Debug)]
enum Phase {
    Idle,
    Starting { canceled: bool },
    Active(Active),
}

#[derive(Debug)]
pub struct SessionMachine {
    phase: Phase,
    last_id: u64,
    timeouts: Timeouts,
}

impl SessionMachine {
    pub fn new(timeouts: Timeouts) -> Self {
        Self { phase: Phase::Idle, last_id: 0, timeouts }
    }

    /// `isRecording()`: true while a start waits for its countdown too.
    pub fn is_recording(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    /// The first line of `startRecording`. False when a session runs or is starting.
    pub fn begin(&mut self) -> bool {
        if self.is_recording() {
            return false;
        }
        self.phase = Phase::Starting { canceled: false };
        true
    }

    pub fn start_called_off(&self) -> bool {
        matches!(self.phase, Phase::Starting { canceled: true })
    }

    /// The start fails before a session exists.
    pub fn abort_start(&mut self) {
        if matches!(self.phase, Phase::Starting { .. }) {
            self.phase = Phase::Idle;
        }
    }

    /// The countdown is over. None when it was called off.
    pub fn activate(&mut self, audio_only: bool, start_ts: i64) -> Option<SessionId> {
        let Phase::Starting { canceled } = self.phase else { return None };
        if canceled {
            self.phase = Phase::Idle;
            return None;
        }
        self.last_id += 1;
        let id = SessionId(self.last_id);
        self.phase = Phase::Active(Active {
            id,
            audio_only,
            paused: false,
            stopping: false,
            canceled: false,
            deadline_ms: None,
            audio: None,
            levels: None,
            start_ts,
        });
        Some(id)
    }

    pub fn request_stop(&mut self, now_ms: u64) -> Request {
        match &mut self.phase {
            Phase::Idle => Request::Nothing,
            Phase::Starting { canceled } => {
                *canceled = true;
                Request::StartCalledOff
            }
            Phase::Active(a) if a.stopping || a.canceled => Request::Nothing,
            Phase::Active(a) => {
                a.stopping = true;
                a.deadline_ms = Some(now_ms + self.timeouts.stop_ms);
                Request::Stop(a.id)
            }
        }
    }

    /// Also after a stop: until the file is final, cancel wins.
    pub fn request_cancel(&mut self, now_ms: u64) -> Request {
        match &mut self.phase {
            Phase::Idle => Request::Nothing,
            Phase::Starting { canceled } => {
                *canceled = true;
                Request::StartCalledOff
            }
            Phase::Active(a) if a.canceled => Request::Nothing,
            Phase::Active(a) => {
                a.canceled = true;
                let deadline = now_ms + self.timeouts.cancel_ms;
                a.deadline_ms = Some(a.deadline_ms.map_or(deadline, |d| d.min(deadline)));
                Request::Cancel(a.id)
            }
        }
    }

    /// Returns the session when the state changed.
    pub fn pause(&mut self) -> Option<SessionId> {
        match &mut self.phase {
            Phase::Active(a) if !a.paused && !a.stopping && !a.canceled => {
                a.paused = true;
                Some(a.id)
            }
            _ => None,
        }
    }

    pub fn resume(&mut self) -> Option<SessionId> {
        match &mut self.phase {
            Phase::Active(a) if a.paused && !a.stopping && !a.canceled => {
                a.paused = false;
                Some(a.id)
            }
            _ => None,
        }
    }

    fn current(&mut self, id: SessionId) -> Option<&mut Active> {
        match &mut self.phase {
            Phase::Active(a) if a.id == id => Some(a),
            _ => None,
        }
    }

    /// Capture really runs now: from here on `audio` is in the status.
    pub fn set_ready(&mut self, id: SessionId, audio: RecAudioInfo) -> bool {
        let Some(a) = self.current(id) else { return false };
        a.audio = Some(audio);
        true
    }

    pub fn set_levels(&mut self, id: SessionId, levels: RecLevels) -> bool {
        let Some(a) = self.current(id) else { return false };
        a.levels = Some(levels);
        true
    }

    /// The worker is done. False when the timeout had already written the session off.
    pub fn settle(&mut self, id: SessionId) -> bool {
        if self.current(id).is_none() {
            return false;
        }
        self.phase = Phase::Idle;
        true
    }

    /// When the watchdog must look again.
    pub fn deadline_ms(&self) -> Option<u64> {
        match &self.phase {
            Phase::Active(a) => a.deadline_ms,
            _ => None,
        }
    }

    pub fn current_id(&self) -> Option<SessionId> {
        match &self.phase {
            Phase::Active(a) => Some(a.id),
            _ => None,
        }
    }

    /// The safety timeout: a worker stuck in finalizing must not keep the recorder busy forever.
    pub fn expire(&mut self, now_ms: u64) -> Option<Expired> {
        let Phase::Active(a) = &self.phase else { return None };
        if a.deadline_ms.is_none_or(|deadline| now_ms < deadline) {
            return None;
        }
        let expired = Expired { id: a.id, canceled: a.canceled };
        self.phase = Phase::Idle;
        Some(expired)
    }

    /// `status()`: during a countdown there is no session yet, so `recording` is false.
    pub fn status(&self, bytes: u64, trace: Vec<String>) -> RecStatus {
        let Phase::Active(a) = &self.phase else {
            return RecStatus { recording: false, trace, ..RecStatus::default() };
        };
        RecStatus {
            recording: true,
            audio_only: Some(a.audio_only),
            paused: Some(a.paused),
            audio: a.audio.clone(),
            levels: a.levels.clone(),
            start_ts: Some(a.start_ts),
            bytes: Some(bytes),
            trace,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMEOUTS: Timeouts = Timeouts { stop_ms: 4000, cancel_ms: 500 };

    fn machine() -> SessionMachine {
        SessionMachine::new(TIMEOUTS)
    }

    fn running(audio_only: bool) -> (SessionMachine, SessionId) {
        let mut m = machine();
        assert!(m.begin());
        let id = m.activate(audio_only, 123).unwrap();
        (m, id)
    }

    #[test]
    fn idle_status_has_only_recording_and_trace() {
        let m = machine();
        let status = m.status(0, vec!["line".into()]);
        assert!(!status.recording);
        assert_eq!(status.trace, vec!["line".to_string()]);
        assert_eq!(status.audio_only, None);
        assert_eq!(status.audio, None);
        assert!(!m.is_recording());
    }

    #[test]
    fn a_second_start_is_refused_during_a_countdown_and_during_a_session() {
        let mut m = machine();
        assert!(m.begin());
        assert!(m.is_recording(), "counts as recording while the countdown runs");
        assert!(!m.status(0, vec![]).recording, "yet there is no session to report");
        assert!(!m.begin(), "second start refused");
        let id = m.activate(false, 1).unwrap();
        assert!(!m.begin(), "refused while the session runs");
        assert_eq!(m.current_id(), Some(id));
    }

    #[test]
    fn stop_during_the_countdown_calls_the_start_off() {
        let mut m = machine();
        m.begin();
        assert_eq!(m.request_stop(0), Request::StartCalledOff);
        assert!(m.start_called_off());
        assert_eq!(m.activate(false, 1), None);
        assert!(!m.is_recording());
    }

    #[test]
    fn cancel_during_the_countdown_calls_the_start_off() {
        let mut m = machine();
        m.begin();
        assert_eq!(m.request_cancel(0), Request::StartCalledOff);
        assert_eq!(m.activate(false, 1), None);
        assert!(!m.is_recording());
    }

    #[test]
    fn a_failed_start_leaves_the_machine_idle() {
        let mut m = machine();
        m.begin();
        m.abort_start();
        assert!(!m.is_recording());
        assert!(m.begin());
    }

    #[test]
    fn audio_stays_absent_until_capture_really_runs() {
        let (mut m, id) = running(false);
        let status = m.status(0, vec![]);
        assert!(status.recording);
        assert_eq!(status.audio_only, Some(false));
        assert_eq!(status.audio, None);
        let info = RecAudioInfo { system: false, microphone: true, tracks: 1 };
        assert!(m.set_ready(id, info.clone()));
        assert_eq!(m.status(0, vec![]).audio, Some(info));
    }

    #[test]
    fn pause_and_resume_change_the_state_once() {
        let (mut m, id) = running(false);
        assert_eq!(m.resume(), None, "not paused");
        assert_eq!(m.pause(), Some(id));
        assert_eq!(m.status(0, vec![]).paused, Some(true));
        assert_eq!(m.pause(), None, "already paused");
        assert_eq!(m.resume(), Some(id));
        assert_eq!(m.status(0, vec![]).paused, Some(false));
    }

    #[test]
    fn nothing_changes_once_the_session_is_ending() {
        let (mut m, id) = running(false);
        assert_eq!(m.request_stop(0), Request::Stop(id));
        assert_eq!(m.request_stop(1), Request::Nothing, "second stop");
        assert_eq!(m.pause(), None);
        assert_eq!(m.request_cancel(2), Request::Cancel(id), "cancel still wins");
        assert_eq!(m.request_cancel(3), Request::Nothing);
    }

    #[test]
    fn the_safety_timeout_ends_a_stuck_session() {
        let (mut m, id) = running(false);
        assert_eq!(m.expire(1_000_000), None, "no deadline before a stop");
        m.request_stop(1000);
        assert_eq!(m.deadline_ms(), Some(5000));
        assert_eq!(m.expire(4999), None);
        assert_eq!(m.expire(5000), Some(Expired { id, canceled: false }));
        assert!(!m.is_recording());
        assert!(!m.settle(id), "a worker that comes back later is told it was written off");
    }

    #[test]
    fn cancel_has_the_shorter_grace_and_can_shorten_a_running_stop() {
        let (mut m, id) = running(false);
        m.request_stop(1000);
        m.request_cancel(2000);
        assert_eq!(m.deadline_ms(), Some(2500));
        assert_eq!(m.expire(2500), Some(Expired { id, canceled: true }));
    }

    #[test]
    fn a_worker_that_finishes_in_time_settles_and_the_next_session_starts() {
        let (mut m, id) = running(true);
        m.request_stop(0);
        assert!(m.settle(id));
        assert!(!m.is_recording());
        assert_eq!(m.expire(100_000), None);
        assert!(m.begin());
        let next = m.activate(false, 2).unwrap();
        assert_ne!(next, id);
        assert!(!m.settle(id), "an old worker cannot end the new session");
        assert!(!m.set_ready(id, RecAudioInfo::default()));
        assert!(!m.set_levels(id, RecLevels::default()));
        assert!(m.is_recording());
    }

    #[test]
    fn status_serializes_with_the_field_names_of_the_js_version() {
        let (mut m, id) = running(true);
        let before = serde_json::to_value(m.status(7, vec![])).unwrap();
        assert_eq!(before["recording"], true);
        assert_eq!(before["audioOnly"], true);
        assert_eq!(before["paused"], false);
        assert_eq!(before["startTs"], 123);
        assert_eq!(before["bytes"], 7);
        assert!(before.get("audio").is_none(), "absent while the session starts: {before}");
        assert!(before.get("levels").is_none());
        m.set_ready(id, RecAudioInfo { system: true, microphone: true, tracks: 1 });
        m.set_levels(id, RecLevels { system: 0.25, microphone: 0.5 });
        let after = serde_json::to_value(m.status(7, vec![])).unwrap();
        assert_eq!(after["audio"], serde_json::json!({"system": true, "microphone": true, "tracks": 1}));
        assert_eq!(after["levels"]["microphone"], 0.5);
    }
}
