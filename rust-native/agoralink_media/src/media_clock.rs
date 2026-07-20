use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub struct MediaTimestampUs(pub u64);

#[derive(Clone, Debug)]
pub struct MediaClock {
    started_at: Instant,
}

impl MediaClock {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
        }
    }

    pub fn now_us(&self) -> u64 {
        duration_to_us(self.started_at.elapsed())
    }
}

impl Default for MediaClock {
    fn default() -> Self {
        Self::new()
    }
}

/// Maps WASAPI's capture QPC timestamps onto an existing media-clock epoch.
///
/// The first valid capture timestamp is anchored to `MediaClock::now_us()`. Later
/// buffers retain the capture-side cadence instead of inheriting the time at
/// which the audio worker happened to drain them.
#[derive(Clone, Debug, Default)]
pub struct QpcMediaTimestampMapper {
    qpc_frequency: Option<u64>,
    anchor_qpc: Option<u64>,
    anchor_media_us: Option<u64>,
    qpc_errors: u64,
}

impl QpcMediaTimestampMapper {
    pub fn new() -> Self {
        Self {
            qpc_frequency: query_qpc_frequency(),
            ..Self::default()
        }
    }

    pub fn qpc_available(&self) -> bool {
        self.qpc_frequency.is_some()
    }

    pub fn qpc_errors(&self) -> u64 {
        self.qpc_errors
    }

    /// Returns `(media_timestamp_us, used_qpc)`.
    pub fn map_capture_timestamp(
        &mut self,
        qpc_position: u64,
        fallback_now_us: u64,
    ) -> (u64, bool) {
        let Some(frequency) = self.qpc_frequency else {
            return (fallback_now_us, false);
        };
        if qpc_position == 0 {
            self.qpc_errors = self.qpc_errors.saturating_add(1);
            return (fallback_now_us, false);
        }

        let anchor_qpc = *self.anchor_qpc.get_or_insert(qpc_position);
        let anchor_media_us = *self.anchor_media_us.get_or_insert(fallback_now_us);
        let Some(delta_qpc) = qpc_position.checked_sub(anchor_qpc) else {
            self.qpc_errors = self.qpc_errors.saturating_add(1);
            return (fallback_now_us, false);
        };
        let delta_us = u128::from(delta_qpc)
            .saturating_mul(1_000_000)
            .checked_div(u128::from(frequency))
            .unwrap_or_default()
            .min(u128::from(u64::MAX)) as u64;
        (anchor_media_us.saturating_add(delta_us), true)
    }
}

#[cfg(windows)]
fn query_qpc_frequency() -> Option<u64> {
    #[link(name = "Kernel32")]
    extern "system" {
        fn QueryPerformanceFrequency(frequency: *mut i64) -> i32;
    }

    let mut frequency = 0i64;
    let available = unsafe { QueryPerformanceFrequency(&mut frequency) } != 0;
    available
        .then_some(frequency)
        .and_then(|value| u64::try_from(value).ok())
}

#[cfg(not(windows))]
fn query_qpc_frequency() -> Option<u64> {
    None
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ReceiverMediaClockAnchor {
    pub first_media_timestamp_us: Option<u64>,
    pub receiver_clock_anchor_us: Option<u64>,
    pub playout_delay_ms: u64,
    pub last_audio_timestamp_us: Option<u64>,
    pub last_video_timestamp_us: Option<u64>,
}

impl ReceiverMediaClockAnchor {
    pub fn new(playout_delay_ms: u64) -> Self {
        Self {
            playout_delay_ms,
            ..Self::default()
        }
    }

    pub fn observe_audio(&mut self, timestamp: MediaTimestampUs, clock: &MediaClock) {
        self.observe(timestamp, clock);
        self.last_audio_timestamp_us = Some(timestamp.0);
    }

    pub fn observe_video(&mut self, timestamp: MediaTimestampUs, clock: &MediaClock) {
        self.observe(timestamp, clock);
        self.last_video_timestamp_us = Some(timestamp.0);
    }

    pub fn audio_video_timestamp_delta_ms(&self) -> Option<f64> {
        Some(
            (self.last_audio_timestamp_us? as i128 - self.last_video_timestamp_us? as i128) as f64
                / 1000.0,
        )
    }

    pub fn json_fragment(&self) -> String {
        format!(
            r#""first_media_timestamp_us":{},"receiver_clock_anchor_us":{},"audio_video_timestamp_delta_ms":{}"#,
            optional_u64_json(self.first_media_timestamp_us),
            optional_u64_json(self.receiver_clock_anchor_us),
            optional_f64_json(self.audio_video_timestamp_delta_ms()),
        )
    }

    fn observe(&mut self, timestamp: MediaTimestampUs, clock: &MediaClock) {
        if self.first_media_timestamp_us.is_none() {
            self.first_media_timestamp_us = Some(timestamp.0);
            self.receiver_clock_anchor_us =
                Some(clock.now_us() + self.playout_delay_ms.saturating_mul(1000));
        }
    }
}

pub fn optional_u64_json(value: Option<u64>) -> String {
    value.map_or_else(|| "null".to_string(), |value| value.to_string())
}

pub fn optional_f64_json(value: Option<f64>) -> String {
    value.map_or_else(|| "null".to_string(), |value| format!("{value:.3}"))
}

fn duration_to_us(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

pub fn run_self_test() -> Result<(), String> {
    let clock = MediaClock::new();
    let mut anchor = ReceiverMediaClockAnchor::new(120);
    anchor.observe_video(MediaTimestampUs(33_000), &clock);
    if anchor.first_media_timestamp_us != Some(33_000)
        || anchor.receiver_clock_anchor_us.unwrap_or_default() < 120_000
    {
        return Err("media clock receiver anchor failed".to_string());
    }
    anchor.observe_audio(MediaTimestampUs(43_000), &clock);
    if anchor.audio_video_timestamp_delta_ms() != Some(10.0) {
        return Err("media clock A/V delta failed".to_string());
    }
    let mut qpc = QpcMediaTimestampMapper {
        qpc_frequency: Some(10_000_000),
        ..QpcMediaTimestampMapper::default()
    };
    let (first, first_used_qpc) = qpc.map_capture_timestamp(100, 50_000);
    let (second, second_used_qpc) = qpc.map_capture_timestamp(100_100, 99_999);
    if !first_used_qpc || !second_used_qpc || first != 50_000 || second != 60_000 {
        return Err("QPC media timestamp mapping failed".to_string());
    }
    Ok(())
}
