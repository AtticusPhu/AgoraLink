use std::time::{Duration, Instant};

const CAPABILITY_MAGIC: &[u8; 4] = b"MCFB";
const PROFILE_CHANGE_MAGIC: &[u8; 4] = b"MPRF";
const PROFILE_ACK_MAGIC: &[u8; 4] = b"MPAK";
const STREAM_CLOSE_MAGIC: &[u8; 4] = b"MCLS";
const STREAM_CLOSE_ACK_MAGIC: &[u8; 4] = b"MCLA";
pub const MEDIA_CONTROL_VERSION: u8 = 1;
pub const CAPABILITY_FEEDBACK_VERSION: u8 = 2;
const CAPABILITY_FEEDBACK_V1_LEN: usize = 112;
pub const CAPABILITY_FEEDBACK_LEN: usize = 144;
pub const PROFILE_CHANGE_LEN: usize = 65;
pub const PROFILE_ACK_LEN: usize = 40;
pub const STREAM_CLOSE_LEN: usize = 40;
pub const STREAM_CLOSE_ACK_LEN: usize = 24;
pub const CAPABILITY_STALE_AFTER: Duration = Duration::from_secs(2);

pub const FEEDBACK_FLAG_SAMPLE_ELIGIBLE: u32 = 1 << 0;
pub const FEEDBACK_FLAG_RENDER_READY: u32 = 1 << 1;
pub const FEEDBACK_FLAG_PROFILE_SETTLED: u32 = 1 << 2;
pub const FEEDBACK_FLAG_FIRST_IDR_DECODED: u32 = 1 << 3;
pub const FEEDBACK_FLAG_FIRST_FRAME_RENDERED: u32 = 1 << 4;
pub const FEEDBACK_FLAG_PROFILE_TRANSITION_ACTIVE: u32 = 1 << 5;
pub const FEEDBACK_FLAG_PROFILE_ACKNOWLEDGED: u32 = 1 << 6;
const FEEDBACK_KNOWN_FLAGS: u32 = FEEDBACK_FLAG_SAMPLE_ELIGIBLE
    | FEEDBACK_FLAG_RENDER_READY
    | FEEDBACK_FLAG_PROFILE_SETTLED
    | FEEDBACK_FLAG_FIRST_IDR_DECODED
    | FEEDBACK_FLAG_FIRST_FRAME_RENDERED
    | FEEDBACK_FLAG_PROFILE_TRANSITION_ACTIVE
    | FEEDBACK_FLAG_PROFILE_ACKNOWLEDGED;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CapabilityFeedback {
    pub version: u8,
    pub session_id: u64,
    pub feedback_sequence: u64,
    pub display_generation: u64,
    pub display_refresh_numerator: u32,
    pub display_refresh_denominator: u32,
    pub display_width: u32,
    pub display_height: u32,
    pub present_fps_measured: f32,
    pub present_interval_p95_ms: f32,
    pub active_render_fps: f32,
    pub decoder_input_fps: f32,
    pub decode_queue_drops_delta: u64,
    pub render_replacements_delta: u64,
    pub repair_deadline_missed_delta: u64,
    pub damaged_gop_delta: u64,
    pub packets_lost_delta: u64,
    pub timestamp_us: u64,
    pub profile_generation: u64,
    pub state_flags: u32,
    pub valid_feedback_windows: u32,
    pub transition_settle_windows: u32,
    pub transition_settle_duration_ms: u32,
    pub profile_transition_started_us: u64,
}

impl CapabilityFeedback {
    pub fn encode(self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let output_len = if self.version == 1 {
            CAPABILITY_FEEDBACK_V1_LEN
        } else {
            CAPABILITY_FEEDBACK_LEN
        };
        let mut output = Vec::with_capacity(output_len);
        output.extend_from_slice(CAPABILITY_MAGIC);
        output.push(self.version);
        output.extend_from_slice(&[0, 0, 0]);
        push_u64(&mut output, self.session_id);
        push_u64(&mut output, self.feedback_sequence);
        push_u64(&mut output, self.display_generation);
        push_u32(&mut output, self.display_refresh_numerator);
        push_u32(&mut output, self.display_refresh_denominator);
        push_u32(&mut output, self.display_width);
        push_u32(&mut output, self.display_height);
        push_f32(&mut output, self.present_fps_measured);
        push_f32(&mut output, self.present_interval_p95_ms);
        push_f32(&mut output, self.active_render_fps);
        push_f32(&mut output, self.decoder_input_fps);
        push_u64(&mut output, self.decode_queue_drops_delta);
        push_u64(&mut output, self.render_replacements_delta);
        push_u64(&mut output, self.repair_deadline_missed_delta);
        push_u64(&mut output, self.damaged_gop_delta);
        push_u64(&mut output, self.packets_lost_delta);
        push_u64(&mut output, self.timestamp_us);
        if self.version >= CAPABILITY_FEEDBACK_VERSION {
            push_u64(&mut output, self.profile_generation);
            push_u32(&mut output, self.state_flags);
            push_u32(&mut output, self.valid_feedback_windows);
            push_u32(&mut output, self.transition_settle_windows);
            push_u32(&mut output, self.transition_settle_duration_ms);
            push_u64(&mut output, self.profile_transition_started_us);
        }
        debug_assert_eq!(output.len(), output_len);
        Ok(output)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < 5 || &bytes[..4] != CAPABILITY_MAGIC {
            return Err("not a MEDIA_CAPABILITY_FEEDBACK datagram".to_string());
        }
        let version = bytes[4];
        let expected_len = match version {
            1 => CAPABILITY_FEEDBACK_V1_LEN,
            CAPABILITY_FEEDBACK_VERSION => CAPABILITY_FEEDBACK_LEN,
            _ => {
                return Err(format!(
                    "unsupported capability feedback version: {version}"
                ));
            }
        };
        if bytes.len() != expected_len {
            return Err(format!(
                "invalid capability feedback length for version {version}: {}",
                bytes.len()
            ));
        }
        if bytes[5..8] != [0, 0, 0] {
            return Err("capability feedback reserved bytes are non-zero".to_string());
        }
        let decoded = Self {
            version,
            session_id: read_u64(bytes, 8),
            feedback_sequence: read_u64(bytes, 16),
            display_generation: read_u64(bytes, 24),
            display_refresh_numerator: read_u32(bytes, 32),
            display_refresh_denominator: read_u32(bytes, 36),
            display_width: read_u32(bytes, 40),
            display_height: read_u32(bytes, 44),
            present_fps_measured: read_f32(bytes, 48),
            present_interval_p95_ms: read_f32(bytes, 52),
            active_render_fps: read_f32(bytes, 56),
            decoder_input_fps: read_f32(bytes, 60),
            decode_queue_drops_delta: read_u64(bytes, 64),
            render_replacements_delta: read_u64(bytes, 72),
            repair_deadline_missed_delta: read_u64(bytes, 80),
            damaged_gop_delta: read_u64(bytes, 88),
            packets_lost_delta: read_u64(bytes, 96),
            timestamp_us: read_u64(bytes, 104),
            profile_generation: if version >= CAPABILITY_FEEDBACK_VERSION {
                read_u64(bytes, 112)
            } else {
                0
            },
            state_flags: if version >= CAPABILITY_FEEDBACK_VERSION {
                read_u32(bytes, 120)
            } else {
                0
            },
            valid_feedback_windows: if version >= CAPABILITY_FEEDBACK_VERSION {
                read_u32(bytes, 124)
            } else {
                0
            },
            transition_settle_windows: if version >= CAPABILITY_FEEDBACK_VERSION {
                read_u32(bytes, 128)
            } else {
                0
            },
            transition_settle_duration_ms: if version >= CAPABILITY_FEEDBACK_VERSION {
                read_u32(bytes, 132)
            } else {
                0
            },
            profile_transition_started_us: if version >= CAPABILITY_FEEDBACK_VERSION {
                read_u64(bytes, 136)
            } else {
                0
            },
        };
        decoded.validate()?;
        Ok(decoded)
    }

    pub fn validate(self) -> Result<(), String> {
        if self.version != 1 && self.version != CAPABILITY_FEEDBACK_VERSION {
            return Err(format!(
                "unsupported capability feedback version: {}",
                self.version
            ));
        }
        if self.session_id == 0 {
            return Err("capability feedback session_id must be non-zero".to_string());
        }
        if self.display_refresh_denominator == 0 {
            return Err("capability feedback refresh denominator is zero".to_string());
        }
        let refresh =
            f64::from(self.display_refresh_numerator) / f64::from(self.display_refresh_denominator);
        if self.display_refresh_numerator != 0 && !(10.0..=1000.0).contains(&refresh) {
            return Err(format!(
                "capability feedback refresh is invalid: {refresh:.3}Hz"
            ));
        }
        if self.display_width > 32_768 || self.display_height > 32_768 {
            return Err("capability feedback display dimensions are out of range".to_string());
        }
        validate_metric("present_fps_measured", self.present_fps_measured, 1000.0)?;
        validate_metric(
            "present_interval_p95_ms",
            self.present_interval_p95_ms,
            10_000.0,
        )?;
        validate_metric("active_render_fps", self.active_render_fps, 1000.0)?;
        validate_metric("decoder_input_fps", self.decoder_input_fps, 1000.0)?;
        if self.state_flags & !FEEDBACK_KNOWN_FLAGS != 0 {
            return Err("capability feedback contains unknown state flags".to_string());
        }
        Ok(())
    }

    pub const fn has_flag(self, flag: u32) -> bool {
        self.state_flags & flag != 0
    }

    pub const fn sample_eligible(self) -> bool {
        self.version >= CAPABILITY_FEEDBACK_VERSION && self.has_flag(FEEDBACK_FLAG_SAMPLE_ELIGIBLE)
    }

    pub const fn render_ready(self) -> bool {
        self.has_flag(FEEDBACK_FLAG_RENDER_READY)
    }

    pub const fn profile_settled(self) -> bool {
        self.has_flag(FEEDBACK_FLAG_PROFILE_SETTLED)
    }

    pub const fn profile_transition_active(self) -> bool {
        self.has_flag(FEEDBACK_FLAG_PROFILE_TRANSITION_ACTIVE)
    }

    pub const fn first_idr_decoded(self) -> bool {
        self.has_flag(FEEDBACK_FLAG_FIRST_IDR_DECODED)
    }

    pub const fn first_frame_rendered(self) -> bool {
        self.has_flag(FEEDBACK_FLAG_FIRST_FRAME_RENDERED)
    }

    pub const fn profile_acknowledged(self) -> bool {
        self.has_flag(FEEDBACK_FLAG_PROFILE_ACKNOWLEDGED)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProfileChange {
    pub version: u8,
    pub old_session_id: u64,
    pub new_session_id: u64,
    pub change_sequence: u64,
    pub profile_generation: u64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_mbps: f32,
    pub timestamp_us: u64,
    pub reason_code: u8,
}

impl ProfileChange {
    pub fn encode(self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let mut output = Vec::with_capacity(PROFILE_CHANGE_LEN);
        output.extend_from_slice(PROFILE_CHANGE_MAGIC);
        output.push(MEDIA_CONTROL_VERSION);
        output.extend_from_slice(&[0, 0, 0]);
        push_u64(&mut output, self.old_session_id);
        push_u64(&mut output, self.new_session_id);
        push_u64(&mut output, self.change_sequence);
        push_u64(&mut output, self.profile_generation);
        push_u32(&mut output, self.width);
        push_u32(&mut output, self.height);
        push_u32(&mut output, self.fps);
        push_f32(&mut output, self.bitrate_mbps);
        push_u64(&mut output, self.timestamp_us);
        output.push(self.reason_code);
        debug_assert_eq!(output.len(), PROFILE_CHANGE_LEN);
        Ok(output)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != PROFILE_CHANGE_LEN || &bytes[..4] != PROFILE_CHANGE_MAGIC {
            return Err("not a MEDIA_PROFILE_CHANGE datagram".to_string());
        }
        if bytes[4] != MEDIA_CONTROL_VERSION || bytes[5..8] != [0, 0, 0] {
            return Err("invalid MEDIA_PROFILE_CHANGE header".to_string());
        }
        let decoded = Self {
            version: bytes[4],
            old_session_id: read_u64(bytes, 8),
            new_session_id: read_u64(bytes, 16),
            change_sequence: read_u64(bytes, 24),
            profile_generation: read_u64(bytes, 32),
            width: read_u32(bytes, 40),
            height: read_u32(bytes, 44),
            fps: read_u32(bytes, 48),
            bitrate_mbps: read_f32(bytes, 52),
            timestamp_us: read_u64(bytes, 56),
            reason_code: bytes[64],
        };
        decoded.validate()?;
        Ok(decoded)
    }

    pub fn validate(self) -> Result<(), String> {
        if self.version != MEDIA_CONTROL_VERSION
            || self.old_session_id == 0
            || self.new_session_id == 0
            || self.old_session_id == self.new_session_id
            || self.change_sequence == 0
            || self.profile_generation == 0
        {
            return Err("invalid profile change version or session transition".to_string());
        }
        if self.width == 0
            || self.height == 0
            || self.width % 2 != 0
            || self.height % 2 != 0
            || self.width > 32_768
            || self.height > 32_768
        {
            return Err("invalid profile change dimensions".to_string());
        }
        if !(1..=240).contains(&self.fps)
            || !self.bitrate_mbps.is_finite()
            || !(0.1..=1000.0).contains(&self.bitrate_mbps)
        {
            return Err("invalid profile change FPS or bitrate".to_string());
        }
        if !(1..=3).contains(&self.reason_code) {
            return Err("invalid profile change reason code".to_string());
        }
        Ok(())
    }
}

pub const PROFILE_ACK_STATUS_ACCEPTED: u8 = 1;
pub const PROFILE_ACK_STATUS_REJECTED: u8 = 2;
pub const PROFILE_ACK_REASON_NONE: u8 = 0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProfileAck {
    pub version: u8,
    pub status: u8,
    pub reason_code: u8,
    pub old_session_id: u64,
    pub new_session_id: u64,
    pub change_sequence: u64,
    pub profile_generation: u64,
}

impl ProfileAck {
    pub const fn accepted(change: ProfileChange) -> Self {
        Self {
            version: MEDIA_CONTROL_VERSION,
            status: PROFILE_ACK_STATUS_ACCEPTED,
            reason_code: PROFILE_ACK_REASON_NONE,
            old_session_id: change.old_session_id,
            new_session_id: change.new_session_id,
            change_sequence: change.change_sequence,
            profile_generation: change.profile_generation,
        }
    }

    pub const fn is_accepted(self) -> bool {
        self.status == PROFILE_ACK_STATUS_ACCEPTED
    }

    pub fn encode(self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let mut output = Vec::with_capacity(PROFILE_ACK_LEN);
        output.extend_from_slice(PROFILE_ACK_MAGIC);
        output.push(self.version);
        output.push(self.status);
        output.push(self.reason_code);
        output.push(0);
        push_u64(&mut output, self.old_session_id);
        push_u64(&mut output, self.new_session_id);
        push_u64(&mut output, self.change_sequence);
        push_u64(&mut output, self.profile_generation);
        debug_assert_eq!(output.len(), PROFILE_ACK_LEN);
        Ok(output)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != PROFILE_ACK_LEN || &bytes[..4] != PROFILE_ACK_MAGIC {
            return Err("not a MEDIA_PROFILE_ACK datagram".to_string());
        }
        if bytes[4] != MEDIA_CONTROL_VERSION || bytes[7] != 0 {
            return Err("invalid MEDIA_PROFILE_ACK header".to_string());
        }
        let decoded = Self {
            version: bytes[4],
            status: bytes[5],
            reason_code: bytes[6],
            old_session_id: read_u64(bytes, 8),
            new_session_id: read_u64(bytes, 16),
            change_sequence: read_u64(bytes, 24),
            profile_generation: read_u64(bytes, 32),
        };
        decoded.validate()?;
        Ok(decoded)
    }

    pub fn validate(self) -> Result<(), String> {
        if self.version != MEDIA_CONTROL_VERSION
            || !matches!(
                self.status,
                PROFILE_ACK_STATUS_ACCEPTED | PROFILE_ACK_STATUS_REJECTED
            )
            || self.old_session_id == 0
            || self.new_session_id == 0
            || self.old_session_id == self.new_session_id
            || self.change_sequence == 0
            || self.profile_generation == 0
        {
            return Err("invalid profile ACK fields".to_string());
        }
        if self.status == PROFILE_ACK_STATUS_ACCEPTED && self.reason_code != PROFILE_ACK_REASON_NONE
        {
            return Err("accepted profile ACK must not contain a rejection reason".to_string());
        }
        Ok(())
    }
}

pub fn is_capability_feedback(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[..4] == CAPABILITY_MAGIC
}

pub fn is_profile_change(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[..4] == PROFILE_CHANGE_MAGIC
}

pub fn is_profile_ack(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[..4] == PROFILE_ACK_MAGIC
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamClose {
    pub version: u8,
    pub stream_id: u8,
    pub reason_code: u8,
    pub video_session_id: u64,
    pub close_id: u64,
    pub timestamp_us: u64,
    pub last_frame_id: u64,
}

impl StreamClose {
    pub fn encode(self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let mut output = Vec::with_capacity(STREAM_CLOSE_LEN);
        output.extend_from_slice(STREAM_CLOSE_MAGIC);
        output.push(self.version);
        output.push(self.stream_id);
        output.push(self.reason_code);
        output.push(0);
        push_u64(&mut output, self.video_session_id);
        push_u64(&mut output, self.close_id);
        push_u64(&mut output, self.timestamp_us);
        push_u64(&mut output, self.last_frame_id);
        debug_assert_eq!(output.len(), STREAM_CLOSE_LEN);
        Ok(output)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != STREAM_CLOSE_LEN || &bytes[..4] != STREAM_CLOSE_MAGIC {
            return Err("not a STREAM_CLOSE datagram".to_string());
        }
        if bytes[4] != MEDIA_CONTROL_VERSION || bytes[7] != 0 {
            return Err("invalid STREAM_CLOSE header".to_string());
        }
        let close = Self {
            version: bytes[4],
            stream_id: bytes[5],
            reason_code: bytes[6],
            video_session_id: read_u64(bytes, 8),
            close_id: read_u64(bytes, 16),
            timestamp_us: read_u64(bytes, 24),
            last_frame_id: read_u64(bytes, 32),
        };
        close.validate()?;
        Ok(close)
    }

    pub fn validate(self) -> Result<(), String> {
        if self.version != MEDIA_CONTROL_VERSION
            || self.stream_id == 0
            || self.reason_code == 0
            || self.close_id == 0
        {
            return Err("invalid STREAM_CLOSE fields".to_string());
        }
        Ok(())
    }

    pub const fn ack(self) -> StreamCloseAck {
        StreamCloseAck {
            version: MEDIA_CONTROL_VERSION,
            stream_id: self.stream_id,
            video_session_id: self.video_session_id,
            close_id: self.close_id,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamCloseAck {
    pub version: u8,
    pub stream_id: u8,
    pub video_session_id: u64,
    pub close_id: u64,
}

impl StreamCloseAck {
    pub fn encode(self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let mut output = Vec::with_capacity(STREAM_CLOSE_ACK_LEN);
        output.extend_from_slice(STREAM_CLOSE_ACK_MAGIC);
        output.push(self.version);
        output.push(self.stream_id);
        output.extend_from_slice(&[0, 0]);
        push_u64(&mut output, self.video_session_id);
        push_u64(&mut output, self.close_id);
        debug_assert_eq!(output.len(), STREAM_CLOSE_ACK_LEN);
        Ok(output)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != STREAM_CLOSE_ACK_LEN || &bytes[..4] != STREAM_CLOSE_ACK_MAGIC {
            return Err("not a STREAM_CLOSE_ACK datagram".to_string());
        }
        if bytes[4] != MEDIA_CONTROL_VERSION || bytes[6..8] != [0, 0] {
            return Err("invalid STREAM_CLOSE_ACK header".to_string());
        }
        let ack = Self {
            version: bytes[4],
            stream_id: bytes[5],
            video_session_id: read_u64(bytes, 8),
            close_id: read_u64(bytes, 16),
        };
        ack.validate()?;
        Ok(ack)
    }

    pub fn validate(self) -> Result<(), String> {
        if self.version != MEDIA_CONTROL_VERSION || self.stream_id == 0 || self.close_id == 0 {
            return Err("invalid STREAM_CLOSE_ACK fields".to_string());
        }
        Ok(())
    }

    pub const fn matches(self, close: StreamClose) -> bool {
        self.stream_id == close.stream_id
            && self.video_session_id == close.video_session_id
            && self.close_id == close.close_id
    }
}

pub fn is_stream_close(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[..4] == STREAM_CLOSE_MAGIC
}

pub fn is_stream_close_ack(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[..4] == STREAM_CLOSE_ACK_MAGIC
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FeedbackTrackerStats {
    pub received: u64,
    pub invalid: u64,
    pub stale_events: u64,
}

pub struct CapabilityFeedbackTracker {
    latest: Option<(CapabilityFeedback, Instant)>,
    last_any_valid_peer_control_at: Option<Instant>,
    last_any_valid_capability_feedback_at: Option<Instant>,
    last_profile_eligible_feedback_at: Option<Instant>,
    transition_grace_until: Option<Instant>,
    stats: FeedbackTrackerStats,
    stale_reported: bool,
}

impl CapabilityFeedbackTracker {
    pub fn new() -> Self {
        Self {
            latest: None,
            last_any_valid_peer_control_at: None,
            last_any_valid_capability_feedback_at: None,
            last_profile_eligible_feedback_at: None,
            transition_grace_until: None,
            stats: FeedbackTrackerStats::default(),
            stale_reported: false,
        }
    }

    pub fn observe(
        &mut self,
        bytes: &[u8],
        expected_session_id: u64,
        now: Instant,
    ) -> Result<bool, String> {
        let feedback = match CapabilityFeedback::decode(bytes) {
            Ok(feedback) => feedback,
            Err(err) => {
                self.stats.invalid = self.stats.invalid.saturating_add(1);
                return Err(err);
            }
        };
        self.last_any_valid_peer_control_at = Some(now);
        self.last_any_valid_capability_feedback_at = Some(now);
        if feedback.session_id != expected_session_id {
            self.stats.invalid = self.stats.invalid.saturating_add(1);
            return Err("capability feedback belongs to another session".to_string());
        }
        if self
            .latest
            .is_some_and(|(current, _)| feedback.feedback_sequence <= current.feedback_sequence)
        {
            self.stats.invalid = self.stats.invalid.saturating_add(1);
            return Err("capability feedback sequence is stale or duplicated".to_string());
        }
        self.latest = Some((feedback, now));
        self.last_profile_eligible_feedback_at = Some(now);
        self.stats.received = self.stats.received.saturating_add(1);
        self.stale_reported = false;
        Ok(true)
    }

    pub fn latest_fresh(&mut self, now: Instant) -> Option<CapabilityFeedback> {
        let Some((feedback, received_at)) = self.latest else {
            return None;
        };
        if now.saturating_duration_since(received_at)
            > crate::shutdown::ShutdownConfig::default().peer_stale_after
        {
            if !self.stale_reported {
                self.stats.stale_events = self.stats.stale_events.saturating_add(1);
                self.stale_reported = true;
            }
            None
        } else {
            Some(feedback)
        }
    }

    pub fn stats(&self) -> FeedbackTrackerStats {
        self.stats
    }

    pub fn last_valid_at(&self) -> Option<Instant> {
        self.last_profile_eligible_feedback_at
    }

    #[allow(dead_code)]
    pub fn valid_feedback_age(&self, now: Instant) -> Option<Duration> {
        self.last_valid_at()
            .map(|received_at| now.saturating_duration_since(received_at))
    }

    pub fn observe_valid_peer_control(&mut self, now: Instant) {
        self.last_any_valid_peer_control_at = Some(now);
    }

    pub fn peer_activity_age(&self, now: Instant) -> Option<Duration> {
        self.last_any_valid_peer_control_at
            .into_iter()
            .chain(self.last_any_valid_capability_feedback_at)
            .max()
            .map(|received_at| now.saturating_duration_since(received_at))
    }

    #[allow(dead_code)]
    pub fn profile_eligible_feedback_age(&self, now: Instant) -> Option<Duration> {
        self.last_profile_eligible_feedback_at
            .map(|received_at| now.saturating_duration_since(received_at))
    }

    pub fn begin_transition_grace(&mut self, deadline: Instant) {
        self.transition_grace_until = Some(deadline);
    }

    pub fn clear_transition_grace(&mut self) {
        self.transition_grace_until = None;
    }

    pub fn transition_grace_active(&self, now: Instant) -> bool {
        self.transition_grace_until
            .is_some_and(|deadline| now < deadline)
    }

    pub fn reset_for_session(&mut self) {
        self.latest = None;
        self.last_profile_eligible_feedback_at = None;
        self.stale_reported = false;
    }
}

impl Default for CapabilityFeedbackTracker {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_metric(name: &str, value: f32, maximum: f32) -> Result<(), String> {
    if value.is_finite() && (0.0..=maximum).contains(&value) {
        Ok(())
    } else {
        Err(format!("capability feedback {name} is out of range"))
    }
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn push_f32(output: &mut Vec<u8>, value: f32) {
    push_u32(output, value.to_bits());
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_be_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn read_f32(bytes: &[u8], offset: usize) -> f32 {
    f32::from_bits(read_u32(bytes, offset))
}

pub fn run_self_test() -> Result<(), String> {
    let feedback = CapabilityFeedback {
        version: CAPABILITY_FEEDBACK_VERSION,
        session_id: 7,
        feedback_sequence: 9,
        display_generation: 2,
        display_refresh_numerator: 60_000,
        display_refresh_denominator: 1001,
        display_width: 1920,
        display_height: 1080,
        present_fps_measured: 59.94,
        present_interval_p95_ms: 17.1,
        active_render_fps: 59.8,
        decoder_input_fps: 60.0,
        decode_queue_drops_delta: 1,
        render_replacements_delta: 2,
        repair_deadline_missed_delta: 3,
        damaged_gop_delta: 4,
        packets_lost_delta: 5,
        timestamp_us: 123_456,
        profile_generation: 2,
        state_flags: FEEDBACK_FLAG_SAMPLE_ELIGIBLE
            | FEEDBACK_FLAG_RENDER_READY
            | FEEDBACK_FLAG_PROFILE_SETTLED
            | FEEDBACK_FLAG_FIRST_IDR_DECODED
            | FEEDBACK_FLAG_FIRST_FRAME_RENDERED
            | FEEDBACK_FLAG_PROFILE_ACKNOWLEDGED,
        valid_feedback_windows: 3,
        transition_settle_windows: 3,
        transition_settle_duration_ms: 2_500,
        profile_transition_started_us: 120_000,
    };
    let encoded = feedback.encode()?;
    if encoded.len() != CAPABILITY_FEEDBACK_LEN || CapabilityFeedback::decode(&encoded)? != feedback
    {
        return Err("capability feedback encode/decode failed".to_string());
    }
    let profile = ProfileChange {
        version: MEDIA_CONTROL_VERSION,
        old_session_id: 7,
        new_session_id: 8,
        change_sequence: 1,
        profile_generation: 2,
        width: 1600,
        height: 900,
        fps: 60,
        bitrate_mbps: 32.0,
        timestamp_us: 234_567,
        reason_code: 2,
    };
    let encoded = profile.encode()?;
    if encoded.len() != PROFILE_CHANGE_LEN || ProfileChange::decode(&encoded)? != profile {
        return Err("profile change encode/decode failed".to_string());
    }
    let ack = ProfileAck::accepted(profile);
    let encoded = ack.encode()?;
    if encoded.len() != PROFILE_ACK_LEN || ProfileAck::decode(&encoded)? != ack {
        return Err("profile ACK encode/decode failed".to_string());
    }
    let close = StreamClose {
        version: MEDIA_CONTROL_VERSION,
        stream_id: 1,
        reason_code: 2,
        video_session_id: 42,
        close_id: 99,
        timestamp_us: 123_456,
        last_frame_id: 77,
    };
    let close_encoded = close.encode()?;
    if StreamClose::decode(&close_encoded)? != close
        || !StreamCloseAck::decode(&close.ack().encode()?)?.matches(close)
    {
        return Err("STREAM_CLOSE/ACK encode/decode failed".to_string());
    }
    let now = Instant::now();
    let mut tracker = CapabilityFeedbackTracker::new();
    tracker.observe(&feedback.encode()?, 7, now)?;
    if tracker.latest_fresh(now).is_none()
        || tracker
            .latest_fresh(now + CAPABILITY_STALE_AFTER + Duration::from_millis(1))
            .is_some()
    {
        return Err("capability feedback freshness tracking failed".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_and_foreign_feedback_is_rejected() {
        let now = Instant::now();
        let mut tracker = CapabilityFeedbackTracker::new();
        assert!(tracker.observe(b"bad", 1, now).is_err());
        let packet = CapabilityFeedback {
            version: CAPABILITY_FEEDBACK_VERSION,
            session_id: 2,
            feedback_sequence: 1,
            display_refresh_denominator: 1,
            ..CapabilityFeedback::default()
        };
        assert!(tracker.observe(&packet.encode().unwrap(), 1, now).is_err());
    }

    #[test]
    fn stale_sequence_and_invalid_refresh_are_rejected() {
        let now = Instant::now();
        let mut tracker = CapabilityFeedbackTracker::new();
        let packet = CapabilityFeedback {
            version: CAPABILITY_FEEDBACK_VERSION,
            session_id: 11,
            feedback_sequence: 7,
            display_refresh_numerator: 60_000,
            display_refresh_denominator: 1001,
            display_width: 1920,
            display_height: 1080,
            ..CapabilityFeedback::default()
        };
        let encoded = packet.encode().unwrap();
        assert!(tracker.observe(&encoded, 11, now).unwrap());
        assert!(tracker
            .observe(&encoded, 11, now + Duration::from_millis(1))
            .is_err());

        let invalid = CapabilityFeedback {
            feedback_sequence: 8,
            display_refresh_denominator: 0,
            ..packet
        };
        assert!(invalid.encode().is_err());
    }

    #[test]
    fn legacy_feedback_decodes_but_is_never_eligible() {
        let legacy = CapabilityFeedback {
            version: MEDIA_CONTROL_VERSION,
            session_id: 11,
            feedback_sequence: 8,
            display_refresh_numerator: 60_000,
            display_refresh_denominator: 1001,
            display_width: 1920,
            display_height: 1080,
            present_fps_measured: 60.0,
            active_render_fps: 60.0,
            decoder_input_fps: 60.0,
            ..CapabilityFeedback::default()
        };
        let encoded = legacy.encode().unwrap();
        assert_eq!(encoded.len(), CAPABILITY_FEEDBACK_V1_LEN);
        let decoded = CapabilityFeedback::decode(&encoded).unwrap();
        assert_eq!(decoded.version, MEDIA_CONTROL_VERSION);
        assert!(!decoded.sample_eligible());
        assert_eq!(decoded.profile_generation, 0);
        assert_eq!(decoded.state_flags, 0);
    }

    #[test]
    fn profile_ack_round_trip_and_reserved_validation() {
        let change = ProfileChange {
            version: MEDIA_CONTROL_VERSION,
            old_session_id: 10,
            new_session_id: 20,
            change_sequence: 3,
            profile_generation: 4,
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_mbps: 4.0,
            timestamp_us: 5,
            reason_code: 2,
        };
        let ack = ProfileAck::accepted(change);
        let bytes = ack.encode().unwrap();
        assert_eq!(ProfileAck::decode(&bytes).unwrap(), ack);
        let mut malformed = bytes;
        malformed[7] = 1;
        assert!(ProfileAck::decode(&malformed).is_err());
    }

    #[test]
    fn profile_change_requires_monotonic_fields_and_known_reason() {
        let valid = ProfileChange {
            version: MEDIA_CONTROL_VERSION,
            old_session_id: 10,
            new_session_id: 20,
            change_sequence: 1,
            profile_generation: 1,
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_mbps: 4.0,
            timestamp_us: 1,
            reason_code: 2,
        };
        assert!(valid.validate().is_ok());
        assert!(ProfileChange {
            change_sequence: 0,
            ..valid
        }
        .validate()
        .is_err());
        assert!(ProfileChange {
            profile_generation: 0,
            ..valid
        }
        .validate()
        .is_err());
        assert!(ProfileChange {
            reason_code: 9,
            ..valid
        }
        .validate()
        .is_err());
    }

    #[test]
    fn stream_close_ack_requires_exact_stream_session_and_close_id() {
        let close = StreamClose {
            version: MEDIA_CONTROL_VERSION,
            stream_id: crate::STREAM_VIDEO,
            reason_code: crate::shutdown::StopReason::LocalStop as u8,
            video_session_id: 17,
            close_id: 23,
            timestamp_us: 42,
            last_frame_id: 99,
        };
        assert!(close.ack().matches(close));
        assert!(!StreamCloseAck {
            close_id: 24,
            ..close.ack()
        }
        .matches(close));
        assert!(!StreamCloseAck {
            video_session_id: 18,
            ..close.ack()
        }
        .matches(close));
        assert!(!StreamCloseAck {
            stream_id: 2,
            ..close.ack()
        }
        .matches(close));
    }

    #[test]
    fn duplicate_stream_close_is_reackable_but_shutdown_reason_is_idempotent() {
        let close = StreamClose {
            version: MEDIA_CONTROL_VERSION,
            stream_id: crate::STREAM_VIDEO,
            reason_code: crate::shutdown::StopReason::CtrlC as u8,
            video_session_id: 17,
            close_id: 23,
            timestamp_us: 42,
            last_frame_id: 99,
        };
        let cancellation = crate::shutdown::CancellationToken::new();
        assert!(cancellation.cancel(crate::shutdown::StopReason::PeerClosed));
        assert!(!cancellation.cancel(crate::shutdown::StopReason::PeerClosed));
        assert!(close.ack().matches(close));
        assert!(close.ack().matches(close));
        assert_eq!(
            cancellation.reason(),
            Some(crate::shutdown::StopReason::PeerClosed)
        );
    }

    #[test]
    fn invalid_feedback_does_not_refresh_peer_activity() {
        let now = Instant::now();
        let mut tracker = CapabilityFeedbackTracker::new();
        let valid = CapabilityFeedback {
            version: CAPABILITY_FEEDBACK_VERSION,
            session_id: 9,
            feedback_sequence: 1,
            display_refresh_denominator: 1,
            ..CapabilityFeedback::default()
        };
        tracker.observe(&valid.encode().unwrap(), 9, now).unwrap();
        let valid_at = tracker.last_valid_at();
        let invalid = CapabilityFeedback {
            feedback_sequence: 2,
            session_id: 10,
            ..valid
        };
        assert!(tracker
            .observe(&invalid.encode().unwrap(), 9, now + Duration::from_secs(1))
            .is_err());
        assert_eq!(tracker.last_valid_at(), valid_at);
    }

    #[test]
    fn session_rollover_separates_peer_liveness_from_profile_eligibility() {
        let start = Instant::now();
        let mut tracker = CapabilityFeedbackTracker::new();
        let old_session = CapabilityFeedback {
            version: CAPABILITY_FEEDBACK_VERSION,
            session_id: 9,
            feedback_sequence: 1,
            display_refresh_denominator: 1,
            ..CapabilityFeedback::default()
        };
        tracker
            .observe(&old_session.encode().unwrap(), 9, start)
            .unwrap();
        tracker.reset_for_session();
        assert!(tracker.profile_eligible_feedback_age(start).is_none());

        let during_rollover = start + Duration::from_millis(25);
        let stale_profile = CapabilityFeedback {
            feedback_sequence: 2,
            ..old_session
        };
        assert!(tracker
            .observe(&stale_profile.encode().unwrap(), 10, during_rollover)
            .is_err());
        assert_eq!(
            tracker.peer_activity_age(during_rollover),
            Some(Duration::ZERO)
        );
        assert!(tracker
            .profile_eligible_feedback_age(during_rollover)
            .is_none());

        tracker.begin_transition_grace(during_rollover + Duration::from_secs(1));
        assert!(tracker.transition_grace_active(during_rollover));
        tracker.clear_transition_grace();
        assert!(!tracker.transition_grace_active(during_rollover));
    }
}
