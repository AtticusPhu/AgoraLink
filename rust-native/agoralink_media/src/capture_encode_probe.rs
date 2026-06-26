#[derive(Debug)]
pub struct CaptureEncodeConfig {
    pub duration_sec: Option<u64>,
    pub target_fps: u32,
    pub bitrate_mbps: f64,
    pub out_width: u32,
    pub out_height: u32,
    pub output: String,
    pub color_spec: crate::color_spec::ColorSpec,
}

#[cfg(windows)]
mod platform {
    use std::fs::File;
    use std::io::{self, Write};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use windows::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
    };

    use super::CaptureEncodeConfig;
    use crate::bgra_to_nv12;
    use crate::color_spec::{ColorSpec, MediaColorMetadata};
    use crate::wgc_latest_capture::{LatestCapture, LatestCaptureStats};
    use crate::wmf_h264_encoder::{EncodedSample, EncoderStats, WmfH264Encoder, ENCODER_NAME};

    const PIXEL_FORMAT_NAME: &str = "B8G8R8A8";
    static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

    #[derive(Clone, Copy, Debug)]
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
        pub samples_out: u64,
        pub bytes_out: u64,
        pub raw_fps: f64,
        pub accepted_fps: f64,
        pub encode_fps: f64,
        pub target_fps: u32,
        pub mbps: f64,
        pub width: u32,
        pub height: u32,
        pub copy_ms_avg: f64,
        pub convert_ms_avg: f64,
        pub encode_ms_avg: f64,
        pub color_spec: ColorSpec,
        pub encoder_input_color_metadata: MediaColorMetadata,
        pub encoder_output_color_metadata: MediaColorMetadata,
    }

    #[derive(Clone, Copy, Debug)]
    pub struct CapturePipelineStarted {
        pub target_fps: u32,
        pub bitrate_mbps: f64,
        pub width: u32,
        pub height: u32,
        pub color_spec: ColorSpec,
        pub encoder_input_color_metadata: MediaColorMetadata,
        pub encoder_output_color_metadata: MediaColorMetadata,
    }

    #[derive(Clone, Copy, Debug)]
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
        pub width: u32,
        pub height: u32,
        pub copy_ms_avg: f64,
        pub convert_ms_avg: f64,
        pub encode_ms_avg: f64,
        pub stopped_by_console: bool,
        pub color_spec: ColorSpec,
        pub encoder_input_color_metadata: MediaColorMetadata,
        pub encoder_output_color_metadata: MediaColorMetadata,
    }

    pub trait CaptureEncodeObserver {
        fn on_started(&mut self, _started: &CapturePipelineStarted) -> Result<(), String> {
            Ok(())
        }

        fn on_sample(&mut self, sample: EncodedSample) -> Result<(), String>;
        fn on_stats(&mut self, stats: &CapturePipelineStats) -> Result<(), String>;
    }

    #[derive(Clone, Copy, Default)]
    struct PipelineCounters {
        encode_ticks: u64,
        no_new_frame_skipped: u64,
        no_new_frame_reused: u64,
        frames_encoded: u64,
        encode_lag_skips: u64,
        convert_ms_total: f64,
        encode_ms_total: f64,
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
                r#"{{"type":"CAPTURE_ENCODE_STATS","mode":"capture_encode_probe","capture_raw_frames":{},"capture_latest_updates":{},"capture_callback_skipped":{},"capture_dropped":{},"encode_ticks":{},"no_new_frame_skipped":{},"no_new_frame_reused":{},"frames_encoded":{},"encode_lag_skips":{},"samples_out":{},"bytes_out":{},"raw_fps":{:.2},"accepted_fps":{:.2},"encode_fps":{:.2},"target_fps":{},"mbps":{:.3},"width":{},"height":{},"format_in":"{}","format_encode":"NV12","copy_ms_avg":{:.3},"convert_ms_avg":{:.3},"encode_ms_avg":{:.3},{},{},{}}}"#,
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
                stats.width,
                stats.height,
                PIXEL_FORMAT_NAME,
                stats.copy_ms_avg,
                stats.convert_ms_avg,
                stats.encode_ms_avg,
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

    struct ConsoleCtrlGuard;

    impl ConsoleCtrlGuard {
        fn install() -> Result<Self, String> {
            STOP_REQUESTED.store(false, Ordering::SeqCst);
            unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), true) }
                .map_err(|err| format!("SetConsoleCtrlHandler failed: {err}"))?;
            Ok(Self)
        }
    }

    impl Drop for ConsoleCtrlGuard {
        fn drop(&mut self) {
            let _ = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), false) };
        }
    }

    unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> windows::core::BOOL {
        if matches!(
            ctrl_type,
            CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT
        ) {
            STOP_REQUESTED.store(true, Ordering::SeqCst);
            true.into()
        } else {
            false.into()
        }
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
            r#"{{"type":"CAPTURE_ENCODE_DONE","encoder":"{}","capture_raw_frames":{},"capture_latest_updates":{},"capture_callback_skipped":{},"capture_dropped":{},"encode_ticks":{},"no_new_frame_skipped":{},"no_new_frame_reused":{},"frames_encoded":{},"encode_lag_skips":{},"samples_out":{},"bytes_out":{},"duration_sec":{:.3},"wall_time_sec":{:.3},"fps":{},"processing_fps":{:.2},"mbps":{:.3},"keyframes":{},"width":{},"height":{},"copy_ms_avg":{:.3},"convert_ms_avg":{:.3},"encode_ms_avg":{:.3},"output":"{}",{},{},{}}}"#,
            ENCODER_NAME,
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
            keyframes_json(done.encoder),
            done.width,
            done.height,
            done.copy_ms_avg,
            done.convert_ms_avg,
            done.encode_ms_avg,
            json_escape(&config.output),
            done.color_spec.json_fragment(),
            done.encoder_input_color_metadata
                .json_fragment("encoder_input"),
            done.encoder_output_color_metadata
                .json_fragment("encoder_output")
        );
        io::stdout().flush().ok();
        eprintln!(
            "capture-encode-probe stopped reason={}",
            if done.stopped_by_console {
                "console-control"
            } else {
                "duration-complete"
            }
        );
        Ok(())
    }

    pub fn run_with_observer(
        config: &CaptureEncodeConfig,
        observer: &mut dyn CaptureEncodeObserver,
    ) -> Result<CapturePipelineDone, String> {
        validate_stream_config(config)?;
        let _console_ctrl = ConsoleCtrlGuard::install()?;
        let capture = LatestCapture::start()?;
        let capture_info = capture.info();
        let mut encoder = WmfH264Encoder::new_stream_with_color(
            config.out_width,
            config.out_height,
            config.target_fps,
            config.bitrate_mbps,
            config.color_spec,
        )?;
        let mut nv12 = vec![0u8; bgra_to_nv12::buffer_size(config.out_width, config.out_height)?];
        let frame_interval = Duration::from_nanos(1_000_000_000u64 / u64::from(config.target_fps));
        let started_at = Instant::now();
        let mut next_tick = started_at;
        let mut report_at = started_at;
        let mut previous_capture = LatestCaptureStats::default();
        let mut previous_pipeline = PipelineCounters::default();
        let mut previous_encoder = EncoderStats::default();
        let mut counters = PipelineCounters::default();
        let mut last_version = 0u64;
        let mut have_nv12 = false;

        eprintln!(
            "capture-encode target=primary-monitor source={}x{} output={}x{} input={} encode=NV12 encoder=\"{}\" target_fps={} bitrate_mbps={} color_matrix={} range={} d3d_driver={} output_buffer={} profile_main={} encoder_input_metadata={:?} encoder_output_metadata={:?} duration_sec={}",
            capture_info.width,
            capture_info.height,
            config.out_width,
            config.out_height,
            PIXEL_FORMAT_NAME,
            ENCODER_NAME,
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
        observer.on_started(&CapturePipelineStarted {
            target_fps: config.target_fps,
            bitrate_mbps: config.bitrate_mbps,
            width: config.out_width,
            height: config.out_height,
            color_spec: config.color_spec,
            encoder_input_color_metadata: encoder.input_color_metadata(),
            encoder_output_color_metadata: encoder.output_color_metadata(),
        })?;

        while !duration_elapsed(started_at, config.duration_sec)
            && !STOP_REQUESTED.load(Ordering::SeqCst)
        {
            sleep_until(next_tick);
            if let Some(error) = capture.error() {
                return Err(error);
            }
            counters.encode_ticks += 1;
            if let Some(frame) = capture.latest() {
                if frame.version != last_version {
                    let convert_started = Instant::now();
                    bgra_to_nv12::convert_scaled_with_spec(
                        &frame.bgra,
                        frame.row_pitch,
                        frame.width,
                        frame.height,
                        config.out_width,
                        config.out_height,
                        &mut nv12,
                        config.color_spec,
                    )?;
                    counters.convert_ms_total += convert_started.elapsed().as_secs_f64() * 1000.0;
                    last_version = frame.version;
                    have_nv12 = true;
                } else if have_nv12 {
                    counters.no_new_frame_reused += 1;
                }
            }
            if have_nv12 {
                let encode_started = Instant::now();
                encoder.encode_nv12(&nv12, counters.frames_encoded)?;
                counters.encode_ms_total += encode_started.elapsed().as_secs_f64() * 1000.0;
                counters.frames_encoded += 1;
                emit_encoded_samples(&mut encoder, observer)?;
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
                    after_work.duration_since(report_at),
                );
                observer.on_stats(&stats)?;
                previous_capture = capture_stats;
                previous_pipeline = counters;
                previous_encoder = encoder_stats;
                report_at = after_work;
            }
        }

        let capture_stats = capture.stats();
        capture.stop()?;
        let encoder_stats = encoder.finish()?;
        emit_encoded_samples(&mut encoder, observer)?;
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
            width: config.out_width,
            height: config.out_height,
            copy_ms_avg: average_ms(capture_stats.copy_ms_total, capture_stats.latest_updates),
            convert_ms_avg: average_ms(counters.convert_ms_total, counters.frames_encoded),
            encode_ms_avg: average_ms(counters.encode_ms_total, counters.frames_encoded),
            stopped_by_console: STOP_REQUESTED.load(Ordering::SeqCst),
            color_spec: config.color_spec,
            encoder_input_color_metadata: encoder.input_color_metadata(),
            encoder_output_color_metadata: encoder.output_color_metadata(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn make_stats(
        config: &CaptureEncodeConfig,
        capture: LatestCaptureStats,
        previous_capture: LatestCaptureStats,
        pipeline: PipelineCounters,
        previous_pipeline: PipelineCounters,
        encoder: EncoderStats,
        previous_encoder: EncoderStats,
        encoder_input_color_metadata: MediaColorMetadata,
        encoder_output_color_metadata: MediaColorMetadata,
        elapsed: Duration,
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
            width: config.out_width,
            height: config.out_height,
            copy_ms_avg: average_ms(capture.copy_ms_total, capture.latest_updates),
            convert_ms_avg: average_ms(pipeline.convert_ms_total, pipeline.frames_encoded),
            encode_ms_avg: average_ms(pipeline.encode_ms_total, pipeline.frames_encoded),
            color_spec: config.color_spec,
            encoder_input_color_metadata,
            encoder_output_color_metadata,
        }
    }

    fn emit_encoded_samples(
        encoder: &mut WmfH264Encoder,
        observer: &mut dyn CaptureEncodeObserver,
    ) -> Result<(), String> {
        for sample in encoder.take_encoded_samples() {
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
            if STOP_REQUESTED.load(Ordering::SeqCst) {
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
}

#[cfg(windows)]
pub use platform::{
    run, run_with_observer, CaptureEncodeObserver, CapturePipelineDone, CapturePipelineStarted,
    CapturePipelineStats,
};

#[cfg(not(windows))]
pub fn run(_config: CaptureEncodeConfig) -> Result<(), String> {
    Err("capture-encode-probe is only supported on Windows".to_string())
}
