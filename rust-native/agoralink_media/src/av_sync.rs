//! Pure, bounded A/V presentation policy.
//!
//! This module deliberately has no dependency on WASAPI, D3D11, UDP, or wall
//! clocks. Callers provide monotonic microsecond timestamps so the policy can
//! be regression-tested without hardware.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AvSyncMode {
    Off,
    Conservative,
}

impl AvSyncMode {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Conservative => "conservative",
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "off" => Ok(Self::Off),
            "conservative" => Ok(Self::Conservative),
            other => Err(format!("invalid av-sync mode: {other}")),
        }
    }
}

impl Default for AvSyncMode {
    fn default() -> Self {
        Self::Off
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AvSyncState {
    Disabled,
    WaitingForRenderer,
    WaitingForAudioAnchor,
    Active,
    TemporarilyBypassed,
}

impl AvSyncState {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::WaitingForRenderer => "waiting_for_renderer",
            Self::WaitingForAudioAnchor => "waiting_for_audio_anchor",
            Self::Active => "active",
            Self::TemporarilyBypassed => "temporarily_bypassed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AvSyncBypassReason {
    AudioDisabled,
    RendererNotReady,
    AudioMasterInvalid,
    AudioMasterStale,
    TimelineDiscontinuity,
    DevicePaddingInvalid,
    ClockJump,
    SessionMismatch,
    HoldDeadlineExceeded,
}

impl AvSyncBypassReason {
    pub const fn name(self) -> &'static str {
        match self {
            Self::AudioDisabled => "audio_disabled",
            Self::RendererNotReady => "renderer_not_ready",
            Self::AudioMasterInvalid => "audio_master_invalid",
            Self::AudioMasterStale => "audio_master_stale",
            Self::TimelineDiscontinuity => "timeline_discontinuity",
            Self::DevicePaddingInvalid => "device_padding_invalid",
            Self::ClockJump => "clock_jump",
            Self::SessionMismatch => "session_mismatch",
            Self::HoldDeadlineExceeded => "hold_deadline_exceeded",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AvSyncInput {
    pub now_us: u64,
    pub video_timestamp_us: u64,
    pub renderer_initialized: bool,
    pub audio_enabled: bool,
    pub audio_playhead_us: Option<u64>,
    pub audio_master_stable: bool,
    pub audio_master_stale: bool,
    pub timeline_discontinuity: bool,
    pub device_padding_valid: bool,
    pub clock_jump: bool,
    pub session_matched: bool,
    pub video_offset_us: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AvSyncDecision {
    RenderNow,
    Hold {
        until_us: u64,
        counts_as_held_frame: bool,
    },
    DropLateFrame,
    BypassAndRender(AvSyncBypassReason),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AvSyncTelemetry {
    pub state: AvSyncState,
    pub bypass_reason: Option<AvSyncBypassReason>,
    pub state_transitions: u64,
    pub forced_release_count: u64,
    pub hold_epoch_ms: u64,
    pub video_frames_actually_held: u64,
    pub video_frames_actually_dropped: u64,
}

impl Default for AvSyncState {
    fn default() -> Self {
        Self::Disabled
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AvSyncScheduler {
    mode: AvSyncMode,
    state: AvSyncState,
    bypass_reason: Option<AvSyncBypassReason>,
    hold_epoch_started_us: Option<u64>,
    hold_counted_for_epoch: bool,
    forced_bypass_until_us: Option<u64>,
    state_transitions: u64,
    forced_release_count: u64,
    video_frames_actually_held: u64,
    video_frames_actually_dropped: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MediaClockJumpDetector {
    last_wall_us: Option<u64>,
    last_media_us: Option<u64>,
}

impl MediaClockJumpDetector {
    pub const MAX_DRIFT_US: u64 = 200_000;

    pub fn observe(&mut self, wall_us: u64, media_us: u64) -> bool {
        let jumped = match (self.last_wall_us, self.last_media_us) {
            (Some(last_wall), Some(last_media)) => {
                let Some(wall_delta) = wall_us.checked_sub(last_wall) else {
                    self.last_wall_us = Some(wall_us);
                    self.last_media_us = Some(media_us);
                    return true;
                };
                let Some(media_delta) = media_us.checked_sub(last_media) else {
                    self.last_wall_us = Some(wall_us);
                    self.last_media_us = Some(media_us);
                    return true;
                };
                wall_delta.abs_diff(media_delta) > Self::MAX_DRIFT_US
            }
            _ => false,
        };
        self.last_wall_us = Some(wall_us);
        self.last_media_us = Some(media_us);
        jumped
    }

    pub fn reset(&mut self) {
        self.last_wall_us = None;
        self.last_media_us = None;
    }
}

impl AvSyncScheduler {
    pub const TOLERANCE_US: i128 = 40_000;
    pub const LATE_DROP_US: i128 = 120_000;
    pub const MAX_HOLD_US: u64 = 80_000;
    pub const FORCED_BYPASS_US: u64 = 500_000;

    pub fn new(mode: AvSyncMode) -> Self {
        Self {
            mode,
            state: if mode == AvSyncMode::Off {
                AvSyncState::Disabled
            } else {
                AvSyncState::WaitingForRenderer
            },
            bypass_reason: if mode == AvSyncMode::Off {
                Some(AvSyncBypassReason::AudioDisabled)
            } else {
                None
            },
            hold_epoch_started_us: None,
            hold_counted_for_epoch: false,
            forced_bypass_until_us: None,
            state_transitions: 0,
            forced_release_count: 0,
            video_frames_actually_held: 0,
            video_frames_actually_dropped: 0,
        }
    }

    pub fn decide(&mut self, input: AvSyncInput) -> AvSyncDecision {
        if self.mode == AvSyncMode::Off || !input.audio_enabled {
            return self.bypass(AvSyncState::Disabled, AvSyncBypassReason::AudioDisabled);
        }
        if !input.renderer_initialized {
            return self.bypass(
                AvSyncState::WaitingForRenderer,
                AvSyncBypassReason::RendererNotReady,
            );
        }
        if !input.session_matched {
            return self.bypass(
                AvSyncState::TemporarilyBypassed,
                AvSyncBypassReason::SessionMismatch,
            );
        }
        if input.timeline_discontinuity {
            return self.bypass(
                AvSyncState::TemporarilyBypassed,
                AvSyncBypassReason::TimelineDiscontinuity,
            );
        }
        if !input.device_padding_valid {
            return self.bypass(
                AvSyncState::TemporarilyBypassed,
                AvSyncBypassReason::DevicePaddingInvalid,
            );
        }
        if input.clock_jump {
            return self.bypass(
                AvSyncState::TemporarilyBypassed,
                AvSyncBypassReason::ClockJump,
            );
        }
        let Some(audio_playhead_us) = input.audio_playhead_us else {
            return self.bypass(
                AvSyncState::WaitingForAudioAnchor,
                if input.audio_master_stale {
                    AvSyncBypassReason::AudioMasterStale
                } else {
                    AvSyncBypassReason::AudioMasterInvalid
                },
            );
        };
        if !input.audio_master_stable {
            return self.bypass(
                AvSyncState::WaitingForAudioAnchor,
                AvSyncBypassReason::AudioMasterInvalid,
            );
        }

        if self
            .forced_bypass_until_us
            .is_some_and(|until_us| input.now_us < until_us)
        {
            return self.bypass(
                AvSyncState::TemporarilyBypassed,
                AvSyncBypassReason::HoldDeadlineExceeded,
            );
        }
        self.forced_bypass_until_us = None;

        self.set_state(AvSyncState::Active, None);
        let target_video_timestamp_us = audio_playhead_us.saturating_add(input.video_offset_us);
        let delta_us = input.video_timestamp_us as i128 - target_video_timestamp_us as i128;
        if delta_us > Self::TOLERANCE_US {
            let epoch_started_us = *self.hold_epoch_started_us.get_or_insert(input.now_us);
            let elapsed_us = input.now_us.saturating_sub(epoch_started_us);
            if elapsed_us < Self::MAX_HOLD_US {
                let counts_as_held_frame = !self.hold_counted_for_epoch;
                if counts_as_held_frame {
                    self.hold_counted_for_epoch = true;
                    self.video_frames_actually_held =
                        self.video_frames_actually_held.saturating_add(1);
                }
                return AvSyncDecision::Hold {
                    until_us: epoch_started_us.saturating_add(Self::MAX_HOLD_US),
                    counts_as_held_frame,
                };
            }
            self.forced_release_count = self.forced_release_count.saturating_add(1);
            self.forced_bypass_until_us = Some(input.now_us.saturating_add(Self::FORCED_BYPASS_US));
            self.clear_hold_epoch();
            self.set_state(
                AvSyncState::TemporarilyBypassed,
                Some(AvSyncBypassReason::HoldDeadlineExceeded),
            );
            return AvSyncDecision::BypassAndRender(AvSyncBypassReason::HoldDeadlineExceeded);
        }
        self.clear_hold_epoch();
        if delta_us < -Self::LATE_DROP_US {
            self.video_frames_actually_dropped =
                self.video_frames_actually_dropped.saturating_add(1);
            return AvSyncDecision::DropLateFrame;
        }
        AvSyncDecision::RenderNow
    }

    pub fn telemetry(&self, now_us: u64) -> AvSyncTelemetry {
        AvSyncTelemetry {
            state: self.state,
            bypass_reason: self.bypass_reason,
            state_transitions: self.state_transitions,
            forced_release_count: self.forced_release_count,
            hold_epoch_ms: self
                .hold_epoch_started_us
                .map(|started| now_us.saturating_sub(started) / 1000)
                .unwrap_or(0),
            video_frames_actually_held: self.video_frames_actually_held,
            video_frames_actually_dropped: self.video_frames_actually_dropped,
        }
    }

    fn bypass(&mut self, state: AvSyncState, reason: AvSyncBypassReason) -> AvSyncDecision {
        self.clear_hold_epoch();
        self.set_state(state, Some(reason));
        AvSyncDecision::BypassAndRender(reason)
    }

    fn set_state(&mut self, state: AvSyncState, reason: Option<AvSyncBypassReason>) {
        if self.state != state || self.bypass_reason != reason {
            self.state_transitions = self.state_transitions.saturating_add(1);
        }
        self.state = state;
        self.bypass_reason = reason;
    }

    fn clear_hold_epoch(&mut self) {
        self.hold_epoch_started_us = None;
        self.hold_counted_for_epoch = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active_input(now_us: u64, video_timestamp_us: u64) -> AvSyncInput {
        AvSyncInput {
            now_us,
            video_timestamp_us,
            renderer_initialized: true,
            audio_enabled: true,
            audio_playhead_us: Some(1_000_000),
            audio_master_stable: true,
            audio_master_stale: false,
            timeline_discontinuity: false,
            device_padding_valid: true,
            clock_jump: false,
            session_matched: true,
            video_offset_us: 0,
        }
    }

    #[test]
    fn audio_off_is_disabled_and_never_holds_or_drops() {
        let mut scheduler = AvSyncScheduler::new(AvSyncMode::Off);
        let decision = scheduler.decide(active_input(0, 5_000_000));
        assert_eq!(
            decision,
            AvSyncDecision::BypassAndRender(AvSyncBypassReason::AudioDisabled)
        );
        let telemetry = scheduler.telemetry(0);
        assert_eq!(telemetry.state, AvSyncState::Disabled);
        assert_eq!(telemetry.video_frames_actually_held, 0);
        assert_eq!(telemetry.video_frames_actually_dropped, 0);
    }

    #[test]
    fn latest_frame_replacement_cannot_restart_hold_epoch() {
        let mut scheduler = AvSyncScheduler::new(AvSyncMode::Conservative);
        for now_us in (0..80_000).step_by(16_667) {
            let decision = scheduler.decide(active_input(now_us, 1_200_000 + now_us));
            assert!(matches!(decision, AvSyncDecision::Hold { .. }));
        }
        let decision = scheduler.decide(active_input(83_335, 1_283_335));
        assert!(matches!(
            decision,
            AvSyncDecision::BypassAndRender(AvSyncBypassReason::HoldDeadlineExceeded)
        ));
        let telemetry = scheduler.telemetry(83_335);
        assert_eq!(telemetry.forced_release_count, 1);
        assert_eq!(telemetry.video_frames_actually_held, 1);
    }

    #[test]
    fn renderer_or_master_unavailable_bypasses_without_drop() {
        let mut scheduler = AvSyncScheduler::new(AvSyncMode::Conservative);
        let mut input = active_input(0, 900_000);
        input.renderer_initialized = false;
        assert!(matches!(
            scheduler.decide(input),
            AvSyncDecision::BypassAndRender(AvSyncBypassReason::RendererNotReady)
        ));
        input.renderer_initialized = true;
        input.audio_playhead_us = None;
        input.audio_master_stale = true;
        assert!(matches!(
            scheduler.decide(input),
            AvSyncDecision::BypassAndRender(AvSyncBypassReason::AudioMasterStale)
        ));
        assert_eq!(scheduler.telemetry(0).video_frames_actually_dropped, 0);
    }

    #[test]
    fn unsafe_timeline_inputs_bypass_instead_of_blocking_video() {
        let cases: [(fn(&mut AvSyncInput), AvSyncBypassReason); 4] = [
            (
                |input: &mut AvSyncInput| input.session_matched = false,
                AvSyncBypassReason::SessionMismatch,
            ),
            (
                |input: &mut AvSyncInput| input.timeline_discontinuity = true,
                AvSyncBypassReason::TimelineDiscontinuity,
            ),
            (
                |input: &mut AvSyncInput| input.device_padding_valid = false,
                AvSyncBypassReason::DevicePaddingInvalid,
            ),
            (
                |input: &mut AvSyncInput| input.clock_jump = true,
                AvSyncBypassReason::ClockJump,
            ),
        ];
        for (mutate, reason) in cases {
            let mut scheduler = AvSyncScheduler::new(AvSyncMode::Conservative);
            let mut input = active_input(0, 1_000_000);
            mutate(&mut input);
            assert_eq!(
                scheduler.decide(input),
                AvSyncDecision::BypassAndRender(reason)
            );
            assert_eq!(scheduler.telemetry(0).video_frames_actually_dropped, 0);
        }
    }

    #[test]
    fn media_clock_jump_detector_rejects_large_drift_and_can_reset() {
        let mut detector = MediaClockJumpDetector::default();
        assert!(!detector.observe(0, 1_000_000));
        assert!(!detector.observe(100_000, 1_105_000));
        assert!(detector.observe(200_000, 1_600_000));
        detector.reset();
        assert!(!detector.observe(900_000, 50_000));
    }

    #[test]
    fn late_frame_is_the_only_drop_path() {
        let mut scheduler = AvSyncScheduler::new(AvSyncMode::Conservative);
        assert_eq!(
            scheduler.decide(active_input(0, 879_000)),
            AvSyncDecision::DropLateFrame
        );
        assert_eq!(scheduler.telemetry(0).video_frames_actually_dropped, 1);
    }

    #[test]
    fn deterministic_soak_makes_progress_after_jitter_and_discontinuities() {
        let mut scheduler = AvSyncScheduler::new(AvSyncMode::Conservative);
        let mut renders = 0u64;
        let mut holds = 0u64;
        for frame_index in 0..36_000u64 {
            let now_us = frame_index * 16_667;
            let jitter_us = match frame_index % 4 {
                0 => 20_000,
                1 => 30_000,
                2 => 40_000,
                _ => 80_000,
            };
            let mut input = active_input(now_us, 1_000_000 + now_us + jitter_us);
            let in_audio_gap = (12_000..12_018).contains(&frame_index);
            input.audio_playhead_us = (!in_audio_gap).then_some(1_000_000 + now_us);
            input.audio_master_stale = in_audio_gap;
            input.audio_master_stable = !in_audio_gap;
            input.timeline_discontinuity = matches!(frame_index, 9_000 | 24_000);
            if input.timeline_discontinuity {
                input.audio_playhead_us = None;
                input.audio_master_stable = false;
            }
            match scheduler.decide(input) {
                AvSyncDecision::Hold { .. } => holds += 1,
                AvSyncDecision::DropLateFrame => {}
                AvSyncDecision::RenderNow | AvSyncDecision::BypassAndRender(_) => renders += 1,
            }
        }
        assert!(renders > 1_000, "scheduler stopped making render progress");
        assert!(
            holds < 10_000,
            "scheduler held an unbounded number of frames"
        );
    }
}
