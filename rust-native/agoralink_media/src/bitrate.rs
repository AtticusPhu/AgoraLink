#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BitrateSource {
    Default,
    ExplicitBitrate,
    QualityBpf,
}

impl BitrateSource {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::ExplicitBitrate => "explicit-bitrate",
            Self::QualityBpf => "quality-bpf",
        }
    }
}

#[derive(Clone, Debug)]
pub struct BitrateSelection {
    pub target_mbps: f64,
    pub quality_bpf_requested: Option<f64>,
    pub source: BitrateSource,
    width: u32,
    height: u32,
    fps: u32,
}

impl BitrateSelection {
    pub fn resolve(
        width: u32,
        height: u32,
        fps: u32,
        default_mbps: f64,
        explicit_mbps: Option<f64>,
        quality_bpf_requested: Option<f64>,
    ) -> Result<Self, String> {
        validate_dimensions(width, height, fps)?;
        validate_positive(default_mbps, "default bitrate-mbps")?;
        if let Some(value) = explicit_mbps {
            validate_positive(value, "bitrate-mbps")?;
        }
        if let Some(value) = quality_bpf_requested {
            validate_positive(value, "quality-bpf")?;
        }

        let (target_mbps, source) = if let Some(value) = explicit_mbps {
            (value, BitrateSource::ExplicitBitrate)
        } else if let Some(value) = quality_bpf_requested {
            (
                pixels_per_second(width, height, fps) * value / 1_000_000.0,
                BitrateSource::QualityBpf,
            )
        } else {
            (default_mbps, BitrateSource::Default)
        };
        validate_positive(target_mbps, "resolved bitrate-mbps")?;

        Ok(Self {
            target_mbps,
            quality_bpf_requested,
            source,
            width,
            height,
            fps,
        })
    }

    pub fn quality_bpf_effective(&self) -> f64 {
        bpf_from_mbps(self.target_mbps, self.width, self.height, self.fps)
    }

    pub fn actual_bpf(&self, actual_mbps: f64) -> Option<f64> {
        if actual_mbps.is_finite() && actual_mbps >= 0.0 {
            Some(bpf_from_mbps(
                actual_mbps,
                self.width,
                self.height,
                self.fps,
            ))
        } else {
            None
        }
    }

    pub fn warning(&self) -> Option<&'static str> {
        (self.target_mbps >= 80.0).then_some("high-bitrate")
    }

    pub fn json_fragment(&self, actual_mbps: Option<f64>) -> String {
        format!(
            r#""quality_bpf_requested":{},"quality_bpf_effective":{:.6},"bitrate_source":"{}","actual_bpf":{},"bitrate_warning":{}"#,
            optional_f64(self.quality_bpf_requested),
            self.quality_bpf_effective(),
            self.source.name(),
            optional_f64(actual_mbps.and_then(|value| self.actual_bpf(value))),
            optional_json_string(self.warning()),
        )
    }
}

fn pixels_per_second(width: u32, height: u32, fps: u32) -> f64 {
    f64::from(width) * f64::from(height) * f64::from(fps)
}

fn bpf_from_mbps(mbps: f64, width: u32, height: u32, fps: u32) -> f64 {
    mbps * 1_000_000.0 / pixels_per_second(width, height, fps)
}

fn validate_dimensions(width: u32, height: u32, fps: u32) -> Result<(), String> {
    if width == 0 || height == 0 || fps == 0 {
        Err("width, height, and fps must be greater than zero for BPF calculation".to_string())
    } else {
        Ok(())
    }
}

fn validate_positive(value: f64, name: &str) -> Result<(), String> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(format!("{name} must be a finite value greater than zero"))
    }
}

fn optional_f64(value: Option<f64>) -> String {
    value.map_or_else(|| "null".to_string(), |value| format!("{value:.6}"))
}

fn optional_json_string(value: Option<&str>) -> String {
    value.map_or_else(|| "null".to_string(), |value| format!(r#""{value}""#))
}

pub fn run_self_test() -> Result<(), String> {
    for (width, height, fps, expected_mbps) in [
        (1280, 720, 30, 27.648),
        (1280, 720, 60, 55.296),
        (1920, 1080, 30, 62.208),
        (1920, 1080, 60, 124.416),
    ] {
        let selection = BitrateSelection::resolve(width, height, fps, 4.0, None, Some(1.0))?;
        if (selection.target_mbps - expected_mbps).abs() > 0.000_001
            || (selection.quality_bpf_effective() - 1.0).abs() > 0.000_001
            || selection.source != BitrateSource::QualityBpf
        {
            return Err(format!(
                "BPF calculation mismatch for {width}x{height}@{fps}: {:?}",
                selection
            ));
        }
    }

    let explicit = BitrateSelection::resolve(1920, 1080, 30, 4.0, Some(24.0), Some(1.0))?;
    if explicit.source != BitrateSource::ExplicitBitrate
        || (explicit.target_mbps - 24.0).abs() > f64::EPSILON
        || explicit.quality_bpf_requested != Some(1.0)
    {
        return Err("explicit bitrate did not override quality-bpf".to_string());
    }
    let high = BitrateSelection::resolve(1920, 1080, 60, 4.0, None, Some(1.0))?;
    if high.warning() != Some("high-bitrate")
        || !high
            .json_fragment(Some(high.target_mbps))
            .contains(r#""bitrate_warning":"high-bitrate""#)
    {
        return Err("high bitrate warning was not emitted".to_string());
    }
    if BitrateSelection::resolve(1280, 720, 30, 4.0, None, Some(0.0)).is_ok() {
        return Err("zero quality-bpf was accepted".to_string());
    }
    Ok(())
}
