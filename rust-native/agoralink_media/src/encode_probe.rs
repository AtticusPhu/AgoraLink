#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncoderChoice {
    Auto,
    Software,
    Hardware,
}

#[derive(Debug)]
pub struct EncodeProbeConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub duration_sec: u64,
    pub bitrate_mbps: f64,
    pub output: String,
    pub encoder: EncoderChoice,
    pub color_spec: crate::color_spec::ColorSpec,
}

#[cfg(windows)]
mod platform {
    use std::io::{self, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use windows::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
    };

    use super::{EncodeProbeConfig, EncoderChoice};
    use crate::nv12_synthetic;
    use crate::wmf_h264_encoder::{EncoderStats, WmfH264Encoder, ENCODER_NAME};

    static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

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

    pub fn run(config: EncodeProbeConfig) -> Result<(), String> {
        validate_config(&config)?;
        if config.encoder == EncoderChoice::Hardware {
            return Err(
                "hardware encoder is not implemented in stage 3A; use --encoder software or auto"
                    .to_string(),
            );
        }
        let _console_ctrl = ConsoleCtrlGuard::install()?;
        let frame_size = nv12_synthetic::buffer_size(config.width, config.height)?;
        let mut encoder = WmfH264Encoder::new_with_color(
            config.width,
            config.height,
            config.fps,
            config.bitrate_mbps,
            &config.output,
            config.color_spec,
        )?;
        eprintln!(
            "encode-probe encoder=\"{}\" input=NV12 output=H264 size={}x{} fps={} bitrate_mbps={} color_matrix={} range={} output_buffer={} profile_main={} encoder_input_metadata={:?} encoder_output_metadata={:?}",
            ENCODER_NAME,
            config.width,
            config.height,
            config.fps,
            config.bitrate_mbps,
            config.color_spec.yuv_matrix(),
            config.color_spec.color_range(),
            encoder.output_buffer_size(),
            encoder.profile_main(),
            encoder.input_color_metadata(),
            encoder.output_color_metadata()
        );

        let total_frames = u64::from(config.fps)
            .checked_mul(config.duration_sec)
            .ok_or_else(|| "frame count overflow".to_string())?;
        let frame_interval = Duration::from_nanos(1_000_000_000 / u64::from(config.fps));
        let mut nv12 = vec![0u8; frame_size];
        let started_at = Instant::now();
        let mut next_frame_at = started_at;
        let mut report_at = started_at;
        let mut previous = EncoderStats::default();

        for frame_index in 0..total_frames {
            if STOP_REQUESTED.load(Ordering::SeqCst) {
                break;
            }
            sleep_until(next_frame_at);
            nv12_synthetic::fill_frame(&mut nv12, config.width, config.height, frame_index)?;
            encoder.encode_nv12(&nv12, frame_index)?;

            let now = Instant::now();
            if now.duration_since(report_at) >= Duration::from_secs(1) {
                let current = encoder.stats();
                print_stats(&config, current, previous, now.duration_since(report_at));
                previous = current;
                report_at = now;
            }
            next_frame_at += frame_interval;
        }

        let stats = encoder.finish()?;
        let wall_time_sec = started_at.elapsed().as_secs_f64();
        let media_duration_sec = stats.frames_in as f64 / f64::from(config.fps);
        println!(
            r#"{{"type":"ENCODE_DONE","encoder":"{}","frames_in":{},"samples_out":{},"bytes_out":{},"duration_sec":{:.3},"wall_time_sec":{:.3},"fps":{},"processing_fps":{:.2},"mbps":{:.3},"keyframes":{},"width":{},"height":{},"output":"{}",{},{},{}}}"#,
            ENCODER_NAME,
            stats.frames_in,
            stats.samples_out,
            stats.bytes_out,
            media_duration_sec,
            wall_time_sec,
            config.fps,
            stats.frames_in as f64 / wall_time_sec.max(0.001),
            stats.bytes_out as f64 * 8.0 / media_duration_sec.max(0.001) / 1_000_000.0,
            keyframes_json(stats),
            config.width,
            config.height,
            json_escape(&config.output),
            config.color_spec.json_fragment(),
            encoder
                .input_color_metadata()
                .json_fragment("encoder_input"),
            encoder
                .output_color_metadata()
                .json_fragment("encoder_output")
        );
        io::stdout().flush().ok();
        eprintln!(
            "encode-probe stopped reason={}",
            if STOP_REQUESTED.load(Ordering::SeqCst) {
                "console-control"
            } else {
                "duration-complete"
            }
        );
        Ok(())
    }

    fn validate_config(config: &EncodeProbeConfig) -> Result<(), String> {
        if config.width == 0
            || config.height == 0
            || config.width % 2 != 0
            || config.height % 2 != 0
        {
            return Err("width and height must be non-zero even values".to_string());
        }
        if config.fps == 0 || config.duration_sec == 0 {
            return Err("fps and duration-sec must be greater than zero".to_string());
        }
        if config.output.trim().is_empty() {
            return Err("output path must not be empty".to_string());
        }
        Ok(())
    }

    fn sleep_until(target: Instant) {
        let now = Instant::now();
        if target > now {
            thread::sleep(target.duration_since(now));
        }
    }

    fn print_stats(
        config: &EncodeProbeConfig,
        current: EncoderStats,
        previous: EncoderStats,
        elapsed: Duration,
    ) {
        let elapsed_sec = elapsed.as_secs_f64().max(0.001);
        let frame_delta = current.frames_in.saturating_sub(previous.frames_in);
        let media_elapsed_sec = frame_delta as f64 / f64::from(config.fps);
        let mbps = current.bytes_out.saturating_sub(previous.bytes_out) as f64 * 8.0
            / media_elapsed_sec.max(0.001)
            / 1_000_000.0;
        println!(
            r#"{{"type":"ENCODE_STATS","mode":"encode_probe","encoder":"{}","frames_in":{},"samples_out":{},"bytes_out":{},"mbps":{:.3},"fps":{},"processing_fps":{:.2},"keyframes":{},"width":{},"height":{},{} }}"#,
            ENCODER_NAME,
            current.frames_in,
            current.samples_out,
            current.bytes_out,
            mbps,
            config.fps,
            frame_delta as f64 / elapsed_sec,
            keyframes_json(current),
            config.width,
            config.height,
            config.color_spec.json_fragment()
        );
        io::stdout().flush().ok();
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
pub use platform::run;

#[cfg(not(windows))]
pub fn run(_config: EncodeProbeConfig) -> Result<(), String> {
    Err("encode-probe is only supported on Windows".to_string())
}
