#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConvertBackend {
    Auto,
    Cpu,
    D3d11,
}

impl ConvertBackend {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cpu => "cpu",
            Self::D3d11 => "d3d11",
        }
    }
}

#[derive(Debug)]
pub struct CaptureEncodeConfig {
    pub duration_sec: Option<u64>,
    pub target_fps: u32,
    pub bitrate_mbps: f64,
    pub bitrate_selection: crate::bitrate::BitrateSelection,
    pub out_width: u32,
    pub out_height: u32,
    pub output: String,
    pub color_spec: crate::color_spec::ColorSpec,
    pub encoder: crate::wmf_h264_encoder::EncoderChoice,
    pub convert_backend: ConvertBackend,
    pub keyframe_interval_sec: Option<f64>,
    pub verbose: bool,
}

#[cfg(windows)]
mod platform {
    use std::collections::VecDeque;
    use std::fs::File;
    use std::io::{self, Write};
    use std::path::Path;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{CaptureEncodeConfig, ConvertBackend};
    use crate::bgra_to_nv12;
    use crate::bitrate::BitrateSelection;
    use crate::color_spec::{ColorSpec, MediaColorMetadata};
    use crate::gpu_nv12_capture::{GpuCaptureStats, GpuNv12Capture};
    use crate::wgc_latest_capture::{LatestCapture, LatestCaptureStats};
    use crate::wmf_h264_encoder::{
        EncodedSample, EncoderKeyframeControl, EncoderSelection, EncoderStats, WmfH264Encoder,
    };

    const PIXEL_FORMAT_NAME: &str = "B8G8R8A8";
    const KEYFRAME_TELEMETRY_HISTORY: usize = 16;
    pub fn stop_requested() -> bool {
        crate::shutdown::ctrl_c_requested()
    }

    #[derive(Clone, Debug)]
    pub struct ConversionSelection {
        pub requested: ConvertBackend,
        pub selected: ConvertBackend,
        pub fallback_reason: Option<String>,
    }

    impl ConversionSelection {
        pub fn fallback(&self) -> bool {
            self.requested == ConvertBackend::Auto && self.selected != ConvertBackend::D3d11
        }

        pub fn selected_name(&self) -> &'static str {
            match self.selected {
                ConvertBackend::D3d11 => "d3d11-video-processor",
                ConvertBackend::Cpu | ConvertBackend::Auto => "cpu",
            }
        }

        pub fn json_fragment(&self) -> String {
            format!(
                r#""convert_backend_requested":"{}","convert_backend_selected":"{}","convert_fallback":{},"convert_fallback_reason":{}"#,
                self.requested.name(),
                self.selected_name(),
                self.fallback(),
                optional_json_string(self.fallback_reason.as_deref()),
            )
        }
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct CaptureStats {
        raw_frames: u64,
        latest_updates: u64,
        callback_skipped: u64,
        dropped: u64,
        copy_ms_total: f64,
        gpu_convert_ms_total: f64,
    }

    #[derive(Clone, Copy, Debug)]
    struct CaptureInfo {
        width: u32,
        height: u32,
        driver_name: &'static str,
    }

    enum CaptureSource {
        Cpu(LatestCapture),
        Gpu(GpuNv12Capture),
    }

    struct CaptureRuntime {
        source: CaptureSource,
        selection: ConversionSelection,
    }

    #[derive(Clone, Debug)]
    pub struct CapturePipelineStats {
        pub capture_raw_frames: u64,
        pub capture_latest_updates: u64,
        pub capture_callback_skipped: u64,
        pub capture_dropped: u64,
        pub encode_ticks: u64,
        pub no_new_frame_skipped: u64,
        pub no_new_frame_reused: u64,
        pub frames_encoded: u64,
        pub encode_lag_skips: u64,
        pub keyframe_force_requests: u64,
        pub keyframe_force_failures: u64,
        pub keyframe_force_last_requested_frame_id: Option<u64>,
        pub keyframe_force_last_effective_frame_id: Option<u64>,
        pub keyframe_force_latency_frames_avg: f64,
        pub keyframe_force_latency_frames_max: u64,
        pub keyframe_force_request_frame_ids: Vec<u64>,
        pub keyframe_force_effective_frame_ids: Vec<u64>,
        pub keyframe_force_latency_frames: Vec<u64>,
        pub samples_out: u64,
        pub bytes_out: u64,
        pub raw_fps: f64,
        pub accepted_fps: f64,
        pub encode_fps: f64,
        pub target_fps: u32,
        pub mbps: f64,
        pub target_bitrate_mbps: f64,
        pub bitrate_selection: BitrateSelection,
        pub width: u32,
        pub height: u32,
        pub copy_ms_avg: f64,
        pub convert_ms_avg: f64,
        pub gpu_convert_ms_avg: f64,
        pub cpu_convert_ms_avg: f64,
        pub encode_ms_avg: f64,
        pub qsv_async_wait_timeouts: u64,
        pub qsv_async_wait_cancelled: u64,
        pub qsv_drain_timeouts: u64,
        pub conversion_selection: ConversionSelection,
        pub color_spec: ColorSpec,
        pub encoder_selection: EncoderSelection,
        pub encoder_input_color_metadata: MediaColorMetadata,
        pub encoder_output_color_metadata: MediaColorMetadata,
    }

    #[derive(Clone, Debug)]
    pub struct CapturePipelineStarted {
        pub target_fps: u32,
        pub bitrate_mbps: f64,
        pub bitrate_selection: BitrateSelection,
        pub width: u32,
        pub height: u32,
        pub color_spec: ColorSpec,
        pub conversion_selection: ConversionSelection,
        pub encoder_selection: EncoderSelection,
        pub encoder_input_color_metadata: MediaColorMetadata,
        pub encoder_output_color_metadata: MediaColorMetadata,
        pub keyframe_interval_applied: bool,
        pub keyframe_interval_target_frames: Option<u32>,
        pub keyframe_force_supported: bool,
        pub keyframe_control: EncoderKeyframeControl,
    }

    #[derive(Clone, Debug)]
    pub struct CapturePipelineDone {
        pub capture_raw_frames: u64,
        pub capture_latest_updates: u64,
        pub capture_callback_skipped: u64,
        pub capture_dropped: u64,
        pub encode_ticks: u64,
        pub no_new_frame_skipped: u64,
        pub no_new_frame_reused: u64,
        pub frames_encoded: u64,
        pub encode_lag_skips: u64,
        pub encoder: EncoderStats,
        pub media_duration_sec: f64,
        pub wall_time_sec: f64,
        pub processing_fps: f64,
        pub mbps: f64,
        pub bitrate_mbps: f64,
        pub bitrate_selection: BitrateSelection,
        pub width: u32,
        pub height: u32,
        pub copy_ms_avg: f64,
        pub convert_ms_avg: f64,
        pub gpu_convert_ms_avg: f64,
        pub cpu_convert_ms_avg: f64,
        pub encode_ms_avg: f64,
        pub stopped_by_console: bool,
        pub color_spec: ColorSpec,
        pub conversion_selection: ConversionSelection,
        pub encoder_selection: EncoderSelection,
        pub encoder_input_color_metadata: MediaColorMetadata,
        pub encoder_output_color_metadata: MediaColorMetadata,
        pub keyframe_force_requests: u64,
        pub keyframe_force_failures: u64,
        pub keyframe_force_last_requested_frame_id: Option<u64>,
        pub keyframe_force_last_effective_frame_id: Option<u64>,
        pub keyframe_force_latency_frames_avg: f64,
        pub keyframe_force_latency_frames_max: u64,
        pub keyframe_force_request_frame_ids: Vec<u64>,
        pub keyframe_force_effective_frame_ids: Vec<u64>,
        pub keyframe_force_latency_frames: Vec<u64>,
        pub reconfigure_requested: bool,
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum CapturePipelineControl {
        Continue,
        UpdateBitrate(f64),
        Restart,
    }

    pub trait CaptureEncodeObserver {
        fn on_started(&mut self, _started: &CapturePipelineStarted) -> Result<(), String> {
            Ok(())
        }

        fn on_sample(&mut self, sample: EncodedSample) -> Result<(), String>;
        fn on_stats(&mut self, stats: &CapturePipelineStats) -> Result<(), String>;

        fn on_encoder_terminal_stats(&mut self, _stats: EncoderStats) {}

        fn stop_requested(&self) -> bool {
            false
        }

        fn cancellation_token(&self) -> Option<crate::shutdown::CancellationToken> {
            None
        }

        fn take_control(&mut self) -> CapturePipelineControl {
            CapturePipelineControl::Continue
        }

        fn on_bitrate_update_result(
            &mut self,
            _requested_mbps: f64,
            result: &Result<(), String>,
            _idr_requested: bool,
        ) -> Result<bool, String> {
            Ok(result.is_err())
        }
    }

    #[derive(Clone, Copy, Default)]
    struct PipelineCounters {
        encode_ticks: u64,
        no_new_frame_skipped: u64,
        no_new_frame_reused: u64,
        frames_encoded: u64,
        encode_lag_skips: u64,
        keyframe_force_requests: u64,
        keyframe_force_failures: u64,
        cpu_convert_ms_total: f64,
        encode_ms_total: f64,
    }

    #[derive(Default)]
    struct KeyframeRequestTracker {
        pending: VecDeque<u64>,
        last_requested_frame_id: Option<u64>,
        last_effective_frame_id: Option<u64>,
        latency_frames_total: u64,
        latency_frames_max: u64,
        effective_requests: u64,
        recent_requested: VecDeque<u64>,
        recent_effective: VecDeque<u64>,
        recent_latencies: VecDeque<u64>,
    }

    impl KeyframeRequestTracker {
        fn record_request(&mut self, frame_id: u64) {
            self.pending.push_back(frame_id);
            self.last_requested_frame_id = Some(frame_id);
            push_bounded(&mut self.recent_requested, frame_id);
        }

        fn observe_sample(&mut self, sample: &EncodedSample, fps: u32) {
            if !crate::h264_annex_b::summarize_nals(&sample.bytes).has_idr_slice {
                return;
            }
            let Some(sample_time_hns) = sample.sample_time_hns else {
                return;
            };
            let effective_frame_id =
                ((sample_time_hns.max(0) as u128 * u128::from(fps) + 5_000_000) / 10_000_000)
                    .min(u128::from(u64::MAX)) as u64;
            let Some(requested_frame_id) = self
                .pending
                .iter()
                .copied()
                .filter(|frame_id| *frame_id <= effective_frame_id)
                .next_back()
            else {
                return;
            };
            while self
                .pending
                .front()
                .is_some_and(|frame_id| *frame_id <= requested_frame_id)
            {
                self.pending.pop_front();
            }
            let latency = effective_frame_id.saturating_sub(requested_frame_id);
            self.last_effective_frame_id = Some(effective_frame_id);
            self.latency_frames_total = self.latency_frames_total.saturating_add(latency);
            self.latency_frames_max = self.latency_frames_max.max(latency);
            self.effective_requests += 1;
            push_bounded(&mut self.recent_effective, effective_frame_id);
            push_bounded(&mut self.recent_latencies, latency);
        }

        fn latency_frames_avg(&self) -> f64 {
            if self.effective_requests == 0 {
                0.0
            } else {
                self.latency_frames_total as f64 / self.effective_requests as f64
            }
        }

        fn request_frame_ids(&self) -> Vec<u64> {
            self.recent_requested.iter().copied().collect()
        }

        fn effective_frame_ids(&self) -> Vec<u64> {
            self.recent_effective.iter().copied().collect()
        }

        fn latency_frames(&self) -> Vec<u64> {
            self.recent_latencies.iter().copied().collect()
        }
    }

    fn push_bounded(values: &mut VecDeque<u64>, value: u64) {
        if values.len() == KEYFRAME_TELEMETRY_HISTORY {
            values.pop_front();
        }
        values.push_back(value);
    }

    fn keyframe_request_due(frames_encoded: u64, target_frames: Option<u32>) -> bool {
        frames_encoded > 0
            && target_frames.is_some_and(|frames| frames_encoded % u64::from(frames) == 0)
    }

    impl CaptureRuntime {
        fn start(config: &CaptureEncodeConfig) -> Result<Self, String> {
            match config.convert_backend {
                ConvertBackend::Cpu => Ok(Self {
                    source: CaptureSource::Cpu(LatestCapture::start()?),
                    selection: ConversionSelection {
                        requested: ConvertBackend::Cpu,
                        selected: ConvertBackend::Cpu,
                        fallback_reason: None,
                    },
                }),
                ConvertBackend::D3d11 => Ok(Self {
                    source: CaptureSource::Gpu(GpuNv12Capture::start(
                        config.out_width,
                        config.out_height,
                        config.target_fps,
                        config.color_spec,
                    )?),
                    selection: ConversionSelection {
                        requested: ConvertBackend::D3d11,
                        selected: ConvertBackend::D3d11,
                        fallback_reason: None,
                    },
                }),
                ConvertBackend::Auto => match GpuNv12Capture::start(
                    config.out_width,
                    config.out_height,
                    config.target_fps,
                    config.color_spec,
                ) {
                    Ok(capture) => Ok(Self {
                        source: CaptureSource::Gpu(capture),
                        selection: ConversionSelection {
                            requested: ConvertBackend::Auto,
                            selected: ConvertBackend::D3d11,
                            fallback_reason: None,
                        },
                    }),
                    Err(gpu_error) => {
                        if crate::shutdown::worker_ownership_failed(&gpu_error) {
                            return Err(gpu_error);
                        }
                        Ok(Self {
                            source: CaptureSource::Cpu(LatestCapture::start()?),
                            selection: ConversionSelection {
                                requested: ConvertBackend::Auto,
                                selected: ConvertBackend::Cpu,
                                fallback_reason: Some(gpu_error),
                            },
                        })
                    }
                },
            }
        }

        fn info(&self) -> CaptureInfo {
            match &self.source {
                CaptureSource::Cpu(capture) => {
                    let info = capture.info();
                    CaptureInfo {
                        width: info.width,
                        height: info.height,
                        driver_name: info.driver_name,
                    }
                }
                CaptureSource::Gpu(capture) => {
                    let info = capture.info();
                    CaptureInfo {
                        width: info.source_width,
                        height: info.source_height,
                        driver_name: info.driver_name,
                    }
                }
            }
        }

        fn error(&self) -> Option<String> {
            match &self.source {
                CaptureSource::Cpu(capture) => capture.error(),
                CaptureSource::Gpu(capture) => capture.error(),
            }
        }

        fn update_nv12(
            &self,
            config: &CaptureEncodeConfig,
            nv12: &mut Vec<u8>,
            last_version: &mut u64,
            counters: &mut PipelineCounters,
        ) -> Result<bool, String> {
            match &self.source {
                CaptureSource::Cpu(capture) => {
                    let Some(frame) = capture.latest() else {
                        return Ok(false);
                    };
                    if frame.version == *last_version {
                        return Ok(false);
                    }
                    let convert_started = Instant::now();
                    bgra_to_nv12::convert_scaled_with_spec(
                        &frame.bgra,
                        frame.row_pitch,
                        frame.width,
                        frame.height,
                        config.out_width,
                        config.out_height,
                        nv12,
                        config.color_spec,
                    )?;
                    counters.cpu_convert_ms_total +=
                        convert_started.elapsed().as_secs_f64() * 1000.0;
                    *last_version = frame.version;
                    Ok(true)
                }
                CaptureSource::Gpu(capture) => {
                    let Some(frame) = capture.latest() else {
                        return Ok(false);
                    };
                    if frame.version == *last_version {
                        return Ok(false);
                    }
                    if frame.nv12.len() != nv12.len() {
                        return Err(format!(
                            "GPU NV12 length mismatch: expected {}, got {}",
                            nv12.len(),
                            frame.nv12.len()
                        ));
                    }
                    nv12.copy_from_slice(&frame.nv12);
                    *last_version = frame.version;
                    Ok(true)
                }
            }
        }

        fn stats(&self) -> CaptureStats {
            match &self.source {
                CaptureSource::Cpu(capture) => {
                    let stats: LatestCaptureStats = capture.stats();
                    CaptureStats {
                        raw_frames: stats.raw_frames,
                        latest_updates: stats.latest_updates,
                        callback_skipped: stats.callback_skipped,
                        dropped: stats.dropped,
                        copy_ms_total: stats.copy_ms_total,
                        gpu_convert_ms_total: 0.0,
                    }
                }
                CaptureSource::Gpu(capture) => {
                    let stats: GpuCaptureStats = capture.stats();
                    CaptureStats {
                        raw_frames: stats.raw_frames,
                        latest_updates: stats.latest_updates,
                        callback_skipped: stats.callback_skipped + stats.pacing_skipped,
                        dropped: stats.dropped,
                        copy_ms_total: stats.copy_ms_total,
                        gpu_convert_ms_total: stats.gpu_convert_ms_total,
                    }
                }
            }
        }

        fn stop(self) -> Result<(), String> {
            match self.source {
                CaptureSource::Cpu(capture) => capture.stop(),
                CaptureSource::Gpu(capture) => capture.stop(),
            }
        }
    }

    struct FileProbeObserver {
        output: File,
    }

    impl CaptureEncodeObserver for FileProbeObserver {
        fn on_sample(&mut self, sample: EncodedSample) -> Result<(), String> {
            self.output
                .write_all(&sample.bytes)
                .map_err(|err| format!("write H.264 output failed: {err}"))
        }

        fn on_stats(&mut self, stats: &CapturePipelineStats) -> Result<(), String> {
            println!(
                r#"{{"type":"CAPTURE_ENCODE_STATS","mode":"capture_encode_probe","capture_raw_frames":{},"capture_latest_updates":{},"capture_callback_skipped":{},"capture_dropped":{},"encode_ticks":{},"no_new_frame_skipped":{},"no_new_frame_reused":{},"frames_encoded":{},"encode_lag_skips":{},"samples_out":{},"bytes_out":{},"raw_fps":{:.2},"accepted_fps":{:.2},"encode_fps":{:.2},"target_fps":{},"mbps":{:.3},"target_bitrate_mbps":{:.3},"width":{},"height":{},"format_in":"{}","format_encode":"NV12","copy_ms_avg":{:.3},"convert_ms_avg":{:.3},"gpu_convert_ms_avg":{:.3},"cpu_convert_ms_avg":{:.3},"encode_ms_avg":{:.3},"qsv_async_wait_timeouts":{},"qsv_async_wait_cancelled":{},"qsv_drain_timeouts":{},{},{},{},{},{},{}}}"#,
                stats.capture_raw_frames,
                stats.capture_latest_updates,
                stats.capture_callback_skipped,
                stats.capture_dropped,
                stats.encode_ticks,
                stats.no_new_frame_skipped,
                stats.no_new_frame_reused,
                stats.frames_encoded,
                stats.encode_lag_skips,
                stats.samples_out,
                stats.bytes_out,
                stats.raw_fps,
                stats.accepted_fps,
                stats.encode_fps,
                stats.target_fps,
                stats.mbps,
                stats.target_bitrate_mbps,
                stats.width,
                stats.height,
                PIXEL_FORMAT_NAME,
                stats.copy_ms_avg,
                stats.convert_ms_avg,
                stats.gpu_convert_ms_avg,
                stats.cpu_convert_ms_avg,
                stats.encode_ms_avg,
                stats.qsv_async_wait_timeouts,
                stats.qsv_async_wait_cancelled,
                stats.qsv_drain_timeouts,
                stats.bitrate_selection.json_fragment(Some(stats.mbps)),
                stats.conversion_selection.json_fragment(),
                stats.encoder_selection.json_fragment(),
                stats.color_spec.json_fragment(),
                stats
                    .encoder_input_color_metadata
                    .json_fragment("encoder_input"),
                stats
                    .encoder_output_color_metadata
                    .json_fragment("encoder_output")
            );
            io::stdout().flush().ok();
            Ok(())
        }
    }

    pub type ConsoleCtrlGuard = crate::shutdown::ConsoleCtrlGuard;

    pub fn install_console_ctrl_guard() -> Result<ConsoleCtrlGuard, String> {
        ConsoleCtrlGuard::install()
    }

    pub fn run(config: CaptureEncodeConfig) -> Result<(), String> {
        validate_config(&config)?;
        let output = File::create(Path::new(&config.output))
            .map_err(|err| format!("create output failed: {err}"))?;
        let mut observer = FileProbeObserver { output };
        let done = run_with_observer(&config, &mut observer)?;
        observer
            .output
            .flush()
            .map_err(|err| format!("flush output failed: {err}"))?;
        println!(
            r#"{{"type":"CAPTURE_ENCODE_DONE","encoder":"{}","capture_raw_frames":{},"capture_latest_updates":{},"capture_callback_skipped":{},"capture_dropped":{},"encode_ticks":{},"no_new_frame_skipped":{},"no_new_frame_reused":{},"frames_encoded":{},"encode_lag_skips":{},"samples_out":{},"bytes_out":{},"duration_sec":{:.3},"wall_time_sec":{:.3},"fps":{},"processing_fps":{:.2},"mbps":{:.3},"target_bitrate_mbps":{:.3},"keyframes":{},"width":{},"height":{},"copy_ms_avg":{:.3},"convert_ms_avg":{:.3},"gpu_convert_ms_avg":{:.3},"cpu_convert_ms_avg":{:.3},"encode_ms_avg":{:.3},"qsv_async_wait_timeouts":{},"qsv_async_wait_cancelled":{},"qsv_drain_timeouts":{},"output":"{}",{},{},{},{},{},{}}}"#,
            json_escape(&done.encoder_selection.selected_name),
            done.capture_raw_frames,
            done.capture_latest_updates,
            done.capture_callback_skipped,
            done.capture_dropped,
            done.encode_ticks,
            done.no_new_frame_skipped,
            done.no_new_frame_reused,
            done.frames_encoded,
            done.encode_lag_skips,
            done.encoder.samples_out,
            done.encoder.bytes_out,
            done.media_duration_sec,
            done.wall_time_sec,
            config.target_fps,
            done.processing_fps,
            done.mbps,
            config.bitrate_mbps,
            keyframes_json(done.encoder),
            done.width,
            done.height,
            done.copy_ms_avg,
            done.convert_ms_avg,
            done.gpu_convert_ms_avg,
            done.cpu_convert_ms_avg,
            done.encode_ms_avg,
            done.encoder.async_wait_timeouts,
            done.encoder.async_wait_cancelled,
            done.encoder.async_drain_timeouts,
            json_escape(&config.output),
            done.bitrate_selection.json_fragment(Some(done.mbps)),
            done.conversion_selection.json_fragment(),
            done.encoder_selection.json_fragment(),
            done.color_spec.json_fragment(),
            done.encoder_input_color_metadata
                .json_fragment("encoder_input"),
            done.encoder_output_color_metadata
                .json_fragment("encoder_output")
        );
        io::stdout().flush().ok();
        if config.verbose {
            eprintln!(
                "capture-encode-probe stopped reason={}",
                if done.stopped_by_console {
                    "console-control"
                } else {
                    "duration-complete"
                }
            );
        }
        Ok(())
    }

    pub struct PreparedCapturePipeline {
        capture: CaptureRuntime,
        encoder: WmfH264Encoder,
    }

    impl PreparedCapturePipeline {
        pub fn discard(self) -> Result<(), String> {
            self.capture.stop()
        }
    }

    pub fn prepare_pipeline(
        config: &CaptureEncodeConfig,
        cancellation: Option<crate::shutdown::CancellationToken>,
    ) -> Result<PreparedCapturePipeline, String> {
        validate_stream_config(config)?;
        let capture = CaptureRuntime::start(config)?;
        let keyframe_interval_target_frames = config.keyframe_interval_sec.map(|seconds| {
            (seconds * f64::from(config.target_fps))
                .round()
                .clamp(1.0, f64::from(u32::MAX)) as u32
        });
        let encoder_result = WmfH264Encoder::new_stream_with_color_choice_and_keyframe_interval(
            config.out_width,
            config.out_height,
            config.target_fps,
            config.bitrate_mbps,
            config.color_spec,
            config.encoder,
            keyframe_interval_target_frames,
        );
        let mut encoder = match encoder_result {
            Ok(encoder) => encoder,
            Err(error) => {
                let _ = capture.stop();
                return Err(error);
            }
        };
        encoder.set_async_cancellation(cancellation.unwrap_or_default());
        Ok(PreparedCapturePipeline { capture, encoder })
    }

    pub fn run_with_observer(
        config: &CaptureEncodeConfig,
        observer: &mut dyn CaptureEncodeObserver,
    ) -> Result<CapturePipelineDone, String> {
        let prepared = prepare_pipeline(config, observer.cancellation_token())?;
        run_prepared_with_observer(config, observer, prepared)
    }

    pub fn run_prepared_with_observer(
        config: &CaptureEncodeConfig,
        observer: &mut dyn CaptureEncodeObserver,
        prepared: PreparedCapturePipeline,
    ) -> Result<CapturePipelineDone, String> {
        validate_stream_config(config)?;
        let _console_ctrl = ConsoleCtrlGuard::install()?;
        let PreparedCapturePipeline {
            capture,
            mut encoder,
        } = prepared;
        let capture_info = capture.info();
        let keyframe_interval_target_frames = config.keyframe_interval_sec.map(|seconds| {
            (seconds * f64::from(config.target_fps))
                .round()
                .clamp(1.0, f64::from(u32::MAX)) as u32
        });
        let keyframe_control = encoder.keyframe_control().clone();
        let keyframe_force_supported =
            keyframe_interval_target_frames.is_some() && keyframe_control.force_supported;
        let mut keyframe_force_enabled = keyframe_force_supported;
        let keyframe_interval_applied = keyframe_control.config_applied || keyframe_force_supported;
        let mut nv12 = vec![0u8; bgra_to_nv12::buffer_size(config.out_width, config.out_height)?];
        let frame_interval = Duration::from_nanos(1_000_000_000u64 / u64::from(config.target_fps));
        let started_at = Instant::now();
        let mut next_tick = started_at;
        let mut report_at = started_at;
        let mut previous_capture = CaptureStats::default();
        let mut previous_pipeline = PipelineCounters::default();
        let mut previous_encoder = EncoderStats::default();
        let mut counters = PipelineCounters::default();
        let mut keyframe_requests = KeyframeRequestTracker::default();
        let mut last_version = 0u64;
        let mut have_nv12 = false;
        let mut effective_bitrate_mbps = config.bitrate_mbps;
        let mut reconfigure_requested = false;

        if config.verbose {
            eprintln!(
                "capture-encode target=primary-monitor source={}x{} output={}x{} input={} encode=NV12 convert_backend={} encoder=\"{}\" requested={} target_fps={} bitrate_mbps={} color_matrix={} range={} d3d_driver={} output_buffer={} profile_main={} encoder_input_metadata={:?} encoder_output_metadata={:?} duration_sec={}",
                capture_info.width,
                capture_info.height,
                config.out_width,
                config.out_height,
                PIXEL_FORMAT_NAME,
                capture.selection.selected_name(),
                encoder.encoder_selection().selected_name,
                config.encoder.name(),
                config.target_fps,
                config.bitrate_mbps,
                config.color_spec.yuv_matrix(),
                config.color_spec.color_range(),
                capture_info.driver_name,
                encoder.output_buffer_size(),
                encoder.profile_main(),
                encoder.input_color_metadata(),
                encoder.output_color_metadata(),
                optional_duration_text(config.duration_sec)
            );
        }
        observer.on_started(&CapturePipelineStarted {
            target_fps: config.target_fps,
            bitrate_mbps: config.bitrate_mbps,
            bitrate_selection: config.bitrate_selection.clone(),
            width: config.out_width,
            height: config.out_height,
            color_spec: config.color_spec,
            conversion_selection: capture.selection.clone(),
            encoder_selection: encoder.encoder_selection().clone(),
            encoder_input_color_metadata: encoder.input_color_metadata(),
            encoder_output_color_metadata: encoder.output_color_metadata(),
            keyframe_interval_applied,
            keyframe_interval_target_frames,
            keyframe_force_supported,
            keyframe_control: keyframe_control.clone(),
        })?;

        while !duration_elapsed(started_at, config.duration_sec)
            && !stop_requested()
            && !observer.stop_requested()
        {
            sleep_until(next_tick);
            if let Some(error) = capture.error() {
                return Err(error);
            }
            counters.encode_ticks += 1;
            if capture.update_nv12(config, &mut nv12, &mut last_version, &mut counters)? {
                have_nv12 = true;
            } else if have_nv12 {
                counters.no_new_frame_reused += 1;
            }
            if have_nv12 {
                if (counters.frames_encoded == 0
                    || keyframe_request_due(
                        counters.frames_encoded,
                        keyframe_interval_target_frames,
                    ))
                    && keyframe_force_enabled
                {
                    counters.keyframe_force_requests += 1;
                    if let Err(err) = encoder.request_keyframe() {
                        counters.keyframe_force_failures += 1;
                        keyframe_force_enabled = false;
                        if config.verbose {
                            eprintln!("encoder forced-IDR request failed: {err}");
                        }
                    } else {
                        keyframe_requests.record_request(counters.frames_encoded);
                    }
                }
                let encode_started = Instant::now();
                if let Err(error) = encoder.encode_nv12(&nv12, counters.frames_encoded) {
                    observer.on_encoder_terminal_stats(encoder.stats());
                    let _ = capture.stop();
                    return Err(error);
                }
                counters.encode_ms_total += encode_started.elapsed().as_secs_f64() * 1000.0;
                counters.frames_encoded += 1;
                emit_encoded_samples(
                    &mut encoder,
                    observer,
                    &mut keyframe_requests,
                    config.target_fps,
                )?;
            } else {
                counters.no_new_frame_skipped += 1;
            }

            let after_work = Instant::now();
            next_tick += frame_interval;
            if after_work > next_tick + frame_interval {
                let lag = after_work.duration_since(next_tick);
                let skipped = (lag.as_nanos() / frame_interval.as_nanos()) as u64 + 1;
                counters.encode_lag_skips += skipped;
                next_tick = after_work + frame_interval;
            }

            if after_work.duration_since(report_at) >= Duration::from_secs(1) {
                let capture_stats = capture.stats();
                let encoder_stats = encoder.stats();
                let stats = make_stats(
                    config,
                    capture_stats,
                    previous_capture,
                    counters,
                    previous_pipeline,
                    encoder_stats,
                    previous_encoder,
                    encoder.input_color_metadata(),
                    encoder.output_color_metadata(),
                    encoder.encoder_selection().clone(),
                    capture.selection.clone(),
                    &keyframe_requests,
                    after_work.duration_since(report_at),
                    effective_bitrate_mbps,
                );
                if let Err(error) = observer.on_stats(&stats) {
                    let capture_error = capture.stop().err();
                    let encoder_error = encoder.finish().err();
                    observer.on_encoder_terminal_stats(encoder.stats());
                    let cleanup_error = [capture_error, encoder_error]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join("; ");
                    return if cleanup_error.is_empty() {
                        Err(error)
                    } else {
                        Err(format!("{error}; pipeline cleanup: {cleanup_error}"))
                    };
                }
                match observer.take_control() {
                    CapturePipelineControl::Continue => {}
                    CapturePipelineControl::UpdateBitrate(requested_mbps) => {
                        let mut idr_requested = false;
                        let result = match encoder.set_mean_bitrate_mbps(requested_mbps) {
                            Ok(()) => {
                                counters.keyframe_force_requests =
                                    counters.keyframe_force_requests.saturating_add(1);
                                match encoder.request_keyframe() {
                                    Ok(()) => {
                                        idr_requested = true;
                                        keyframe_requests.record_request(counters.frames_encoded);
                                        Ok(())
                                    }
                                    Err(error) => {
                                        counters.keyframe_force_failures =
                                            counters.keyframe_force_failures.saturating_add(1);
                                        Err(format!(
                                            "bitrate updated but forced-IDR request failed: {error}"
                                        ))
                                    }
                                }
                            }
                            Err(error) => Err(error),
                        };
                        if result.is_ok() {
                            effective_bitrate_mbps = requested_mbps;
                        }
                        if observer.on_bitrate_update_result(
                            requested_mbps,
                            &result,
                            idr_requested,
                        )? {
                            reconfigure_requested = true;
                            break;
                        }
                    }
                    CapturePipelineControl::Restart => {
                        reconfigure_requested = true;
                        break;
                    }
                }
                previous_capture = capture_stats;
                previous_pipeline = counters;
                previous_encoder = encoder_stats;
                report_at = after_work;
            }
        }

        let capture_stats = capture.stats();
        let conversion_selection = capture.selection.clone();
        capture.stop()?;
        let encoder_stats = match encoder.finish() {
            Ok(stats) => stats,
            Err(error) => {
                observer.on_encoder_terminal_stats(encoder.stats());
                return Err(error);
            }
        };
        emit_encoded_samples(
            &mut encoder,
            observer,
            &mut keyframe_requests,
            config.target_fps,
        )?;
        let wall_time_sec = started_at.elapsed().as_secs_f64();
        let media_duration_sec = encoder_stats.frames_in as f64 / f64::from(config.target_fps);
        Ok(CapturePipelineDone {
            capture_raw_frames: capture_stats.raw_frames,
            capture_latest_updates: capture_stats.latest_updates,
            capture_callback_skipped: capture_stats.callback_skipped,
            capture_dropped: capture_stats.dropped,
            encode_ticks: counters.encode_ticks,
            no_new_frame_skipped: counters.no_new_frame_skipped,
            no_new_frame_reused: counters.no_new_frame_reused,
            frames_encoded: counters.frames_encoded,
            encode_lag_skips: counters.encode_lag_skips,
            encoder: encoder_stats,
            media_duration_sec,
            wall_time_sec,
            processing_fps: encoder_stats.frames_in as f64 / wall_time_sec.max(0.001),
            mbps: encoder_stats.bytes_out as f64 * 8.0
                / media_duration_sec.max(0.001)
                / 1_000_000.0,
            bitrate_mbps: effective_bitrate_mbps,
            bitrate_selection: config.bitrate_selection.clone(),
            width: config.out_width,
            height: config.out_height,
            copy_ms_avg: average_ms(capture_stats.copy_ms_total, capture_stats.latest_updates),
            convert_ms_avg: if conversion_selection.selected == ConvertBackend::D3d11 {
                average_ms(
                    capture_stats.gpu_convert_ms_total,
                    capture_stats.latest_updates,
                )
            } else {
                average_ms(counters.cpu_convert_ms_total, counters.frames_encoded)
            },
            gpu_convert_ms_avg: average_ms(
                capture_stats.gpu_convert_ms_total,
                capture_stats.latest_updates,
            ),
            cpu_convert_ms_avg: average_ms(counters.cpu_convert_ms_total, counters.frames_encoded),
            encode_ms_avg: average_ms(counters.encode_ms_total, counters.frames_encoded),
            stopped_by_console: stop_requested(),
            color_spec: config.color_spec,
            conversion_selection,
            encoder_selection: encoder.encoder_selection().clone(),
            encoder_input_color_metadata: encoder.input_color_metadata(),
            encoder_output_color_metadata: encoder.output_color_metadata(),
            keyframe_force_requests: counters.keyframe_force_requests,
            keyframe_force_failures: counters.keyframe_force_failures,
            keyframe_force_last_requested_frame_id: keyframe_requests.last_requested_frame_id,
            keyframe_force_last_effective_frame_id: keyframe_requests.last_effective_frame_id,
            keyframe_force_latency_frames_avg: keyframe_requests.latency_frames_avg(),
            keyframe_force_latency_frames_max: keyframe_requests.latency_frames_max,
            keyframe_force_request_frame_ids: keyframe_requests.request_frame_ids(),
            keyframe_force_effective_frame_ids: keyframe_requests.effective_frame_ids(),
            keyframe_force_latency_frames: keyframe_requests.latency_frames(),
            reconfigure_requested,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn make_stats(
        config: &CaptureEncodeConfig,
        capture: CaptureStats,
        previous_capture: CaptureStats,
        pipeline: PipelineCounters,
        previous_pipeline: PipelineCounters,
        encoder: EncoderStats,
        previous_encoder: EncoderStats,
        encoder_input_color_metadata: MediaColorMetadata,
        encoder_output_color_metadata: MediaColorMetadata,
        encoder_selection: EncoderSelection,
        conversion_selection: ConversionSelection,
        keyframe_requests: &KeyframeRequestTracker,
        elapsed: Duration,
        effective_bitrate_mbps: f64,
    ) -> CapturePipelineStats {
        let elapsed_sec = elapsed.as_secs_f64().max(0.001);
        CapturePipelineStats {
            capture_raw_frames: capture.raw_frames,
            capture_latest_updates: capture.latest_updates,
            capture_callback_skipped: capture.callback_skipped,
            capture_dropped: capture.dropped,
            encode_ticks: pipeline.encode_ticks,
            no_new_frame_skipped: pipeline.no_new_frame_skipped,
            no_new_frame_reused: pipeline.no_new_frame_reused,
            frames_encoded: pipeline.frames_encoded,
            encode_lag_skips: pipeline.encode_lag_skips,
            keyframe_force_requests: pipeline.keyframe_force_requests,
            keyframe_force_failures: pipeline.keyframe_force_failures,
            keyframe_force_last_requested_frame_id: keyframe_requests.last_requested_frame_id,
            keyframe_force_last_effective_frame_id: keyframe_requests.last_effective_frame_id,
            keyframe_force_latency_frames_avg: keyframe_requests.latency_frames_avg(),
            keyframe_force_latency_frames_max: keyframe_requests.latency_frames_max,
            keyframe_force_request_frame_ids: keyframe_requests.request_frame_ids(),
            keyframe_force_effective_frame_ids: keyframe_requests.effective_frame_ids(),
            keyframe_force_latency_frames: keyframe_requests.latency_frames(),
            samples_out: encoder.samples_out,
            bytes_out: encoder.bytes_out,
            raw_fps: capture
                .raw_frames
                .saturating_sub(previous_capture.raw_frames) as f64
                / elapsed_sec,
            accepted_fps: pipeline
                .frames_encoded
                .saturating_sub(previous_pipeline.frames_encoded) as f64
                / elapsed_sec,
            encode_fps: encoder.frames_in.saturating_sub(previous_encoder.frames_in) as f64
                / elapsed_sec,
            target_fps: config.target_fps,
            mbps: encoder.bytes_out.saturating_sub(previous_encoder.bytes_out) as f64 * 8.0
                / elapsed_sec
                / 1_000_000.0,
            target_bitrate_mbps: effective_bitrate_mbps,
            bitrate_selection: config.bitrate_selection.clone(),
            width: config.out_width,
            height: config.out_height,
            copy_ms_avg: average_ms(capture.copy_ms_total, capture.latest_updates),
            convert_ms_avg: if conversion_selection.selected == ConvertBackend::D3d11 {
                average_ms(capture.gpu_convert_ms_total, capture.latest_updates)
            } else {
                average_ms(pipeline.cpu_convert_ms_total, pipeline.frames_encoded)
            },
            gpu_convert_ms_avg: average_ms(capture.gpu_convert_ms_total, capture.latest_updates),
            cpu_convert_ms_avg: average_ms(pipeline.cpu_convert_ms_total, pipeline.frames_encoded),
            encode_ms_avg: average_ms(pipeline.encode_ms_total, pipeline.frames_encoded),
            qsv_async_wait_timeouts: encoder.async_wait_timeouts,
            qsv_async_wait_cancelled: encoder.async_wait_cancelled,
            qsv_drain_timeouts: encoder.async_drain_timeouts,
            conversion_selection,
            color_spec: config.color_spec,
            encoder_selection,
            encoder_input_color_metadata,
            encoder_output_color_metadata,
        }
    }

    fn emit_encoded_samples(
        encoder: &mut WmfH264Encoder,
        observer: &mut dyn CaptureEncodeObserver,
        keyframe_requests: &mut KeyframeRequestTracker,
        fps: u32,
    ) -> Result<(), String> {
        for sample in encoder.take_encoded_samples() {
            keyframe_requests.observe_sample(&sample, fps);
            observer.on_sample(sample)?;
        }
        Ok(())
    }

    fn validate_config(config: &CaptureEncodeConfig) -> Result<(), String> {
        validate_stream_config(config)?;
        if config.output.trim().is_empty() {
            return Err("output path must not be empty".to_string());
        }
        Ok(())
    }

    fn validate_stream_config(config: &CaptureEncodeConfig) -> Result<(), String> {
        if config.duration_sec == Some(0) || config.target_fps == 0 {
            return Err(
                "duration-sec, when provided, and target-fps must be greater than zero".to_string(),
            );
        }
        if !config.bitrate_mbps.is_finite() || config.bitrate_mbps <= 0.0 {
            return Err("bitrate-mbps must be greater than zero".to_string());
        }
        if config.out_width == 0
            || config.out_height == 0
            || config.out_width % 2 != 0
            || config.out_height % 2 != 0
        {
            return Err("output width and height must be non-zero even values".to_string());
        }
        Ok(())
    }

    pub fn run_keyframe_schedule_self_test() -> Result<(), String> {
        let request_ids = (0..=600u64)
            .filter(|frame_id| keyframe_request_due(*frame_id, Some(60)))
            .collect::<Vec<_>>();
        let expected = (1..=10u64).map(|step| step * 60).collect::<Vec<_>>();
        if request_ids != expected {
            return Err(format!(
                "60 FPS keyframe request schedule mismatch: {request_ids:?}"
            ));
        }
        for damaged_frame_id in 0..600u64 {
            let next_periodic_idr = (damaged_frame_id / 60 + 1) * 60;
            if next_periodic_idr.saturating_sub(damaged_frame_id) > 60 {
                return Err("periodic IDR recovery exceeded 60 frames".to_string());
            }
        }

        let mut tracker = KeyframeRequestTracker::default();
        tracker.record_request(60);
        tracker.observe_sample(
            &EncodedSample {
                bytes: vec![0, 0, 0, 1, 5, 0x80],
                keyframe: Some(true),
                sample_time_hns: Some(61 * 10_000_000 / 60),
            },
            60,
        );
        if tracker.request_frame_ids() != vec![60]
            || tracker.effective_frame_ids() != vec![61]
            || tracker.latency_frames() != vec![1]
            || tracker.latency_frames_avg() != 1.0
            || tracker.latency_frames_max != 1
        {
            return Err("forced-IDR request/effective latency tracking failed".to_string());
        }
        Ok(())
    }

    fn duration_elapsed(started_at: Instant, duration_sec: Option<u64>) -> bool {
        duration_sec
            .map(|seconds| started_at.elapsed() >= Duration::from_secs(seconds))
            .unwrap_or(false)
    }

    fn optional_duration_text(duration_sec: Option<u64>) -> String {
        duration_sec.map_or_else(|| "unlimited".to_string(), |seconds| seconds.to_string())
    }

    fn sleep_until(target: Instant) {
        loop {
            if stop_requested() {
                return;
            }
            let now = Instant::now();
            if now >= target {
                return;
            }
            thread::sleep((target - now).min(Duration::from_millis(2)));
        }
    }

    fn average_ms(total: f64, count: u64) -> f64 {
        if count == 0 {
            0.0
        } else {
            total / count as f64
        }
    }

    fn keyframes_json(stats: EncoderStats) -> String {
        if stats.keyframe_detection_available {
            stats.keyframes.to_string()
        } else {
            "null".to_string()
        }
    }

    fn json_escape(text: &str) -> String {
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\r', "\\r")
            .replace('\n', "\\n")
    }

    fn optional_json_string(value: Option<&str>) -> String {
        value.map_or_else(
            || "null".to_string(),
            |value| format!(r#""{}""#, json_escape(value)),
        )
    }
}

#[cfg(windows)]
pub use platform::{
    install_console_ctrl_guard, prepare_pipeline, run, run_keyframe_schedule_self_test,
    run_prepared_with_observer, run_with_observer, CaptureEncodeObserver, CapturePipelineControl,
    CapturePipelineDone, CapturePipelineStarted, CapturePipelineStats, ConversionSelection,
    PreparedCapturePipeline,
};

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn deterministic_keyframe_schedule() {
        super::run_keyframe_schedule_self_test().expect("keyframe schedule self-test");
    }
}

#[cfg(not(windows))]
pub fn run(_config: CaptureEncodeConfig) -> Result<(), String> {
    Err("capture-encode-probe is only supported on Windows".to_string())
}
