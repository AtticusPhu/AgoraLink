use crate::display_capability::RefreshRate;

pub const SAFE_FPS_TIERS: [u32; 7] = [30, 45, 50, 60, 75, 90, 120];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaxFps {
    Auto,
    Fixed(u32),
}

impl MaxFps {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "60" => Ok(Self::Fixed(60)),
            "75" => Ok(Self::Fixed(75)),
            "90" => Ok(Self::Fixed(90)),
            "120" => Ok(Self::Fixed(120)),
            _ => Err("max-fps must be auto, 60, 75, 90, or 120".to_string()),
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Fixed(60) => "60",
            Self::Fixed(75) => "75",
            Self::Fixed(90) => "90",
            Self::Fixed(120) => "120",
            Self::Fixed(_) => "custom",
        }
    }

    pub fn resolved(self, high_refresh_enabled: bool, feedback_is_fresh: bool) -> u32 {
        let requested = match self {
            Self::Auto if high_refresh_enabled && feedback_is_fresh => 120,
            Self::Auto => 60,
            Self::Fixed(value) => value,
        };
        if high_refresh_enabled && feedback_is_fresh {
            requested.min(120)
        } else {
            requested.min(60)
        }
    }
}

impl Default for MaxFps {
    fn default() -> Self {
        Self::Fixed(60)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FrameRatePolicyInput {
    pub source_refresh: Option<RefreshRate>,
    pub receiver_refresh: Option<RefreshRate>,
    pub user_max_fps: MaxFps,
    pub configured_fps: u32,
    pub capture_sustainable_fps: Option<f64>,
    pub encoder_sustainable_fps: Option<f64>,
    pub adaptive_enabled: bool,
    pub high_refresh_enabled: bool,
    pub feedback_is_fresh: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrameRateDecision {
    pub source_refresh_hz: Option<f64>,
    pub receiver_refresh_hz: Option<f64>,
    pub user_max_fps: u32,
    pub nominal_target_fps: u32,
    pub effective_target_fps: u32,
    pub selection_reason: String,
    pub limit_source: String,
    pub limited_by_source_display: bool,
    pub limited_by_receiver_display: bool,
    pub limited_by_user: bool,
    pub limited_by_capture: bool,
    pub limited_by_encoder: bool,
    pub receiver_capability_stale: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SustainableFpsEstimator {
    capture_ewma: Option<f64>,
    encoder_ewma: Option<f64>,
    valid_windows: u32,
}

impl SustainableFpsEstimator {
    pub fn observe(
        &mut self,
        sample_eligible: bool,
        transition_active: bool,
        capture_fps: f64,
        encoder_fps: f64,
    ) {
        if !sample_eligible || transition_active {
            return;
        }
        self.capture_ewma = update_ewma(self.capture_ewma, capture_fps);
        self.encoder_ewma = update_ewma(self.encoder_ewma, encoder_fps);
        self.valid_windows = self.valid_windows.saturating_add(1);
    }

    pub fn reset_for_transition(&mut self) {
        self.capture_ewma = None;
        self.encoder_ewma = None;
        self.valid_windows = 0;
    }

    pub fn estimates(self) -> (Option<f64>, Option<f64>) {
        if self.valid_windows < 3 {
            (None, None)
        } else {
            (self.capture_ewma, self.encoder_ewma)
        }
    }
}

impl FrameRateDecision {
    pub fn json_fragment(&self) -> String {
        format!(
            concat!(
                r#""fps_policy_source_refresh_hz":{},"fps_policy_receiver_refresh_hz":{},"user_max_fps":{},"#,
                r#""nominal_target_fps":{},"effective_target_fps":{},"#,
                r#""fps_selection_reason":"{}","fps_limit_source":"{}","#,
                r#""fps_limited_by_source_display":{},"fps_limited_by_receiver_display":{},"#,
                r#""fps_limited_by_user":{},"fps_limited_by_capture":{},"#,
                r#""fps_limited_by_encoder":{},"fps_policy_receiver_capability_stale":{}"#
            ),
            optional_f64(self.source_refresh_hz),
            optional_f64(self.receiver_refresh_hz),
            self.user_max_fps,
            self.nominal_target_fps,
            self.effective_target_fps,
            self.selection_reason,
            self.limit_source,
            self.limited_by_source_display,
            self.limited_by_receiver_display,
            self.limited_by_user,
            self.limited_by_capture,
            self.limited_by_encoder,
            self.receiver_capability_stale,
        )
    }
}

pub fn select_target_fps(input: FrameRatePolicyInput) -> FrameRateDecision {
    let source = input
        .source_refresh
        .and_then(|value| value.validate().ok())
        .map(RefreshRate::hz);
    let receiver = input
        .feedback_is_fresh
        .then_some(input.receiver_refresh)
        .flatten()
        .and_then(|value| value.validate().ok())
        .map(RefreshRate::hz);
    let user_max = input
        .user_max_fps
        .resolved(input.high_refresh_enabled, input.feedback_is_fresh);
    let mut limits = vec![("user", f64::from(user_max))];
    if let Some(value) = source {
        limits.push(("source-display", value));
    }
    if let Some(value) = receiver {
        limits.push(("receiver-display", value));
    } else {
        limits.push(("receiver-safe-default", 60.0));
    }
    if let Some(value) = sustainable_limit(input.capture_sustainable_fps, input.configured_fps) {
        limits.push(("capture", value));
    }
    if let Some(value) = sustainable_limit(input.encoder_sustainable_fps, input.configured_fps) {
        limits.push(("encoder", value));
    }
    let (limit_source, limit) = limits
        .iter()
        .copied()
        .min_by(|left, right| left.1.total_cmp(&right.1))
        .unwrap_or(("safe-default", 60.0));
    let nominal = safe_tier_at_or_below(limit);
    // This is the FPS the active encoder pipeline is actually configured for.
    // A policy target does not become effective until the adaptive controller
    // emits a SetFps action, which also updates its FPS-change telemetry.
    let effective = input.configured_fps;
    let tolerance = 0.11;
    FrameRateDecision {
        source_refresh_hz: source,
        receiver_refresh_hz: receiver,
        user_max_fps: user_max,
        nominal_target_fps: nominal,
        effective_target_fps: effective,
        selection_reason: if input.adaptive_enabled {
            format!("safe-tier-limited-by-{limit_source}")
        } else {
            "fixed-mode-adaptive-off".to_string()
        },
        limit_source: limit_source.to_string(),
        limited_by_source_display: source.is_some_and(|value| value <= limit + tolerance),
        limited_by_receiver_display: receiver.is_some_and(|value| value <= limit + tolerance),
        limited_by_user: f64::from(user_max) <= limit + tolerance,
        limited_by_capture: sustainable_limit(input.capture_sustainable_fps, input.configured_fps)
            .is_some_and(|value| value <= limit + tolerance),
        limited_by_encoder: sustainable_limit(input.encoder_sustainable_fps, input.configured_fps)
            .is_some_and(|value| value <= limit + tolerance),
        receiver_capability_stale: !input.feedback_is_fresh,
    }
}

fn safe_tier_at_or_below(limit: f64) -> u32 {
    // 59.94Hz is a 60Hz timing family. Preserve its rational in telemetry while
    // selecting the 60 FPS encoder tier.
    let tolerance = if (59.0..60.0).contains(&limit) {
        0.1
    } else {
        0.001
    };
    SAFE_FPS_TIERS
        .iter()
        .copied()
        .filter(|tier| f64::from(*tier) <= limit + tolerance)
        .next_back()
        .unwrap_or(30)
}

fn finite_positive(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value > 0.0)
}

fn sustainable_limit(value: Option<f64>, configured_fps: u32) -> Option<f64> {
    let configured = f64::from(configured_fps);
    // A capped capture/encode loop naturally reports the current target. It is
    // not evidence that the machine cannot sustain a higher policy tier, and a
    // short low sample must never rewrite the nominal/effective FPS directly.
    finite_positive(value).filter(|value| *value > configured * 1.05)
}

fn update_ewma(previous: Option<f64>, sample: f64) -> Option<f64> {
    if !sample.is_finite() || sample <= 0.0 {
        return previous;
    }
    Some(previous.map_or(sample, |previous| previous * 0.8 + sample * 0.2))
}

fn optional_f64(value: Option<f64>) -> String {
    value.map_or_else(|| "null".to_string(), |value| format!("{value:.3}"))
}

pub fn run_self_test() -> Result<(), String> {
    let decision = |source_num, source_den, receiver, max, high, fresh| {
        select_target_fps(FrameRatePolicyInput {
            source_refresh: Some(RefreshRate::new(source_num, source_den)),
            receiver_refresh: Some(RefreshRate::new(receiver, 1)),
            user_max_fps: MaxFps::Fixed(max),
            configured_fps: 60,
            capture_sustainable_fps: None,
            encoder_sustainable_fps: None,
            adaptive_enabled: true,
            high_refresh_enabled: high,
            feedback_is_fresh: fresh,
        })
    };
    if decision(60, 1, 144, 120, true, true).nominal_target_fps != 60
        || decision(144, 1, 60, 120, true, true).nominal_target_fps != 60
        || decision(120, 1, 120, 60, false, true).nominal_target_fps != 60
        || decision(120, 1, 120, 120, true, true).nominal_target_fps != 120
        || decision(144, 1, 75, 120, true, true).nominal_target_fps != 75
        || decision(60_000, 1001, 60, 120, true, true).nominal_target_fps != 60
        || decision(120, 1, 120, 120, true, false).nominal_target_fps != 60
    {
        return Err("frame-rate policy deterministic matrix failed".to_string());
    }
    let fixed = select_target_fps(FrameRatePolicyInput {
        source_refresh: Some(RefreshRate::new(30, 1)),
        receiver_refresh: Some(RefreshRate::new(30, 1)),
        user_max_fps: MaxFps::Fixed(60),
        configured_fps: 50,
        capture_sustainable_fps: Some(20.0),
        encoder_sustainable_fps: Some(20.0),
        adaptive_enabled: false,
        high_refresh_enabled: false,
        feedback_is_fresh: true,
    });
    if fixed.effective_target_fps != 50 || fixed.selection_reason != "fixed-mode-adaptive-off" {
        return Err("adaptive-off fixed FPS behavior changed".to_string());
    }
    let sustainable = select_target_fps(FrameRatePolicyInput {
        source_refresh: Some(RefreshRate::new(60, 1)),
        receiver_refresh: Some(RefreshRate::new(60, 1)),
        user_max_fps: MaxFps::Fixed(60),
        configured_fps: 60,
        capture_sustainable_fps: Some(59.0),
        encoder_sustainable_fps: Some(58.5),
        adaptive_enabled: true,
        high_refresh_enabled: false,
        feedback_is_fresh: true,
    });
    if sustainable.effective_target_fps != 60 {
        return Err("normal one-second FPS measurement jitter lowered the safe tier".to_string());
    }
    let mut estimator = SustainableFpsEstimator::default();
    estimator.observe(false, false, 0.0, 0.0);
    estimator.observe(true, true, 5.0, 5.0);
    estimator.observe(true, false, 60.0, 60.0);
    estimator.observe(true, false, 30.0, 30.0);
    if estimator.estimates() != (None, None) {
        return Err(
            "sustainable FPS estimator accepted fewer than three valid windows".to_string(),
        );
    }
    estimator.observe(true, false, 60.0, 60.0);
    let (capture, encoder) = estimator.estimates();
    if capture.is_none_or(|value| !(45.0..=60.0).contains(&value))
        || encoder.is_none_or(|value| !(45.0..=60.0).contains(&value))
    {
        return Err("sustainable FPS EWMA produced an invalid estimate".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_refresh_and_fixed_mode_policy() {
        super::run_self_test().expect("frame-rate policy self-test");
    }

    #[test]
    fn monitor_change_rechecks_high_refresh_without_raising_default_cap() {
        let decision = |receiver_refresh, max_fps, high_refresh_enabled| {
            select_target_fps(FrameRatePolicyInput {
                source_refresh: Some(RefreshRate::new(120, 1)),
                receiver_refresh: Some(RefreshRate::new(receiver_refresh, 1)),
                user_max_fps: MaxFps::Fixed(max_fps),
                configured_fps: 60,
                capture_sustainable_fps: Some(120.0),
                encoder_sustainable_fps: Some(120.0),
                adaptive_enabled: true,
                high_refresh_enabled,
                feedback_is_fresh: true,
            })
        };

        assert_eq!(decision(60, 120, true).nominal_target_fps, 60);
        assert_eq!(decision(144, 60, false).nominal_target_fps, 60);
        assert_eq!(decision(144, 120, true).nominal_target_fps, 120);
        assert_eq!(decision(144, 120, true).effective_target_fps, 60);
    }

    #[test]
    fn one_low_window_cannot_change_nominal_or_effective_fps() {
        let decision = select_target_fps(FrameRatePolicyInput {
            source_refresh: Some(RefreshRate::new(60, 1)),
            receiver_refresh: Some(RefreshRate::new(60, 1)),
            user_max_fps: MaxFps::Fixed(60),
            configured_fps: 60,
            capture_sustainable_fps: Some(30.0),
            encoder_sustainable_fps: Some(30.0),
            adaptive_enabled: true,
            high_refresh_enabled: false,
            feedback_is_fresh: true,
        });
        assert_eq!(decision.nominal_target_fps, 60);
        assert_eq!(decision.effective_target_fps, 60);
        assert!(!decision.limited_by_capture);
        assert!(!decision.limited_by_encoder);
    }
}
