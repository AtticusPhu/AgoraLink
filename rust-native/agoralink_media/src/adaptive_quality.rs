use std::collections::VecDeque;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdaptiveMode {
    Off,
    Smoothness,
}

impl AdaptiveMode {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Smoothness => "smoothness",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "smoothness" => Ok(Self::Smoothness),
            _ => Err("adaptive-quality must be off or smoothness".to_string()),
        }
    }
}

impl Default for AdaptiveMode {
    fn default() -> Self {
        Self::Off
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdaptiveState {
    Disabled,
    Startup,
    Stable,
    MildPressure,
    SeverePressure,
    ProfileTransition,
    EmergencyFpsReduction,
    Recovering,
    Cooldown,
}

impl AdaptiveState {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Startup => "startup",
            Self::Stable => "stable",
            Self::MildPressure => "mild-pressure",
            Self::SeverePressure => "severe-pressure",
            Self::ProfileTransition => "profile-transition",
            Self::EmergencyFpsReduction => "emergency-fps-reduction",
            Self::Recovering => "recovering",
            Self::Cooldown => "cooldown",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Bottleneck {
    NetworkPressure,
    SenderOverload,
    ReceiverOverload,
    MixedPressure,
    Unknown,
    Stable,
}

impl Bottleneck {
    pub const fn name(self) -> &'static str {
        match self {
            Self::NetworkPressure => "network-pressure",
            Self::SenderOverload => "sender-overload",
            Self::ReceiverOverload => "receiver-overload",
            Self::MixedPressure => "mixed-pressure",
            Self::Unknown => "unknown",
            Self::Stable => "stable",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PressureSeverity {
    None,
    Mild,
    Severe,
}

impl PressureSeverity {
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Mild => "mild",
            Self::Severe => "severe",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PressureBreakdown {
    pub network: PressureSeverity,
    pub decoder: PressureSeverity,
    pub render: PressureSeverity,
    pub transition: PressureSeverity,
    pub network_reason: &'static str,
    pub decoder_reason: &'static str,
    pub render_reason: &'static str,
    pub transition_reason: &'static str,
}

impl Default for PressureBreakdown {
    fn default() -> Self {
        Self {
            network: PressureSeverity::None,
            decoder: PressureSeverity::None,
            render: PressureSeverity::None,
            transition: PressureSeverity::None,
            network_reason: "none",
            decoder_reason: "none",
            render_reason: "none",
            transition_reason: "none",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QualityProfile {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_mbps: f64,
}

impl QualityProfile {
    pub fn bpf(self) -> f64 {
        let pixels = f64::from(self.width) * f64::from(self.height) * f64::from(self.fps);
        if pixels <= 0.0 {
            0.0
        } else {
            self.bitrate_mbps * 1_000_000.0 / pixels
        }
    }

    pub fn profile_name(self) -> &'static str {
        if self.width >= 1920 && self.height >= 1080 {
            if self.bitrate_mbps > 42.0 {
                "F0"
            } else {
                "F1"
            }
        } else if self.width >= 1600 && self.height >= 900 {
            "F2"
        } else if self.fps >= 60 {
            "F3"
        } else if self.fps >= 45 {
            "F4"
        } else {
            "F5"
        }
    }

    pub fn bitrate_bounds(self) -> (f64, f64) {
        match self.profile_name() {
            "F0" => (42.0, 50.0),
            "F1" => (34.0, 42.0),
            "F2" => (28.0, 36.0),
            "F3" => (20.0, 28.0),
            "F4" => (14.0, 22.0),
            _ => (8.0, 14.0),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AdaptiveAction {
    SetBitrate {
        bitrate_mbps: f64,
        reason: String,
    },
    SetResolution {
        width: u32,
        height: u32,
        reason: String,
    },
    SetFps {
        fps: u32,
        reason: String,
    },
}

impl AdaptiveAction {
    pub const fn dimension(&self) -> &'static str {
        match self {
            Self::SetBitrate { .. } => "bitrate",
            Self::SetResolution { .. } => "resolution",
            Self::SetFps { .. } => "fps",
        }
    }

    pub fn reason(&self) -> &str {
        match self {
            Self::SetBitrate { reason, .. }
            | Self::SetResolution { reason, .. }
            | Self::SetFps { reason, .. } => reason,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AdaptiveConfig {
    pub mode: AdaptiveMode,
    pub min_width: u32,
    pub min_height: u32,
    pub min_fps: u32,
    pub max_bitrate_mbps: Option<f64>,
    pub upgrade_stable_sec: u64,
    pub resolution_cooldown_sec: u64,
    pub fps_cooldown_sec: u64,
    pub interactive_lag_guard: bool,
    pub startup_warmup_sec: u64,
    pub bitrate_cooldown_sec: u64,
    pub min_valid_windows: u64,
    pub mild_pressure_windows: u64,
    pub severe_pressure_windows: u64,
}

impl Default for AdaptiveConfig {
    fn default() -> Self {
        Self {
            mode: AdaptiveMode::Off,
            min_width: 1280,
            min_height: 720,
            min_fps: 30,
            max_bitrate_mbps: None,
            upgrade_stable_sec: 15,
            resolution_cooldown_sec: 30,
            fps_cooldown_sec: 20,
            interactive_lag_guard: true,
            startup_warmup_sec: 5,
            bitrate_cooldown_sec: 10,
            min_valid_windows: 5,
            mild_pressure_windows: 5,
            severe_pressure_windows: 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AdaptiveIntervalSample {
    pub data_bytes: u64,
    pub repair_bytes: u64,
    pub packets_lost: u64,
    pub nack_items: u64,
    pub repair_packets: u64,
    pub late_repairs: u64,
    pub duplicate_repairs: u64,
    pub complete_frame_ratio: f64,
    pub decoded_frame_ratio: f64,
    pub rendered_frame_ratio: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AdaptiveWindowMetrics {
    pub data_bytes_interval: u64,
    pub repair_bytes_interval: u64,
    pub repair_overhead_ratio_1s: f64,
    pub repair_overhead_ratio_5s: f64,
    pub packet_loss_delta_1s: u64,
    pub packet_loss_delta_5s: u64,
    pub nack_items_delta_1s: u64,
    pub repair_packets_delta_1s: u64,
    pub late_repair_delta_1s: u64,
    pub duplicate_repair_delta_1s: u64,
    pub complete_frame_ratio_1s: f64,
    pub decoded_frame_ratio_1s: f64,
    pub rendered_frame_ratio_1s: f64,
    pub valid_windows: u64,
    pub window_ready: bool,
}

impl AdaptiveWindowMetrics {
    pub fn json_fragment(self) -> String {
        format!(
            concat!(
                r#""adaptive_counter_scope":"window","data_bytes_interval":{},"repair_bytes_interval":{},"#,
                r#""repair_overhead_ratio_1s":{:.6},"repair_overhead_ratio_5s":{:.6},"#,
                r#""packet_loss_delta_1s":{},"packet_loss_delta_5s":{},"nack_items_delta_1s":{},"#,
                r#""repair_packets_delta_1s":{},"late_repair_delta_1s":{},"duplicate_repair_delta_1s":{},"#,
                r#""complete_frame_ratio_1s":{:.6},"decoded_frame_ratio_1s":{:.6},"#,
                r#""rendered_frame_ratio_1s":{:.6},"adaptive_valid_windows":{},"transport_window_ready":{}"#
            ),
            self.data_bytes_interval,
            self.repair_bytes_interval,
            self.repair_overhead_ratio_1s,
            self.repair_overhead_ratio_5s,
            self.packet_loss_delta_1s,
            self.packet_loss_delta_5s,
            self.nack_items_delta_1s,
            self.repair_packets_delta_1s,
            self.late_repair_delta_1s,
            self.duplicate_repair_delta_1s,
            self.complete_frame_ratio_1s,
            self.decoded_frame_ratio_1s,
            self.rendered_frame_ratio_1s,
            self.valid_windows,
            self.window_ready,
        )
    }
}

#[derive(Clone, Copy, Debug)]
struct TimedAdaptiveSample {
    at: Duration,
    sample: AdaptiveIntervalSample,
}

#[derive(Debug, Default)]
pub struct AdaptiveWindowTracker {
    session_id: Option<u64>,
    samples: VecDeque<TimedAdaptiveSample>,
}

impl AdaptiveWindowTracker {
    pub fn reset(&mut self, session_id: u64) {
        self.session_id = Some(session_id);
        self.samples.clear();
    }

    pub fn observe(
        &mut self,
        session_id: u64,
        now: Duration,
        sample: AdaptiveIntervalSample,
    ) -> AdaptiveWindowMetrics {
        if self.session_id != Some(session_id) {
            self.reset(session_id);
        }
        self.samples
            .push_back(TimedAdaptiveSample { at: now, sample });
        while self
            .samples
            .front()
            .is_some_and(|entry| now.saturating_sub(entry.at) > Duration::from_secs(5))
        {
            self.samples.pop_front();
        }
        let latest = self.samples.back().copied().unwrap();
        let cutoff = now.saturating_sub(Duration::from_secs(5));
        let five_second = self
            .samples
            .iter()
            .filter(|entry| entry.at >= cutoff)
            .map(|entry| entry.sample)
            .fold(AdaptiveIntervalSample::default(), |mut total, item| {
                total.data_bytes = total.data_bytes.saturating_add(item.data_bytes);
                total.repair_bytes = total.repair_bytes.saturating_add(item.repair_bytes);
                total.packets_lost = total.packets_lost.saturating_add(item.packets_lost);
                total.nack_items = total.nack_items.saturating_add(item.nack_items);
                total.repair_packets = total.repair_packets.saturating_add(item.repair_packets);
                total.late_repairs = total.late_repairs.saturating_add(item.late_repairs);
                total.duplicate_repairs = total
                    .duplicate_repairs
                    .saturating_add(item.duplicate_repairs);
                total
            });
        let valid_windows = self.samples.len() as u64;
        let span_ready = self
            .samples
            .front()
            .is_some_and(|first| now.saturating_sub(first.at) >= Duration::from_secs(4));
        AdaptiveWindowMetrics {
            data_bytes_interval: latest.sample.data_bytes,
            repair_bytes_interval: latest.sample.repair_bytes,
            repair_overhead_ratio_1s: byte_ratio(
                latest.sample.repair_bytes,
                latest.sample.data_bytes,
            ),
            repair_overhead_ratio_5s: byte_ratio(five_second.repair_bytes, five_second.data_bytes),
            packet_loss_delta_1s: latest.sample.packets_lost,
            packet_loss_delta_5s: five_second.packets_lost,
            nack_items_delta_1s: latest.sample.nack_items,
            repair_packets_delta_1s: latest.sample.repair_packets,
            late_repair_delta_1s: latest.sample.late_repairs,
            duplicate_repair_delta_1s: latest.sample.duplicate_repairs,
            complete_frame_ratio_1s: latest.sample.complete_frame_ratio,
            decoded_frame_ratio_1s: latest.sample.decoded_frame_ratio,
            rendered_frame_ratio_1s: latest.sample.rendered_frame_ratio,
            valid_windows,
            window_ready: valid_windows >= 5 && span_ready,
        }
    }
}

fn byte_ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AdaptiveSnapshot {
    pub target_fps: u32,
    pub actual_sender_fps: f64,
    pub capture_actual_fps: f64,
    pub encoder_actual_fps: f64,
    pub encode_lag_skips_delta: u64,
    pub capture_dropped_delta: u64,
    pub video_worker_loop_ms_p99: f64,
    pub packetize_send_ms_p95: f64,
    pub packetize_send_ms_p99: f64,
    pub pacing_late_us_p95: f64,
    pub pacing_late_us_p99: f64,
    pub send_syscall_ms_p95: f64,
    pub actual_mbps: f64,
    pub target_mbps: f64,
    pub repair_packets_resent_delta: u64,
    pub send_errors_delta: u64,
    pub receiver_active_fps: f64,
    pub receiver_decoder_input_fps: f64,
    pub decode_queue_drops_delta: u64,
    pub render_replacements_delta: u64,
    pub repair_deadline_missed_delta: u64,
    pub damaged_gop_delta: u64,
    pub packets_lost_delta: u64,
    pub present_fps_measured: f64,
    pub present_interval_p95_ms: f64,
    pub feedback_fresh: bool,
    pub feedback_sample_eligible: bool,
    pub profile_transition_active: bool,
    pub audio_queue_dropping: bool,
}

impl AdaptiveSnapshot {
    pub fn frame_budget_ms(self) -> f64 {
        if self.target_fps == 0 {
            1000.0 / 60.0
        } else {
            1000.0 / f64::from(self.target_fps)
        }
    }

    pub fn sender_ratio(self) -> f64 {
        ratio(self.actual_sender_fps, self.target_fps)
    }

    pub fn capture_ratio(self) -> f64 {
        ratio(self.capture_actual_fps, self.target_fps)
    }

    pub fn encoder_ratio(self) -> f64 {
        ratio(self.encoder_actual_fps, self.target_fps)
    }

    pub fn receiver_ratio(self) -> f64 {
        ratio(self.receiver_active_fps, self.target_fps)
    }

    fn mild(self) -> bool {
        let budget = self.frame_budget_ms();
        let throughput_shortfall = self.target_mbps > 0.0
            && self.actual_mbps < self.target_mbps * 0.90
            && (self.sender_ratio() < 0.98
                || self.pacing_late_us_p95 / 1000.0 > budget * 0.25
                || self.send_syscall_ms_p95 > budget * 0.20
                || self.repair_packets_resent_delta >= 4);
        let pressure = classify_pressure(self);
        self.sender_ratio() < 0.95
            || self.packetize_send_ms_p95 > budget * 0.5
            || self.pacing_late_us_p95 / 1000.0 > budget * 0.5
            || pressure.network == PressureSeverity::Mild
            || pressure.decoder == PressureSeverity::Mild
            || pressure.render == PressureSeverity::Mild
            || throughput_shortfall
    }

    fn severe(self) -> bool {
        let budget = self.frame_budget_ms();
        let pressure = classify_pressure(self);
        self.sender_ratio() < 0.85
            || self.packetize_send_ms_p99 > budget
            || self.pacing_late_us_p99 / 1000.0 > budget
            || self.video_worker_loop_ms_p99 > budget * 2.0
            || pressure.network == PressureSeverity::Severe
            || pressure.decoder == PressureSeverity::Severe
            || pressure.render == PressureSeverity::Severe
    }

    fn stable(self) -> bool {
        self.feedback_sample_eligible
            && self.repair_deadline_missed_delta == 0
            && self.damaged_gop_delta == 0
            && self.sender_ratio() >= 0.97
            && self.receiver_ratio() >= 0.95
            && ratio(self.receiver_decoder_input_fps, self.target_fps) >= 0.95
            && self.send_errors_delta == 0
            && self.decode_queue_drops_delta == 0
            && !self.audio_queue_dropping
            && self.packetize_send_ms_p95 <= self.frame_budget_ms() * 0.5
            && classify_pressure(self).network == PressureSeverity::None
            && classify_pressure(self).decoder == PressureSeverity::None
            && classify_pressure(self).render == PressureSeverity::None
            && self.pacing_late_us_p95 / 1000.0 <= self.frame_budget_ms() * 0.5
    }
}

#[derive(Clone, Debug)]
pub struct AdaptiveTelemetry {
    pub enabled: bool,
    pub mode: AdaptiveMode,
    pub state: AdaptiveState,
    pub bottleneck: Bottleneck,
    pub current: QualityProfile,
    pub nominal: QualityProfile,
    pub profile_generation: u64,
    pub reason: String,
    pub profile_changes: u64,
    pub bitrate_changes: u64,
    pub resolution_changes: u64,
    pub fps_changes: u64,
    pub last_change_age_ms: u64,
    pub cooldown_remaining_ms: u64,
    pub stable_window_count: u64,
    pub mild_window_count: u64,
    pub severe_window_count: u64,
    pub emergency_fps_entries: u64,
    pub interactive_lag_guard_active: bool,
    pub interactive_lag_guard_entries: u64,
    pub interactive_lag_guard_reason: Option<String>,
    pub last_change_dimension: Option<String>,
    pub bitrate_reduction_reason: Option<String>,
    pub resolution_reduction_reason: Option<String>,
    pub fps_reduction_reason: Option<String>,
    pub action_blocked_transition: u64,
    pub action_blocked_invalid_feedback: u64,
    pub action_blocked_cooldown: u64,
    pub last_eligible_feedback_age_ms: u64,
    pub warmup_remaining_ms: u64,
    pub pressure_streak: u64,
    pub window_ready: bool,
    pub adaptation_suppressed_reason: Option<String>,
    pub pressure: PressureBreakdown,
}

impl AdaptiveTelemetry {
    pub fn json_fragment(&self) -> String {
        let (floor, ceiling) = self.current.bitrate_bounds();
        format!(
            concat!(
                r#""adaptive_enabled":{},"adaptive_mode":"{}","adaptive_state":"{}","#,
                r#""adaptive_bottleneck":"{}","adaptive_profile":"{}","#,
                r#""adaptive_profile_generation":{},"adaptive_reason":"{}","#,
                r#""adaptive_profile_changes":{},"adaptive_bitrate_changes":{},"#,
                r#""adaptive_resolution_changes":{},"adaptive_fps_changes":{},"#,
                r#""adaptive_last_change_age_ms":{},"adaptive_cooldown_remaining_ms":{},"#,
                r#""adaptive_stable_window_count":{},"adaptive_mild_window_count":{},"#,
                r#""adaptive_severe_window_count":{},"adaptive_emergency_fps_entries":{},"#,
                r#""adaptive_target_width":{},"adaptive_target_height":{},"#,
                r#""adaptive_target_fps":{},"adaptive_target_bitrate_mbps":{:.3},"#,
                r#""adaptive_nominal_width":{},"adaptive_nominal_height":{},"#,
                r#""adaptive_nominal_fps":{},"adaptive_nominal_bitrate_mbps":{:.3},"#,
                r#""adaptive_bitrate_floor_mbps":{:.3},"adaptive_bitrate_ceiling_mbps":{:.3},"#,
                r#""adaptive_bpf":{:.6},"adaptive_bpf_min":{:.6},"adaptive_bpf_max":{:.6},"#,
                r#""interactive_lag_guard_active":{},"interactive_lag_guard_entries":{},"#,
                r#""interactive_lag_guard_reason":{},"adaptive_last_change_dimension":{},"#,
                r#""adaptive_bitrate_reduction_reason":{},"#,
                r#""adaptive_resolution_reduction_reason":{},"adaptive_fps_reduction_reason":{},"#,
                r#""adaptive_action_blocked_transition":{},"#,
                r#""adaptive_action_blocked_invalid_feedback":{},"#,
                r#""adaptive_action_blocked_cooldown":{},"#,
                r#""adaptive_last_eligible_feedback_age_ms":{},"adaptive_warmup_remaining_ms":{},"#,
                r#""adaptive_pressure_streak":{},"window_ready":{},"adaptation_suppressed_reason":{},"#,
                r#""network_pressure_active":{},"network_pressure_severity":"{}","network_pressure_reason":"{}","#,
                r#""decoder_pressure_active":{},"decoder_pressure_severity":"{}","decoder_pressure_reason":"{}","#,
                r#""render_pressure_active":{},"render_pressure_severity":"{}","render_pressure_reason":"{}","#,
                r#""transition_pressure_active":{},"transition_pressure_severity":"{}","transition_pressure_reason":"{}""#
            ),
            self.enabled,
            self.mode.name(),
            self.state.name(),
            self.bottleneck.name(),
            self.current.profile_name(),
            self.profile_generation,
            json_escape(&self.reason),
            self.profile_changes,
            self.bitrate_changes,
            self.resolution_changes,
            self.fps_changes,
            self.last_change_age_ms,
            self.cooldown_remaining_ms,
            self.stable_window_count,
            self.mild_window_count,
            self.severe_window_count,
            self.emergency_fps_entries,
            self.current.width,
            self.current.height,
            self.current.fps,
            self.current.bitrate_mbps,
            self.nominal.width,
            self.nominal.height,
            self.nominal.fps,
            self.nominal.bitrate_mbps,
            floor,
            ceiling,
            self.current.bpf(),
            bpf_for(
                self.current.width,
                self.current.height,
                self.current.fps,
                floor
            ),
            bpf_for(
                self.current.width,
                self.current.height,
                self.current.fps,
                ceiling
            ),
            self.interactive_lag_guard_active,
            self.interactive_lag_guard_entries,
            optional_json_string(self.interactive_lag_guard_reason.as_deref()),
            optional_json_string(self.last_change_dimension.as_deref()),
            optional_json_string(self.bitrate_reduction_reason.as_deref()),
            optional_json_string(self.resolution_reduction_reason.as_deref()),
            optional_json_string(self.fps_reduction_reason.as_deref()),
            self.action_blocked_transition,
            self.action_blocked_invalid_feedback,
            self.action_blocked_cooldown,
            self.last_eligible_feedback_age_ms,
            self.warmup_remaining_ms,
            self.pressure_streak,
            self.window_ready,
            optional_json_string(self.adaptation_suppressed_reason.as_deref()),
            self.pressure.network != PressureSeverity::None,
            self.pressure.network.name(),
            self.pressure.network_reason,
            self.pressure.decoder != PressureSeverity::None,
            self.pressure.decoder.name(),
            self.pressure.decoder_reason,
            self.pressure.render != PressureSeverity::None,
            self.pressure.render.name(),
            self.pressure.render_reason,
            self.pressure.transition != PressureSeverity::None,
            self.pressure.transition.name(),
            self.pressure.transition_reason,
        )
    }
}

pub struct AdaptiveQualityController {
    config: AdaptiveConfig,
    current: QualityProfile,
    nominal: QualityProfile,
    state: AdaptiveState,
    bottleneck: Bottleneck,
    started_at: Duration,
    last_change_at: Option<Duration>,
    last_bitrate_change_at: Option<Duration>,
    last_resolution_change_at: Option<Duration>,
    last_fps_change_at: Option<Duration>,
    stable_windows: u64,
    mild_windows: u64,
    severe_windows: u64,
    profile_generation: u64,
    profile_changes: u64,
    bitrate_changes: u64,
    resolution_changes: u64,
    fps_changes: u64,
    emergency_fps_entries: u64,
    interactive_lag_guard_entries: u64,
    interactive_lag_guard_active: bool,
    interactive_lag_guard_reason: Option<String>,
    last_change_dimension: Option<String>,
    bitrate_reduction_reason: Option<String>,
    resolution_reduction_reason: Option<String>,
    fps_reduction_reason: Option<String>,
    action_blocked_transition: u64,
    action_blocked_invalid_feedback: u64,
    action_blocked_cooldown: u64,
    last_eligible_feedback_at: Option<Duration>,
    reason: String,
    valid_windows_since_reset: u64,
    window_ready: bool,
    adaptation_suppressed_reason: Option<String>,
    pressure: PressureBreakdown,
}

impl AdaptiveQualityController {
    pub fn new(config: AdaptiveConfig, initial: QualityProfile, now: Duration) -> Self {
        let state = if config.mode == AdaptiveMode::Off {
            AdaptiveState::Disabled
        } else {
            AdaptiveState::Startup
        };
        Self {
            config,
            current: initial,
            nominal: initial,
            state,
            bottleneck: Bottleneck::Unknown,
            started_at: now,
            last_change_at: None,
            last_bitrate_change_at: None,
            last_resolution_change_at: None,
            last_fps_change_at: None,
            stable_windows: 0,
            mild_windows: 0,
            severe_windows: 0,
            profile_generation: 0,
            profile_changes: 0,
            bitrate_changes: 0,
            resolution_changes: 0,
            fps_changes: 0,
            emergency_fps_entries: 0,
            interactive_lag_guard_entries: 0,
            interactive_lag_guard_active: false,
            interactive_lag_guard_reason: None,
            last_change_dimension: None,
            bitrate_reduction_reason: None,
            resolution_reduction_reason: None,
            fps_reduction_reason: None,
            action_blocked_transition: 0,
            action_blocked_invalid_feedback: 0,
            action_blocked_cooldown: 0,
            last_eligible_feedback_at: None,
            reason: "initializing".to_string(),
            valid_windows_since_reset: 0,
            window_ready: false,
            adaptation_suppressed_reason: Some("startup-warmup".to_string()),
            pressure: PressureBreakdown::default(),
        }
    }

    pub fn current(&self) -> QualityProfile {
        self.current
    }

    pub fn set_nominal_fps(&mut self, fps: u32) {
        self.nominal.fps = fps.max(self.config.min_fps);
    }

    pub fn enforce_fps_cap(&mut self, now: Duration, fps: u32) -> Option<AdaptiveAction> {
        let fps = fps.max(self.config.min_fps);
        self.nominal.fps = fps;
        if self.config.mode == AdaptiveMode::Off || self.current.fps <= fps {
            return None;
        }
        self.state = AdaptiveState::Cooldown;
        self.apply_action(
            now,
            AdaptiveAction::SetFps {
                fps,
                reason: "display-or-user-fps-cap".to_string(),
            },
        )
    }

    pub fn begin_profile_transition(&mut self) {
        if self.config.mode == AdaptiveMode::Off {
            return;
        }
        self.state = AdaptiveState::ProfileTransition;
        self.bottleneck = Bottleneck::Unknown;
        self.reset_pressure_windows();
        self.valid_windows_since_reset = 0;
        self.window_ready = false;
        self.adaptation_suppressed_reason = Some("profile-transition".to_string());
        self.reason = "profile-transition-feedback-isolated".to_string();
    }

    pub fn finish_profile_transition(&mut self, now: Duration) {
        if self.config.mode == AdaptiveMode::Off {
            return;
        }
        self.state = AdaptiveState::Startup;
        self.started_at = now;
        self.reset_pressure_windows();
        self.valid_windows_since_reset = 0;
        self.window_ready = false;
        self.adaptation_suppressed_reason = Some("profile-baseline-warmup".to_string());
        self.reason = "profile-transition-settled".to_string();
    }

    pub fn observe_ineligible_feedback(&mut self, transition_active: bool, reason: &str) {
        if self.config.mode == AdaptiveMode::Off {
            self.state = AdaptiveState::Disabled;
            return;
        }
        self.bottleneck = Bottleneck::Unknown;
        self.reset_pressure_windows();
        self.valid_windows_since_reset = 0;
        self.window_ready = false;
        self.adaptation_suppressed_reason = Some(reason.to_string());
        if transition_active {
            self.state = AdaptiveState::ProfileTransition;
            self.action_blocked_transition = self.action_blocked_transition.saturating_add(1);
        } else {
            self.state = AdaptiveState::Startup;
            self.action_blocked_invalid_feedback =
                self.action_blocked_invalid_feedback.saturating_add(1);
        }
        self.reason = reason.to_string();
    }

    pub fn observe(&mut self, now: Duration, snapshot: AdaptiveSnapshot) -> Option<AdaptiveAction> {
        self.observe_windowed(now, snapshot, true)
    }

    pub fn observe_windowed(
        &mut self,
        now: Duration,
        snapshot: AdaptiveSnapshot,
        transport_window_ready: bool,
    ) -> Option<AdaptiveAction> {
        if self.config.mode == AdaptiveMode::Off {
            self.state = AdaptiveState::Disabled;
            self.reason = "adaptive-quality-off".to_string();
            return None;
        }
        if snapshot.profile_transition_active {
            self.pressure = classify_pressure(snapshot);
            self.observe_ineligible_feedback(true, "profile-transition-feedback-ignored");
            return None;
        }
        if !snapshot.feedback_fresh || !snapshot.feedback_sample_eligible {
            self.observe_ineligible_feedback(false, "feedback-sample-ineligible");
            return None;
        }
        self.last_eligible_feedback_at = Some(now);
        self.valid_windows_since_reset = self.valid_windows_since_reset.saturating_add(1);
        self.window_ready = transport_window_ready
            && self.valid_windows_since_reset >= self.config.min_valid_windows;
        let startup_elapsed = now.saturating_sub(self.started_at);
        if startup_elapsed < Duration::from_secs(self.config.startup_warmup_sec) {
            self.state = AdaptiveState::Startup;
            self.bottleneck = Bottleneck::Unknown;
            self.reset_pressure_windows();
            self.reason = "startup-protection".to_string();
            self.adaptation_suppressed_reason = Some("startup-warmup".to_string());
            return None;
        }
        if !self.window_ready {
            self.state = AdaptiveState::Startup;
            self.reset_pressure_windows();
            self.reason = "collecting-valid-window-baseline".to_string();
            self.adaptation_suppressed_reason = Some("window-not-ready".to_string());
            return None;
        }
        self.adaptation_suppressed_reason = None;
        self.pressure = classify_pressure(snapshot);
        self.bottleneck = classify_bottleneck(snapshot);
        self.update_lag_guard(snapshot);
        let severe = snapshot.severe();
        let mild = !severe && snapshot.mild();
        let stable = snapshot.stable();
        if severe {
            self.severe_windows = self.severe_windows.saturating_add(1);
            self.mild_windows = 0;
            self.stable_windows = 0;
            self.state = AdaptiveState::SeverePressure;
        } else if mild {
            self.mild_windows = self.mild_windows.saturating_add(1);
            self.severe_windows = 0;
            self.stable_windows = 0;
            self.state = AdaptiveState::MildPressure;
        } else if stable {
            self.stable_windows = self.stable_windows.saturating_add(1);
            self.mild_windows = 0;
            self.severe_windows = 0;
            self.state = AdaptiveState::Stable;
        } else {
            self.stable_windows = 0;
            self.mild_windows = 0;
            self.severe_windows = 0;
            self.state = AdaptiveState::Stable;
        }

        if severe && self.severe_windows >= self.config.severe_pressure_windows {
            return self.degrade(now, true);
        }
        if mild && self.mild_windows >= self.config.mild_pressure_windows {
            return self.degrade(now, false);
        }
        if stable && self.stable_windows >= self.config.upgrade_stable_sec.max(1) {
            return self.recover(now);
        }
        self.reason = match self.state {
            AdaptiveState::MildPressure => "waiting-for-mild-pressure-hysteresis",
            AdaptiveState::SeverePressure => "waiting-for-severe-pressure-hysteresis",
            AdaptiveState::Stable => "collecting-stability-window",
            _ => "no-action",
        }
        .to_string();
        None
    }

    pub fn telemetry(&self, now: Duration) -> AdaptiveTelemetry {
        let cooldown_remaining = self.cooldown_remaining(now);
        AdaptiveTelemetry {
            enabled: self.config.mode != AdaptiveMode::Off,
            mode: self.config.mode,
            state: self.state,
            bottleneck: self.bottleneck,
            current: self.current,
            nominal: self.nominal,
            profile_generation: self.profile_generation,
            reason: self.reason.clone(),
            profile_changes: self.profile_changes,
            bitrate_changes: self.bitrate_changes,
            resolution_changes: self.resolution_changes,
            fps_changes: self.fps_changes,
            last_change_age_ms: self
                .last_change_at
                .map(|then| duration_ms(now.saturating_sub(then)))
                .unwrap_or(0),
            cooldown_remaining_ms: duration_ms(cooldown_remaining),
            stable_window_count: self.stable_windows,
            mild_window_count: self.mild_windows,
            severe_window_count: self.severe_windows,
            emergency_fps_entries: self.emergency_fps_entries,
            interactive_lag_guard_active: self.interactive_lag_guard_active,
            interactive_lag_guard_entries: self.interactive_lag_guard_entries,
            interactive_lag_guard_reason: self.interactive_lag_guard_reason.clone(),
            last_change_dimension: self.last_change_dimension.clone(),
            bitrate_reduction_reason: self.bitrate_reduction_reason.clone(),
            resolution_reduction_reason: self.resolution_reduction_reason.clone(),
            fps_reduction_reason: self.fps_reduction_reason.clone(),
            action_blocked_transition: self.action_blocked_transition,
            action_blocked_invalid_feedback: self.action_blocked_invalid_feedback,
            action_blocked_cooldown: self.action_blocked_cooldown,
            last_eligible_feedback_age_ms: self
                .last_eligible_feedback_at
                .map(|then| duration_ms(now.saturating_sub(then)))
                .unwrap_or(0),
            warmup_remaining_ms: duration_ms(
                Duration::from_secs(self.config.startup_warmup_sec)
                    .saturating_sub(now.saturating_sub(self.started_at)),
            ),
            pressure_streak: self.severe_windows.max(self.mild_windows),
            window_ready: self.window_ready,
            adaptation_suppressed_reason: self.adaptation_suppressed_reason.clone(),
            pressure: self.pressure,
        }
    }

    fn degrade(&mut self, now: Duration, severe: bool) -> Option<AdaptiveAction> {
        if self.pressure.render != PressureSeverity::None
            && self.pressure.network == PressureSeverity::None
            && self.current.fps > self.config.min_fps
        {
            if cooldown_elapsed(
                self.last_fps_change_at,
                now,
                Duration::from_secs(self.config.fps_cooldown_sec),
            ) {
                return self.apply_action(
                    now,
                    AdaptiveAction::SetFps {
                        fps: next_lower_fps(self.current.fps, self.config.min_fps),
                        reason: "render-pressure-fps-first".to_string(),
                    },
                );
            }
            self.state = AdaptiveState::Cooldown;
            self.adaptation_suppressed_reason = Some("render-fps-cooldown".to_string());
            return None;
        }
        if self.pressure.decoder != PressureSeverity::None
            && self.pressure.network == PressureSeverity::None
            && (self.current.width > self.config.min_width
                || self.current.height > self.config.min_height)
        {
            if cooldown_elapsed(
                self.last_resolution_change_at,
                now,
                Duration::from_secs(self.config.resolution_cooldown_sec),
            ) {
                let (width, height) = next_lower_resolution(
                    self.current.width,
                    self.current.height,
                    self.config.min_width,
                    self.config.min_height,
                );
                return self.apply_action(
                    now,
                    AdaptiveAction::SetResolution {
                        width,
                        height,
                        reason: "decoder-pressure-resolution-first".to_string(),
                    },
                );
            }
            self.state = AdaptiveState::Cooldown;
            self.adaptation_suppressed_reason = Some("decoder-resolution-cooldown".to_string());
            return None;
        }
        let (floor, _) = self.current.bitrate_bounds();
        if self.current.bitrate_mbps > floor + 0.05 {
            if cooldown_elapsed(
                self.last_bitrate_change_at,
                now,
                Duration::from_secs(self.config.bitrate_cooldown_sec),
            ) {
                let factor = if severe { 0.88 } else { 0.92 };
                let bitrate = (self.current.bitrate_mbps * factor).max(floor);
                return self.apply_action(
                    now,
                    AdaptiveAction::SetBitrate {
                        bitrate_mbps: bitrate,
                        reason: format!("{}-bitrate-first", self.bottleneck.name()),
                    },
                );
            }
            self.state = AdaptiveState::Cooldown;
            self.action_blocked_cooldown = self.action_blocked_cooldown.saturating_add(1);
            self.reason = "bitrate-cooldown-preserves-dimension-order".to_string();
            return None;
        }
        if self.current.width > self.config.min_width
            || self.current.height > self.config.min_height
        {
            if !cooldown_elapsed(
                self.last_resolution_change_at,
                now,
                Duration::from_secs(self.config.resolution_cooldown_sec),
            ) {
                self.state = AdaptiveState::Cooldown;
                self.action_blocked_cooldown = self.action_blocked_cooldown.saturating_add(1);
                self.reason = "resolution-cooldown-preserves-dimension-order".to_string();
                return None;
            }
            let (width, height) = next_lower_resolution(
                self.current.width,
                self.current.height,
                self.config.min_width,
                self.config.min_height,
            );
            if (width, height) != (self.current.width, self.current.height) {
                return self.apply_action(
                    now,
                    AdaptiveAction::SetResolution {
                        width,
                        height,
                        reason: format!("{}-quality-floor-reached", self.bottleneck.name()),
                    },
                );
            }
        }
        if severe
            && self.severe_windows >= 5
            && self.current.width <= self.config.min_width
            && self.current.height <= self.config.min_height
            && self.current.fps > self.config.min_fps
            && cooldown_elapsed(
                self.last_fps_change_at,
                now,
                Duration::from_secs(self.config.fps_cooldown_sec),
            )
        {
            let fps = next_lower_fps(self.current.fps, self.config.min_fps);
            self.state = AdaptiveState::EmergencyFpsReduction;
            self.emergency_fps_entries = self.emergency_fps_entries.saturating_add(1);
            return self.apply_action(
                now,
                AdaptiveAction::SetFps {
                    fps,
                    reason: format!("{}-sustained-emergency", self.bottleneck.name()),
                },
            );
        }
        self.state = AdaptiveState::Cooldown;
        self.action_blocked_cooldown = self.action_blocked_cooldown.saturating_add(1);
        self.reason = "degrade-action-blocked-by-floor-or-cooldown".to_string();
        None
    }

    fn recover(&mut self, now: Duration) -> Option<AdaptiveAction> {
        self.state = AdaptiveState::Recovering;
        if self.current.fps < self.nominal.fps {
            if !cooldown_elapsed(
                self.last_fps_change_at,
                now,
                Duration::from_secs(self.config.fps_cooldown_sec),
            ) {
                self.state = AdaptiveState::Cooldown;
                self.action_blocked_cooldown = self.action_blocked_cooldown.saturating_add(1);
                self.reason = "fps-recovery-cooldown-preserves-priority".to_string();
                return None;
            }
            return self.apply_action(
                now,
                AdaptiveAction::SetFps {
                    fps: next_higher_fps(self.current.fps, self.nominal.fps),
                    reason: "stable-recovery-fps-first".to_string(),
                },
            );
        }
        if self.current.width < self.nominal.width || self.current.height < self.nominal.height {
            if !cooldown_elapsed(
                self.last_resolution_change_at,
                now,
                Duration::from_secs(self.config.resolution_cooldown_sec),
            ) {
                self.state = AdaptiveState::Cooldown;
                self.action_blocked_cooldown = self.action_blocked_cooldown.saturating_add(1);
                self.reason = "resolution-recovery-cooldown-preserves-priority".to_string();
                return None;
            }
            let (width, height) = next_higher_resolution(
                self.current.width,
                self.current.height,
                self.nominal.width,
                self.nominal.height,
            );
            return self.apply_action(
                now,
                AdaptiveAction::SetResolution {
                    width,
                    height,
                    reason: "stable-recovery-resolution-second".to_string(),
                },
            );
        }
        let ceiling = self
            .config
            .max_bitrate_mbps
            .unwrap_or(self.nominal.bitrate_mbps)
            .min(self.nominal.bitrate_mbps);
        if self.current.bitrate_mbps + 0.05 < ceiling
            && cooldown_elapsed(
                self.last_bitrate_change_at,
                now,
                Duration::from_secs(self.config.bitrate_cooldown_sec),
            )
        {
            return self.apply_action(
                now,
                AdaptiveAction::SetBitrate {
                    bitrate_mbps: (self.current.bitrate_mbps * 1.06).min(ceiling),
                    reason: "stable-recovery-bitrate-last".to_string(),
                },
            );
        }
        self.reason = "nominal-quality-restored".to_string();
        None
    }

    fn apply_action(&mut self, now: Duration, action: AdaptiveAction) -> Option<AdaptiveAction> {
        let previous = self.current;
        match &action {
            AdaptiveAction::SetBitrate { bitrate_mbps, .. } => {
                self.current.bitrate_mbps = *bitrate_mbps;
                self.bitrate_changes = self.bitrate_changes.saturating_add(1);
                self.last_bitrate_change_at = Some(now);
                if *bitrate_mbps < previous.bitrate_mbps {
                    self.bitrate_reduction_reason = Some(action.reason().to_string());
                }
            }
            AdaptiveAction::SetResolution { width, height, .. } => {
                self.current.width = *width;
                self.current.height = *height;
                self.resolution_changes = self.resolution_changes.saturating_add(1);
                self.last_resolution_change_at = Some(now);
                if u64::from(*width) * u64::from(*height)
                    < u64::from(previous.width) * u64::from(previous.height)
                {
                    self.resolution_reduction_reason = Some(action.reason().to_string());
                }
            }
            AdaptiveAction::SetFps { fps, .. } => {
                self.current.fps = *fps;
                self.fps_changes = self.fps_changes.saturating_add(1);
                self.last_fps_change_at = Some(now);
                if *fps < previous.fps {
                    self.fps_reduction_reason = Some(action.reason().to_string());
                }
            }
        }
        if !matches!(action, AdaptiveAction::SetBitrate { .. }) {
            self.profile_changes = self.profile_changes.saturating_add(1);
            self.profile_generation = self.profile_generation.saturating_add(1);
        }
        self.reason = action.reason().to_string();
        self.last_change_dimension = Some(action.dimension().to_string());
        self.last_change_at = Some(now);
        self.valid_windows_since_reset = 0;
        self.window_ready = false;
        self.adaptation_suppressed_reason = Some("post-change-cooldown".to_string());
        self.stable_windows = 0;
        self.mild_windows = 0;
        if !matches!(action, AdaptiveAction::SetFps { .. }) {
            self.severe_windows = 0;
        }
        Some(action)
    }

    fn update_lag_guard(&mut self, snapshot: AdaptiveSnapshot) {
        let active = self.config.interactive_lag_guard
            && (snapshot.sender_ratio() < 0.85
                || snapshot.video_worker_loop_ms_p99 > snapshot.frame_budget_ms() * 2.0);
        if active && !self.interactive_lag_guard_active {
            self.interactive_lag_guard_entries =
                self.interactive_lag_guard_entries.saturating_add(1);
        }
        self.interactive_lag_guard_active = active;
        self.interactive_lag_guard_reason = active.then(|| {
            if snapshot.sender_ratio() < 0.85 {
                "sender-fps-below-85-percent".to_string()
            } else {
                "video-worker-p99-over-two-frame-budgets".to_string()
            }
        });
    }

    fn cooldown_remaining(&self, now: Duration) -> Duration {
        let bitrate = remaining(
            self.last_bitrate_change_at,
            now,
            Duration::from_secs(self.config.bitrate_cooldown_sec),
        );
        let resolution = remaining(
            self.last_resolution_change_at,
            now,
            Duration::from_secs(self.config.resolution_cooldown_sec),
        );
        let fps = remaining(
            self.last_fps_change_at,
            now,
            Duration::from_secs(self.config.fps_cooldown_sec),
        );
        bitrate.max(resolution).max(fps)
    }

    fn reset_pressure_windows(&mut self) {
        self.stable_windows = 0;
        self.mild_windows = 0;
        self.severe_windows = 0;
        self.interactive_lag_guard_active = false;
        self.interactive_lag_guard_reason = None;
    }
}

pub fn classify_bottleneck(snapshot: AdaptiveSnapshot) -> Bottleneck {
    if !snapshot.feedback_fresh
        || !snapshot.feedback_sample_eligible
        || snapshot.profile_transition_active
    {
        return Bottleneck::Unknown;
    }
    let sender = snapshot.sender_ratio() < 0.95
        || snapshot.capture_ratio() < 0.95
        || snapshot.encoder_ratio() < 0.95
        || snapshot.encode_lag_skips_delta > 0
        || snapshot.capture_dropped_delta > 0
        || snapshot.video_worker_loop_ms_p99 > snapshot.frame_budget_ms();
    let pressure = classify_pressure(snapshot);
    let network = pressure.network != PressureSeverity::None;
    let receiver =
        pressure.decoder != PressureSeverity::None || pressure.render != PressureSeverity::None;
    match (sender, network, receiver) {
        (false, false, false) if snapshot.stable() => Bottleneck::Stable,
        (false, true, false) => Bottleneck::NetworkPressure,
        (true, false, false) => Bottleneck::SenderOverload,
        (false, false, true) => Bottleneck::ReceiverOverload,
        (true, _, _) | (_, true, true) => Bottleneck::MixedPressure,
        _ => Bottleneck::Unknown,
    }
}

pub fn classify_pressure(snapshot: AdaptiveSnapshot) -> PressureBreakdown {
    if snapshot.profile_transition_active {
        return PressureBreakdown {
            transition: PressureSeverity::Severe,
            transition_reason: "profile-transition-active",
            ..PressureBreakdown::default()
        };
    }
    if !snapshot.feedback_fresh || !snapshot.feedback_sample_eligible {
        return PressureBreakdown {
            transition: PressureSeverity::Mild,
            transition_reason: "feedback-not-eligible",
            ..PressureBreakdown::default()
        };
    }
    let budget = snapshot.frame_budget_ms();
    let (network, network_reason) = if snapshot.repair_deadline_missed_delta > 0
        || snapshot.damaged_gop_delta > 0
        || snapshot.send_errors_delta > 0
    {
        (PressureSeverity::Severe, "loss-or-repair-deadline")
    } else if snapshot.packets_lost_delta > 0
        || snapshot.repair_packets_resent_delta >= 4
        || snapshot.pacing_late_us_p95 / 1000.0 > budget * 0.5
        || snapshot.send_syscall_ms_p95 > budget * 0.25
    {
        (PressureSeverity::Mild, "network-window-pressure")
    } else {
        (PressureSeverity::None, "none")
    };
    let decoder_ratio = ratio(snapshot.receiver_decoder_input_fps, snapshot.target_fps);
    let (decoder, decoder_reason) = if snapshot.decode_queue_drops_delta > 0
        || (snapshot.receiver_decoder_input_fps > 0.0 && decoder_ratio < 0.80)
    {
        (PressureSeverity::Severe, "decoder-throughput-or-queue")
    } else if snapshot.receiver_decoder_input_fps > 0.0 && decoder_ratio < 0.90 {
        (PressureSeverity::Mild, "decoder-throughput")
    } else {
        (PressureSeverity::None, "none")
    };
    let render_ratio = if snapshot.receiver_decoder_input_fps <= 0.0 {
        1.0
    } else {
        snapshot.receiver_active_fps / snapshot.receiver_decoder_input_fps
    };
    let present_ratio = ratio(snapshot.present_fps_measured, snapshot.target_fps);
    let p95_is_sustained = snapshot.present_interval_p95_ms > budget * 1.5
        && snapshot.present_fps_measured > 0.0
        && present_ratio < 0.90
        && snapshot.receiver_ratio() < 0.90
        && render_ratio < 0.90;
    let (render, render_reason) = if render_ratio < 0.75
        || (snapshot.render_replacements_delta > 0 && snapshot.receiver_ratio() < 0.80)
    {
        (PressureSeverity::Severe, "render-throughput-or-queue")
    } else if render_ratio < 0.90 || p95_is_sustained {
        (PressureSeverity::Mild, "render-throughput")
    } else {
        (PressureSeverity::None, "none")
    };
    PressureBreakdown {
        network,
        decoder,
        render,
        transition: PressureSeverity::None,
        network_reason,
        decoder_reason,
        render_reason,
        transition_reason: "none",
    }
}

fn next_lower_resolution(width: u32, height: u32, min_width: u32, min_height: u32) -> (u32, u32) {
    let candidate = if width > 1600 || height > 900 {
        (1600, 900)
    } else {
        (1280, 720)
    };
    (candidate.0.max(min_width), candidate.1.max(min_height))
}

fn next_higher_resolution(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    let candidate = if width < 1600 || height < 900 {
        (1600, 900)
    } else {
        (1920, 1080)
    };
    (candidate.0.min(max_width), candidate.1.min(max_height))
}

fn next_lower_fps(fps: u32, min_fps: u32) -> u32 {
    if fps > 45 {
        45.max(min_fps)
    } else {
        30.max(min_fps)
    }
}

fn next_higher_fps(fps: u32, nominal: u32) -> u32 {
    if fps < 45 {
        45.min(nominal)
    } else if fps < 60 {
        60.min(nominal)
    } else if fps < 75 {
        75.min(nominal)
    } else if fps < 90 {
        90.min(nominal)
    } else {
        120.min(nominal)
    }
}

fn ratio(actual: f64, target: u32) -> f64 {
    if target == 0 || !actual.is_finite() {
        0.0
    } else {
        actual / f64::from(target)
    }
}

fn bpf_for(width: u32, height: u32, fps: u32, bitrate: f64) -> f64 {
    QualityProfile {
        width,
        height,
        fps,
        bitrate_mbps: bitrate,
    }
    .bpf()
}

fn cooldown_elapsed(last: Option<Duration>, now: Duration, cooldown: Duration) -> bool {
    last.is_none_or(|last| now.saturating_sub(last) >= cooldown)
}

fn remaining(last: Option<Duration>, now: Duration, cooldown: Duration) -> Duration {
    last.map_or(Duration::ZERO, |last| {
        cooldown.saturating_sub(now.saturating_sub(last))
    })
}

fn duration_ms(value: Duration) -> u64 {
    value.as_millis().min(u128::from(u64::MAX)) as u64
}

fn json_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn optional_json_string(value: Option<&str>) -> String {
    value.map_or_else(
        || "null".to_string(),
        |value| format!(r#""{}""#, json_escape(value)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stable_snapshot() -> AdaptiveSnapshot {
        AdaptiveSnapshot {
            target_fps: 60,
            actual_sender_fps: 60.0,
            capture_actual_fps: 60.0,
            encoder_actual_fps: 60.0,
            encode_lag_skips_delta: 0,
            capture_dropped_delta: 0,
            video_worker_loop_ms_p99: 2.0,
            packetize_send_ms_p95: 2.0,
            packetize_send_ms_p99: 3.0,
            pacing_late_us_p95: 100.0,
            pacing_late_us_p99: 200.0,
            send_syscall_ms_p95: 0.1,
            actual_mbps: 50.0,
            target_mbps: 50.0,
            repair_packets_resent_delta: 0,
            send_errors_delta: 0,
            receiver_active_fps: 60.0,
            receiver_decoder_input_fps: 60.0,
            decode_queue_drops_delta: 0,
            render_replacements_delta: 0,
            repair_deadline_missed_delta: 0,
            damaged_gop_delta: 0,
            packets_lost_delta: 0,
            present_fps_measured: 60.0,
            present_interval_p95_ms: 17.0,
            feedback_fresh: true,
            feedback_sample_eligible: true,
            profile_transition_active: false,
            audio_queue_dropping: false,
        }
    }

    fn controller(initial: QualityProfile) -> AdaptiveQualityController {
        AdaptiveQualityController::new(
            AdaptiveConfig {
                mode: AdaptiveMode::Smoothness,
                ..AdaptiveConfig::default()
            },
            initial,
            Duration::ZERO,
        )
    }

    fn stable_for_current(controller: &AdaptiveQualityController) -> AdaptiveSnapshot {
        let mut snapshot = stable_snapshot();
        let fps = controller.current().fps;
        snapshot.target_fps = fps;
        snapshot.actual_sender_fps = f64::from(fps);
        snapshot.capture_actual_fps = f64::from(fps);
        snapshot.encoder_actual_fps = f64::from(fps);
        snapshot.receiver_active_fps = f64::from(fps);
        snapshot.receiver_decoder_input_fps = f64::from(fps);
        snapshot.present_fps_measured = f64::from(fps);
        snapshot.present_interval_p95_ms = 1000.0 / f64::from(fps) * 1.02;
        snapshot.actual_mbps = controller.current().bitrate_mbps;
        snapshot.target_mbps = controller.current().bitrate_mbps;
        snapshot
    }

    fn next_stable_action(
        controller: &mut AdaptiveQualityController,
        first_second: u64,
        last_second: u64,
    ) -> AdaptiveAction {
        (first_second..=last_second)
            .find_map(|second| {
                let snapshot = stable_for_current(controller);
                controller.observe(Duration::from_secs(second), snapshot)
            })
            .unwrap_or_else(|| {
                panic!("no recovery action between {first_second}s and {last_second}s")
            })
    }

    #[test]
    fn fps_priority_and_hysteresis() {
        let initial = QualityProfile {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_mbps: 50.0,
        };
        let mut controller = controller(initial);
        let mut mild = stable_snapshot();
        mild.actual_mbps = 40.0;
        mild.pacing_late_us_p95 = 9_000.0;
        for second in 1..9 {
            assert!(controller
                .observe(Duration::from_secs(second), mild)
                .is_none());
        }
        let action = controller.observe(Duration::from_secs(9), mild).unwrap();
        assert!(matches!(action, AdaptiveAction::SetBitrate { .. }));
        assert_eq!(controller.current().fps, 60);
        assert_eq!(
            (controller.current().width, controller.current().height),
            (1920, 1080)
        );
    }

    #[test]
    fn resolution_before_emergency_fps() {
        let mut controller = controller(QualityProfile {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_mbps: 34.0,
        });
        let mut severe = stable_snapshot();
        severe.damaged_gop_delta = 1;
        for second in 1..7 {
            assert!(controller
                .observe(Duration::from_secs(second), severe)
                .is_none());
        }
        let action = controller.observe(Duration::from_secs(7), severe).unwrap();
        assert!(matches!(action, AdaptiveAction::SetResolution { .. }));
        assert_eq!(controller.current().fps, 60);
    }

    #[test]
    fn emergency_fps_reduction_requires_sustained_severe_pressure_at_floor() {
        let mut controller = controller(QualityProfile {
            width: 1280,
            height: 720,
            fps: 60,
            bitrate_mbps: 20.0,
        });
        let mut severe = stable_snapshot();
        severe.damaged_gop_delta = 1;
        for second in 1..9 {
            assert!(controller
                .observe(Duration::from_secs(second), severe)
                .is_none());
            assert_eq!(controller.current().fps, 60);
        }
        let action = controller
            .observe(Duration::from_secs(9), severe)
            .expect("five sustained severe windows should permit emergency FPS reduction");
        assert!(matches!(action, AdaptiveAction::SetFps { fps: 45, .. }));
        assert_eq!(
            controller
                .telemetry(Duration::from_secs(9))
                .emergency_fps_entries,
            1
        );
    }

    #[test]
    fn recovery_order_is_fps_resolution_bitrate() {
        let mut controller = controller(QualityProfile {
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_mbps: 10.0,
        });
        controller.nominal = QualityProfile {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_mbps: 50.0,
        };
        let first = next_stable_action(&mut controller, 1, 19);
        assert!(matches!(first, AdaptiveAction::SetFps { fps: 45, .. }));

        let second = next_stable_action(&mut controller, 20, 39);
        assert!(matches!(second, AdaptiveAction::SetFps { fps: 60, .. }));

        let third = next_stable_action(&mut controller, 40, 58);
        assert!(matches!(
            third,
            AdaptiveAction::SetResolution {
                width: 1600,
                height: 900,
                ..
            }
        ));

        let fourth = next_stable_action(&mut controller, 59, 88);
        assert!(matches!(
            fourth,
            AdaptiveAction::SetResolution {
                width: 1920,
                height: 1080,
                ..
            }
        ));

        let fifth = next_stable_action(&mut controller, 89, 107);
        assert!(matches!(fifth, AdaptiveAction::SetBitrate { .. }));
        assert_eq!(
            controller
                .telemetry(Duration::from_secs(107))
                .profile_changes,
            4
        );
        assert_eq!(
            controller
                .telemetry(Duration::from_secs(107))
                .bitrate_changes,
            1
        );
    }

    #[test]
    fn spikes_stale_feedback_and_short_stability_do_not_change_profile() {
        let initial = QualityProfile {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_mbps: 50.0,
        };
        let mut adaptive = controller(initial);
        let mut spike = stable_snapshot();
        spike.video_worker_loop_ms_p99 = 100.0;
        assert!(adaptive.observe(Duration::from_secs(3), spike).is_none());

        for second in 4..18 {
            let mut missing_feedback = stable_snapshot();
            missing_feedback.feedback_fresh = false;
            assert!(adaptive
                .observe(Duration::from_secs(second), missing_feedback)
                .is_none());
        }
        assert_eq!(adaptive.current(), initial);

        for second in 18..48 {
            let mut static_scene_vbr = stable_snapshot();
            static_scene_vbr.actual_mbps = 5.0;
            assert!(adaptive
                .observe(Duration::from_secs(second), static_scene_vbr)
                .is_none());
        }
        assert_eq!(adaptive.current(), initial);

        let mut degraded = controller(QualityProfile {
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_mbps: 10.0,
        });
        degraded.nominal = initial;
        for second in 1..15 {
            let snapshot = stable_for_current(&degraded);
            assert!(degraded
                .observe(Duration::from_secs(second), snapshot)
                .is_none());
        }
        assert_eq!(degraded.current().fps, 30);
    }

    #[test]
    fn adaptive_off_never_changes_fixed_profile() {
        let initial = QualityProfile {
            width: 1600,
            height: 900,
            fps: 50,
            bitrate_mbps: 32.0,
        };
        let mut controller =
            AdaptiveQualityController::new(AdaptiveConfig::default(), initial, Duration::ZERO);
        let mut severe = stable_snapshot();
        severe.damaged_gop_delta = 1;
        severe.actual_sender_fps = 10.0;
        for second in 1..=600 {
            assert!(controller
                .observe(Duration::from_secs(second), severe)
                .is_none());
        }
        assert_eq!(controller.current(), initial);
        assert_eq!(
            controller.telemetry(Duration::from_secs(600)).state,
            AdaptiveState::Disabled
        );
    }

    #[test]
    fn bottleneck_classification_matrix() {
        let stable = stable_snapshot();
        assert_eq!(classify_bottleneck(stable), Bottleneck::Stable);
        let mut network = stable;
        network.packets_lost_delta = 1;
        assert_eq!(classify_bottleneck(network), Bottleneck::NetworkPressure);
        let mut sender = stable;
        sender.actual_sender_fps = 40.0;
        assert_eq!(classify_bottleneck(sender), Bottleneck::SenderOverload);
        let mut receiver = stable;
        receiver.receiver_active_fps = 40.0;
        assert_eq!(classify_bottleneck(receiver), Bottleneck::ReceiverOverload);
        let mut mixed = sender;
        mixed.packets_lost_delta = 1;
        assert_eq!(classify_bottleneck(mixed), Bottleneck::MixedPressure);
    }

    #[test]
    fn virtual_soak_does_not_oscillate() {
        let initial = QualityProfile {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_mbps: 50.0,
        };
        let mut controller = controller(initial);
        let mut actions = 0u64;
        for second in 1..=1800u64 {
            let mut snapshot = stable_snapshot();
            if second % 97 == 0 {
                snapshot.pacing_late_us_p99 = 20_000.0;
            }
            if second % 211 == 0 {
                snapshot.feedback_fresh = false;
            }
            if controller
                .observe(Duration::from_secs(second), snapshot)
                .is_some()
            {
                actions += 1;
            }
        }
        assert!(
            actions <= 2,
            "unexpected profile oscillation: {actions} actions"
        );
    }

    #[test]
    fn startup_zero_fps_is_ineligible_and_cannot_pollute_pressure_windows() {
        let initial = QualityProfile {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_mbps: 50.0,
        };
        let mut controller = controller(initial);
        let mut startup = stable_snapshot();
        startup.receiver_active_fps = 0.0;
        startup.receiver_decoder_input_fps = 0.0;
        startup.present_fps_measured = 0.0;
        startup.feedback_sample_eligible = false;
        for second in 1..=10 {
            assert!(controller
                .observe(Duration::from_secs(second), startup)
                .is_none());
        }
        let telemetry = controller.telemetry(Duration::from_secs(10));
        assert_eq!(controller.current(), initial);
        assert_eq!(telemetry.bottleneck, Bottleneck::Unknown);
        assert_eq!(telemetry.mild_window_count, 0);
        assert_eq!(telemetry.severe_window_count, 0);
        assert_eq!(telemetry.profile_changes, 0);
        assert_eq!(telemetry.bitrate_changes, 0);
        assert_eq!(telemetry.action_blocked_invalid_feedback, 10);
    }

    #[test]
    fn initial_decoder_warmup_then_sixty_fps_stays_at_f0_for_sixty_seconds() {
        let initial = QualityProfile {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_mbps: 50.0,
        };
        let mut controller = controller(initial);
        for second in 1..=60 {
            let mut snapshot = stable_snapshot();
            if second <= 2 {
                snapshot.receiver_active_fps = 0.0;
                snapshot.receiver_decoder_input_fps = 0.0;
                snapshot.feedback_sample_eligible = false;
            }
            assert!(controller
                .observe(Duration::from_secs(second), snapshot)
                .is_none());
        }
        let telemetry = controller.telemetry(Duration::from_secs(60));
        assert_eq!(controller.current(), initial);
        assert_eq!(telemetry.current.profile_name(), "F0");
        assert_eq!(telemetry.profile_changes, 0);
        assert_eq!(telemetry.bitrate_changes, 0);
        assert_eq!(telemetry.fps_changes, 0);
    }

    #[test]
    fn stable_network_at_fifty_eight_to_sixty_fps_keeps_f0() {
        let initial = QualityProfile {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_mbps: 50.0,
        };
        let mut controller = controller(initial);
        for second in 1..=60 {
            let fps = [58.0, 59.0, 60.0][(second as usize - 1) % 3];
            let mut snapshot = stable_snapshot();
            snapshot.actual_sender_fps = fps;
            snapshot.capture_actual_fps = fps;
            snapshot.encoder_actual_fps = fps;
            snapshot.receiver_active_fps = fps;
            snapshot.receiver_decoder_input_fps = fps;
            snapshot.present_fps_measured = fps;
            assert!(controller
                .observe(Duration::from_secs(second), snapshot)
                .is_none());
        }
        let telemetry = controller.telemetry(Duration::from_secs(60));
        assert_eq!(telemetry.current, initial);
        assert_eq!(telemetry.current.profile_name(), "F0");
        assert_eq!(telemetry.profile_changes, 0);
        assert_eq!(telemetry.bitrate_changes, 0);
        assert_eq!(telemetry.resolution_changes, 0);
        assert_eq!(telemetry.fps_changes, 0);
    }

    #[test]
    fn bitrate_only_action_does_not_increment_profile_generation() {
        let mut controller = controller(QualityProfile {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_mbps: 50.0,
        });
        let mut mild = stable_snapshot();
        mild.pacing_late_us_p95 = 9_000.0;
        mild.actual_mbps = 40.0;
        for second in 1..9 {
            assert!(controller
                .observe(Duration::from_secs(second), mild)
                .is_none());
        }
        assert!(matches!(
            controller.observe(Duration::from_secs(9), mild),
            Some(AdaptiveAction::SetBitrate { .. })
        ));
        let telemetry = controller.telemetry(Duration::from_secs(9));
        assert_eq!(telemetry.bitrate_changes, 1);
        assert_eq!(telemetry.profile_changes, 0);
        assert_eq!(telemetry.profile_generation, 0);
        assert_eq!(telemetry.resolution_changes, 0);
        assert_eq!(telemetry.fps_changes, 0);
        assert_eq!(
            (telemetry.current.width, telemetry.current.height),
            (1920, 1080)
        );
        assert_eq!(telemetry.current.fps, 60);
    }

    #[test]
    fn transition_feedback_never_enters_classifier_or_triggers_positive_feedback() {
        let initial = QualityProfile {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_mbps: 50.0,
        };
        let mut controller = controller(initial);
        controller.begin_profile_transition();
        for (second, fps) in [0.0, 5.0, 30.0].into_iter().enumerate() {
            let mut snapshot = stable_snapshot();
            snapshot.receiver_active_fps = fps;
            snapshot.receiver_decoder_input_fps = fps;
            snapshot.feedback_sample_eligible = false;
            snapshot.profile_transition_active = true;
            assert!(controller
                .observe(Duration::from_secs(second as u64 + 1), snapshot)
                .is_none());
        }
        let transition = controller.telemetry(Duration::from_secs(3));
        assert_eq!(transition.state, AdaptiveState::ProfileTransition);
        assert_eq!(transition.severe_window_count, 0);
        assert_eq!(transition.profile_changes, 0);
        assert_eq!(transition.action_blocked_transition, 3);

        controller.finish_profile_transition(Duration::from_secs(4));
        for second in 4..=60 {
            assert!(controller
                .observe(Duration::from_secs(second), stable_snapshot())
                .is_none());
        }
        assert_eq!(controller.current(), initial);
        assert_eq!(
            controller
                .telemetry(Duration::from_secs(60))
                .profile_changes,
            0
        );
    }

    fn force_network_degrade(
        controller: &mut AdaptiveQualityController,
        second: u64,
        severe_windows: u64,
    ) -> Option<AdaptiveAction> {
        controller.pressure = PressureBreakdown {
            network: PressureSeverity::Severe,
            network_reason: "r4-deterministic-network-pressure",
            ..PressureBreakdown::default()
        };
        controller.bottleneck = Bottleneck::NetworkPressure;
        controller.severe_windows = severe_windows;
        controller.degrade(Duration::from_secs(second), true)
    }

    fn force_stable_recovery(
        controller: &mut AdaptiveQualityController,
        second: u64,
    ) -> Option<AdaptiveAction> {
        controller.pressure = PressureBreakdown::default();
        controller.bottleneck = Bottleneck::Stable;
        controller.recover(Duration::from_secs(second))
    }

    fn assert_profile(
        actual: QualityProfile,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_mbps: f64,
    ) {
        assert_eq!((actual.width, actual.height), (width, height));
        assert_eq!(actual.fps, fps);
        assert!(
            (actual.bitrate_mbps - bitrate_mbps).abs() < 0.001,
            "expected {bitrate_mbps} Mbps, got {} Mbps",
            actual.bitrate_mbps
        );
    }

    #[test]
    fn r4_standard_ladder_degrades_adjacent_q0_through_q4() {
        let mut controller = controller(QualityProfile {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_mbps: 22.0,
        });

        for (second, expected) in [
            (40, (1600, 900, 60, 22.0)),
            (80, (1280, 720, 60, 22.0)),
            (120, (1280, 720, 60, 18.0)),
            (160, (1280, 720, 60, 15.0)),
        ] {
            force_network_degrade(&mut controller, second, 3)
                .expect("each adjacent R4 quality step should produce an action");
            assert_profile(
                controller.current(),
                expected.0,
                expected.1,
                expected.2,
                expected.3,
            );
        }
    }

    #[test]
    fn r4_explicit_bitrate_ladder_obeys_minimum_caps() {
        for (base, expected_bitrates) in [
            (30.0, [30.0, 30.0, 30.0, 18.0, 15.0]),
            (20.0, [20.0, 20.0, 20.0, 18.0, 15.0]),
            (16.0, [16.0, 16.0, 16.0, 16.0, 15.0]),
            (10.0, [10.0, 10.0, 10.0, 10.0, 10.0]),
        ] {
            let mut controller = controller(QualityProfile {
                width: 1920,
                height: 1080,
                fps: 60,
                bitrate_mbps: base,
            });
            assert_profile(controller.current(), 1920, 1080, 60, expected_bitrates[0]);

            for (index, second) in [40, 80, 120, 160].into_iter().enumerate() {
                force_network_degrade(&mut controller, second, 3)
                    .expect("each adjacent R4 quality step should produce an action");
                let (width, height) = match index {
                    0 => (1600, 900),
                    _ => (1280, 720),
                };
                assert_profile(
                    controller.current(),
                    width,
                    height,
                    60,
                    expected_bitrates[index + 1],
                );
            }
        }
    }

    #[test]
    fn r4_recovery_is_exact_reverse_adjacent_path() {
        let nominal = QualityProfile {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_mbps: 22.0,
        };
        let mut controller = controller(nominal);
        controller.current = QualityProfile {
            width: 1280,
            height: 720,
            fps: 60,
            bitrate_mbps: 15.0,
        };

        for (second, expected) in [
            (40, (1280, 720, 60, 18.0)),
            (80, (1280, 720, 60, 22.0)),
            (120, (1600, 900, 60, 22.0)),
            (160, (1920, 1080, 60, 22.0)),
        ] {
            force_stable_recovery(&mut controller, second)
                .expect("each reverse R4 quality step should produce an action");
            assert_profile(
                controller.current(),
                expected.0,
                expected.1,
                expected.2,
                expected.3,
            );
        }
    }

    #[test]
    fn r4_emergency_fps_steps_are_only_reachable_below_q4() {
        let nominal = QualityProfile {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_mbps: 22.0,
        };
        let mut controller = controller(nominal);
        controller.current = QualityProfile {
            width: 1280,
            height: 720,
            fps: 60,
            bitrate_mbps: 15.0,
        };

        assert!(force_network_degrade(&mut controller, 40, 4).is_none());
        force_network_degrade(&mut controller, 80, 5)
            .expect("Q4 sustained pressure should enter E1");
        assert_profile(controller.current(), 1280, 720, 45, 15.0);
        force_network_degrade(&mut controller, 120, 5)
            .expect("E1 sustained pressure should enter E2");
        assert_profile(controller.current(), 1280, 720, 30, 15.0);

        force_stable_recovery(&mut controller, 160).expect("E2 should recover to E1");
        assert_profile(controller.current(), 1280, 720, 45, 15.0);
        force_stable_recovery(&mut controller, 200).expect("E1 should recover to Q4");
        assert_profile(controller.current(), 1280, 720, 60, 15.0);
    }

    #[test]
    fn r4_transition_suppresses_pressure_and_profile_generation() {
        let initial = QualityProfile {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_mbps: 22.0,
        };
        let mut controller = controller(initial);
        controller.begin_profile_transition();

        for second in 1..=20 {
            let mut snapshot = stable_snapshot();
            snapshot.profile_transition_active = true;
            snapshot.feedback_sample_eligible = false;
            snapshot.damaged_gop_delta = 10;
            snapshot.packets_lost_delta = 100;
            assert!(controller
                .observe(Duration::from_secs(second), snapshot)
                .is_none());
        }

        let telemetry = controller.telemetry(Duration::from_secs(20));
        assert_eq!(controller.current(), initial);
        assert_eq!(telemetry.profile_generation, 0);
        assert_eq!(telemetry.profile_changes, 0);
        assert_eq!(telemetry.bitrate_changes, 0);
        assert_eq!(telemetry.resolution_changes, 0);
        assert_eq!(telemetry.fps_changes, 0);
        assert_eq!(telemetry.mild_window_count, 0);
        assert_eq!(telemetry.severe_window_count, 0);
        assert_eq!(telemetry.stable_window_count, 0);
    }

    #[test]
    fn deterministic_ten_minute_soak_has_no_self_excited_degradation() {
        let initial = QualityProfile {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_mbps: 50.0,
        };
        let mut controller = controller(initial);
        for second in 1..=600 {
            if second == 60 {
                let _ = controller.apply_action(
                    Duration::from_secs(second),
                    AdaptiveAction::SetBitrate {
                        bitrate_mbps: 46.0,
                        reason: "deterministic-runtime-bitrate-update".to_string(),
                    },
                );
                controller.nominal.bitrate_mbps = 46.0;
                continue;
            }
            if second == 180 {
                let _ = controller.apply_action(
                    Duration::from_secs(second),
                    AdaptiveAction::SetResolution {
                        width: 1600,
                        height: 900,
                        reason: "deterministic-structural-change".to_string(),
                    },
                );
                controller.nominal.width = 1600;
                controller.nominal.height = 900;
                controller.nominal.bitrate_mbps = 46.0;
                controller.begin_profile_transition();
                continue;
            }
            let mut snapshot = stable_for_current(&controller);
            if (181..=183).contains(&second) {
                snapshot.feedback_sample_eligible = false;
                snapshot.profile_transition_active = true;
            } else if second == 184 {
                controller.finish_profile_transition(Duration::from_secs(second));
            } else if second == 300 {
                snapshot.feedback_fresh = false;
                snapshot.feedback_sample_eligible = false;
            }
            assert!(controller
                .observe(Duration::from_secs(second), snapshot)
                .is_none());
        }
        let telemetry = controller.telemetry(Duration::from_secs(600));
        assert_eq!(telemetry.bitrate_changes, 1);
        assert_eq!(telemetry.resolution_changes, 1);
        assert_eq!(telemetry.profile_changes, 1);
        assert_eq!(telemetry.fps_changes, 0);
        assert_eq!(telemetry.current.fps, 60);
        assert_eq!(
            (telemetry.current.width, telemetry.current.height),
            (1600, 900)
        );
    }

    #[test]
    fn sliding_window_resets_at_session_boundary_and_requires_a_fresh_baseline() {
        let mut tracker = AdaptiveWindowTracker::default();
        for second in 0..5 {
            let metrics = tracker.observe(
                10,
                Duration::from_secs(second),
                AdaptiveIntervalSample {
                    data_bytes: 1_000,
                    repair_bytes: 100,
                    packets_lost: 1,
                    complete_frame_ratio: 1.0,
                    decoded_frame_ratio: 1.0,
                    rendered_frame_ratio: 1.0,
                    ..AdaptiveIntervalSample::default()
                },
            );
            assert_eq!(metrics.window_ready, second == 4);
        }
        let old_session = tracker.observe(
            10,
            Duration::from_secs(5),
            AdaptiveIntervalSample {
                data_bytes: 1_000,
                repair_bytes: 100,
                packets_lost: 1,
                ..AdaptiveIntervalSample::default()
            },
        );
        assert!(old_session.window_ready);
        assert_eq!(old_session.packet_loss_delta_5s, 6);

        let new_session = tracker.observe(
            11,
            Duration::from_secs(6),
            AdaptiveIntervalSample {
                data_bytes: 2_000,
                repair_bytes: 0,
                packets_lost: 0,
                ..AdaptiveIntervalSample::default()
            },
        );
        assert!(!new_session.window_ready);
        assert_eq!(new_session.valid_windows, 1);
        assert_eq!(new_session.packet_loss_delta_5s, 0);
        assert_eq!(new_session.repair_overhead_ratio_5s, 0.0);
    }

    #[test]
    fn isolated_thirty_three_ms_present_sample_is_not_network_pressure() {
        let mut snapshot = stable_snapshot();
        snapshot.present_interval_p95_ms = 33.0;
        let pressure = classify_pressure(snapshot);
        assert_eq!(pressure.network, PressureSeverity::None);
        assert_eq!(pressure.decoder, PressureSeverity::None);
        assert_eq!(pressure.render, PressureSeverity::None);
        assert_eq!(classify_bottleneck(snapshot), Bottleneck::Stable);
    }

    #[test]
    fn transport_window_gate_blocks_adaptation_even_after_local_warmup() {
        let initial = QualityProfile {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_mbps: 50.0,
        };
        let mut controller = controller(initial);
        let mut severe = stable_snapshot();
        severe.damaged_gop_delta = 1;
        for second in 1..=20 {
            assert!(controller
                .observe_windowed(Duration::from_secs(second), severe, false)
                .is_none());
        }
        let telemetry = controller.telemetry(Duration::from_secs(20));
        assert_eq!(controller.current(), initial);
        assert!(!telemetry.window_ready);
        assert_eq!(
            telemetry.adaptation_suppressed_reason.as_deref(),
            Some("window-not-ready")
        );
    }
}

pub fn run_self_test() -> Result<(), String> {
    let initial = QualityProfile {
        width: 1920,
        height: 1080,
        fps: 60,
        bitrate_mbps: 50.0,
    };
    let mut controller = AdaptiveQualityController::new(
        AdaptiveConfig {
            mode: AdaptiveMode::Smoothness,
            ..AdaptiveConfig::default()
        },
        initial,
        Duration::ZERO,
    );
    let mut mild = tests_support::stable_snapshot();
    mild.actual_mbps = 40.0;
    mild.pacing_late_us_p95 = 9_000.0;
    for second in 1..9 {
        if controller
            .observe(Duration::from_secs(second), mild)
            .is_some()
        {
            return Err("mild pressure changed quality before warm-up and hysteresis".to_string());
        }
    }
    let Some(action) = controller.observe(Duration::from_secs(9), mild) else {
        return Err("sustained mild pressure did not reduce bitrate".to_string());
    };
    if !matches!(action, AdaptiveAction::SetBitrate { .. }) || controller.current().fps != 60 {
        return Err("frame-rate-first degradation order failed".to_string());
    }
    Ok(())
}

mod tests_support {
    use super::AdaptiveSnapshot;

    pub fn stable_snapshot() -> AdaptiveSnapshot {
        AdaptiveSnapshot {
            target_fps: 60,
            actual_sender_fps: 60.0,
            capture_actual_fps: 60.0,
            encoder_actual_fps: 60.0,
            encode_lag_skips_delta: 0,
            capture_dropped_delta: 0,
            video_worker_loop_ms_p99: 2.0,
            packetize_send_ms_p95: 2.0,
            packetize_send_ms_p99: 3.0,
            pacing_late_us_p95: 100.0,
            pacing_late_us_p99: 200.0,
            send_syscall_ms_p95: 0.1,
            actual_mbps: 50.0,
            target_mbps: 50.0,
            repair_packets_resent_delta: 0,
            send_errors_delta: 0,
            receiver_active_fps: 60.0,
            receiver_decoder_input_fps: 60.0,
            decode_queue_drops_delta: 0,
            render_replacements_delta: 0,
            repair_deadline_missed_delta: 0,
            damaged_gop_delta: 0,
            packets_lost_delta: 0,
            present_fps_measured: 60.0,
            present_interval_p95_ms: 17.0,
            feedback_fresh: true,
            feedback_sample_eligible: true,
            profile_transition_active: false,
            audio_queue_dropping: false,
        }
    }
}
