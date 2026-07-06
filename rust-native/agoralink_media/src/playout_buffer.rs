use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::h264_reassembly::EncodedFrame;

const MAX_PLAYOUT_DELAY_MS: u64 = 500;
const LATE_TOLERANCE: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, Default)]
pub struct PlayoutStats {
    pub buffer_peak_frames: usize,
    pub late_frames: u64,
    pub dropped_late_frames: u64,
    pub dropped_discontinuity_frames: u64,
    pub delay_actual_ms_total: f64,
    pub delay_actual_ms_max: f64,
    pub released_frames: u64,
}

impl PlayoutStats {
    pub fn delay_actual_ms_avg(self) -> f64 {
        if self.released_frames == 0 {
            0.0
        } else {
            self.delay_actual_ms_total / self.released_frames as f64
        }
    }
}

struct BufferedFrame {
    frame: EncodedFrame,
    completed_at: Instant,
    target_playout_at: Instant,
}

pub struct PlayoutBuffer {
    delay: Duration,
    frames: VecDeque<BufferedFrame>,
    first_timestamp_ms: Option<u64>,
    first_target_at: Option<Instant>,
    last_target_at: Option<Instant>,
    stats: PlayoutStats,
}

impl PlayoutBuffer {
    pub fn new(playout_delay_ms: u64) -> Result<Self, String> {
        if playout_delay_ms > MAX_PLAYOUT_DELAY_MS {
            return Err(format!(
                "playout-delay-ms must be between 0 and {MAX_PLAYOUT_DELAY_MS}"
            ));
        }
        Ok(Self {
            delay: Duration::from_millis(playout_delay_ms),
            frames: VecDeque::new(),
            first_timestamp_ms: None,
            first_target_at: None,
            last_target_at: None,
            stats: PlayoutStats::default(),
        })
    }

    pub fn delay_ms(&self) -> u64 {
        self.delay.as_millis() as u64
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn stats(&self) -> PlayoutStats {
        self.stats
    }

    pub fn push_frames(&mut self, frames: Vec<EncodedFrame>, completed_at: Instant) {
        for frame in frames {
            let target_playout_at = self.target_playout_at(frame.timestamp_ms, completed_at);
            self.last_target_at = Some(target_playout_at);
            self.frames.push_back(BufferedFrame {
                frame,
                completed_at,
                target_playout_at,
            });
        }
        self.stats.buffer_peak_frames = self.stats.buffer_peak_frames.max(self.frames.len());
    }

    pub fn pop_due(&mut self, now: Instant) -> Vec<EncodedFrame> {
        let mut due = Vec::new();
        while self
            .frames
            .front()
            .is_some_and(|buffered| buffered.target_playout_at <= now)
        {
            let Some(buffered) = self.frames.pop_front() else {
                break;
            };
            let lateness = now
                .checked_duration_since(buffered.target_playout_at)
                .unwrap_or_default();
            if lateness > LATE_TOLERANCE {
                self.stats.late_frames += 1;
            }
            let actual_delay_ms = now
                .checked_duration_since(buffered.completed_at)
                .unwrap_or_default()
                .as_secs_f64()
                * 1000.0;
            self.stats.delay_actual_ms_total += actual_delay_ms;
            self.stats.delay_actual_ms_max = self.stats.delay_actual_ms_max.max(actual_delay_ms);
            self.stats.released_frames += 1;
            due.push(buffered.frame);
        }
        due
    }

    pub fn clear_for_discontinuity(&mut self) {
        self.stats.dropped_discontinuity_frames += self.frames.len() as u64;
        self.frames.clear();
        self.first_timestamp_ms = None;
        self.first_target_at = None;
        self.last_target_at = None;
    }

    fn target_playout_at(&mut self, timestamp_ms: u64, completed_at: Instant) -> Instant {
        let fallback = completed_at.checked_add(self.delay).unwrap_or(completed_at);
        let first_timestamp_ms = *self.first_timestamp_ms.get_or_insert(timestamp_ms);
        let first_target_at = *self.first_target_at.get_or_insert(fallback);
        let target = timestamp_ms
            .checked_sub(first_timestamp_ms)
            .and_then(|delta_ms| first_target_at.checked_add(Duration::from_millis(delta_ms)))
            .unwrap_or(fallback);
        self.last_target_at.map_or(target, |last| target.max(last))
    }
}

pub fn run_self_test() -> Result<(), String> {
    fn frame(frame_id: u64, timestamp_ms: u64) -> EncodedFrame {
        EncodedFrame {
            frame_id,
            flags: 0,
            timestamp_ms,
            bytes: vec![frame_id as u8],
        }
    }

    for delay in [0, 120, 500] {
        PlayoutBuffer::new(delay)?;
    }
    if PlayoutBuffer::new(501).is_ok() {
        return Err("playout delay above 500ms was accepted".to_string());
    }

    let now = Instant::now();
    let mut buffer = PlayoutBuffer::new(120)?;
    buffer.push_frames(vec![frame(0, 1000), frame(1, 1033)], now);
    if !buffer.pop_due(now + Duration::from_millis(119)).is_empty() {
        return Err("playout buffer released a frame before its target".to_string());
    }
    let first = buffer.pop_due(now + Duration::from_millis(120));
    if first.len() != 1 || first[0].frame_id != 0 {
        return Err("playout buffer did not release the first frame on time".to_string());
    }
    let second = buffer.pop_due(now + Duration::from_millis(153));
    if second.len() != 1 || second[0].frame_id != 1 || buffer.stats().released_frames != 2 {
        return Err("playout buffer timestamp pacing failed".to_string());
    }

    let mut immediate = PlayoutBuffer::new(0)?;
    immediate.push_frames(vec![frame(0, 1000)], now);
    if immediate.pop_due(now).len() != 1 {
        return Err("zero-delay playout did not release immediately".to_string());
    }
    Ok(())
}
