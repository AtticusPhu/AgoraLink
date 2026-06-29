#[derive(Debug)]
pub struct GpuConvertProbeConfig {
    pub duration_sec: u64,
    pub target_fps: u32,
    pub out_width: u32,
    pub out_height: u32,
    pub color_spec: crate::color_spec::ColorSpec,
    pub debug_dump_nv12: Option<String>,
    pub debug_dump_bgra: Option<String>,
    pub debug_dump_limit: usize,
}

#[cfg(windows)]
mod platform {
    use std::fs;
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::GpuConvertProbeConfig;
    use crate::bgra_to_nv12;
    use crate::gpu_nv12_capture::{
        CapturedNv12Frame, GpuCaptureStats, GpuNv12Capture, Nv12ReadbackLayout,
    };
    use crate::json_escape;
    use crate::wgc_latest_capture::{LatestCapture, LatestCaptureStats};

    pub fn run(config: GpuConvertProbeConfig) -> Result<(), String> {
        validate_config(&config)?;
        match GpuNv12Capture::start(
            config.out_width,
            config.out_height,
            config.target_fps,
            config.color_spec,
        ) {
            Ok(capture) => run_gpu(config, capture),
            Err(gpu_error) => run_cpu_fallback(config, gpu_error),
        }
    }

    fn run_gpu(config: GpuConvertProbeConfig, capture: GpuNv12Capture) -> Result<(), String> {
        let info = capture.info();
        let started_at = Instant::now();
        let mut report_at = started_at;
        let mut previous = GpuCaptureStats::default();
        let mut last_dumped_version = 0u64;
        let mut dumped = 0usize;

        while started_at.elapsed() < Duration::from_secs(config.duration_sec) {
            if let Some(error) = capture.error() {
                return Err(error);
            }
            if dumped < config.debug_dump_limit {
                if let Some(frame) = capture.latest() {
                    if frame.version != last_dumped_version {
                        dump_frame(&config, &frame, dumped + 1)?;
                        last_dumped_version = frame.version;
                        dumped += 1;
                    }
                }
            }
            thread::sleep(Duration::from_millis(10));
            let now = Instant::now();
            if now.duration_since(report_at) >= Duration::from_secs(1) {
                let stats = capture.stats();
                print_stats(&config, &stats, &previous, now.duration_since(report_at));
                previous = stats;
                report_at = now;
            }
        }

        let stats = capture.stats();
        capture.stop()?;
        println!(
            r#"{{"type":"GPU_CONVERT_DONE","conversion_backend":"d3d11-video-processor","raw_frames":{},"frames_converted":{},"pacing_skipped":{},"dropped":{},"duration_sec":{:.3},"gpu_convert_ms_avg":{:.3},"copy_ms_avg":{:.3},"cpu_convert_ms_avg":0.0,"width":{},"height":{},"source_width":{},"source_height":{},"driver":"{}","color_space_api":"{}","fallback_reason":null,{}}}"#,
            stats.raw_frames,
            stats.latest_updates,
            stats.pacing_skipped,
            stats.dropped,
            started_at.elapsed().as_secs_f64(),
            average_ms(stats.gpu_convert_ms_total, stats.latest_updates),
            average_ms(stats.copy_ms_total, stats.latest_updates),
            info.output_width,
            info.output_height,
            info.source_width,
            info.source_height,
            info.driver_name,
            info.color_space_api,
            config.color_spec.json_fragment(),
        );
        io::stdout().flush().ok();
        Ok(())
    }

    fn run_cpu_fallback(
        config: GpuConvertProbeConfig,
        fallback_reason: String,
    ) -> Result<(), String> {
        let capture = LatestCapture::start()?;
        let info = capture.info();
        let started_at = Instant::now();
        let frame_interval = Duration::from_nanos(1_000_000_000u64 / u64::from(config.target_fps));
        let mut next_tick = started_at;
        let mut report_at = started_at;
        let mut previous_capture = LatestCaptureStats::default();
        let mut last_version = 0u64;
        let mut converted = 0u64;
        let mut previous_converted = 0u64;
        let mut cpu_convert_ms_total = 0.0f64;
        let mut previous_cpu_ms = 0.0f64;
        let mut nv12 = vec![0u8; bgra_to_nv12::buffer_size(config.out_width, config.out_height)?];
        let mut dumped = 0usize;

        while started_at.elapsed() < Duration::from_secs(config.duration_sec) {
            sleep_until(next_tick);
            if let Some(error) = capture.error() {
                return Err(error);
            }
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
                    let cpu_ms = convert_started.elapsed().as_secs_f64() * 1000.0;
                    cpu_convert_ms_total += cpu_ms;
                    converted += 1;
                    last_version = frame.version;
                    if dumped < config.debug_dump_limit {
                        let debug_frame = CapturedNv12Frame {
                            version: frame.version,
                            nv12: nv12.clone(),
                            layout: Nv12ReadbackLayout {
                                y_stride: config.out_width as usize,
                                uv_stride: config.out_width as usize,
                                uv_offset: config.out_width as usize * config.out_height as usize,
                                allocated_height: config.out_height,
                                visible_width: config.out_width,
                                visible_height: config.out_height,
                            },
                            gpu_convert_ms: 0.0,
                            copy_ms: 0.0,
                        };
                        dump_frame(&config, &debug_frame, dumped + 1)?;
                        dumped += 1;
                    }
                }
            }
            let now = Instant::now();
            next_tick += frame_interval;
            if now > next_tick + frame_interval {
                next_tick = now + frame_interval;
            }
            if now.duration_since(report_at) >= Duration::from_secs(1) {
                let stats = capture.stats();
                let elapsed = now.duration_since(report_at).as_secs_f64().max(0.001);
                let converted_delta = converted.saturating_sub(previous_converted);
                println!(
                    r#"{{"type":"GPU_CONVERT_STATS","mode":"gpu_convert_probe","conversion_backend":"cpu","raw_frames":{},"frames_converted":{},"pacing_skipped":0,"dropped":{},"raw_fps":{:.2},"converted_fps":{:.2},"target_fps":{},"gpu_convert_ms_avg":0.0,"copy_ms_avg":{:.3},"cpu_convert_ms_avg":{:.3},"width":{},"height":{},"fallback_reason":"{}",{}}}"#,
                    stats.raw_frames,
                    converted,
                    stats.dropped,
                    stats.raw_frames.saturating_sub(previous_capture.raw_frames) as f64 / elapsed,
                    converted_delta as f64 / elapsed,
                    config.target_fps,
                    average_ms(
                        stats.copy_ms_total - previous_capture.copy_ms_total,
                        stats
                            .latest_updates
                            .saturating_sub(previous_capture.latest_updates)
                    ),
                    average_delta(cpu_convert_ms_total, previous_cpu_ms, converted_delta),
                    config.out_width,
                    config.out_height,
                    json_escape(&fallback_reason),
                    config.color_spec.json_fragment(),
                );
                io::stdout().flush().ok();
                previous_capture = stats;
                previous_converted = converted;
                previous_cpu_ms = cpu_convert_ms_total;
                report_at = now;
            }
        }

        let stats = capture.stats();
        capture.stop()?;
        println!(
            r#"{{"type":"GPU_CONVERT_DONE","conversion_backend":"cpu","raw_frames":{},"frames_converted":{},"pacing_skipped":0,"dropped":{},"duration_sec":{:.3},"gpu_convert_ms_avg":0.0,"copy_ms_avg":{:.3},"cpu_convert_ms_avg":{:.3},"width":{},"height":{},"source_width":{},"source_height":{},"driver":"{}","color_space_api":null,"fallback_reason":"{}",{}}}"#,
            stats.raw_frames,
            converted,
            stats.dropped,
            started_at.elapsed().as_secs_f64(),
            average_ms(stats.copy_ms_total, stats.latest_updates),
            average_ms(cpu_convert_ms_total, converted),
            config.out_width,
            config.out_height,
            info.width,
            info.height,
            info.driver_name,
            json_escape(&fallback_reason),
            config.color_spec.json_fragment(),
        );
        io::stdout().flush().ok();
        Ok(())
    }

    fn sleep_until(target: Instant) {
        loop {
            let now = Instant::now();
            if now >= target {
                return;
            }
            thread::sleep((target - now).min(Duration::from_millis(2)));
        }
    }

    fn print_stats(
        config: &GpuConvertProbeConfig,
        stats: &GpuCaptureStats,
        previous: &GpuCaptureStats,
        elapsed: Duration,
    ) {
        let elapsed_sec = elapsed.as_secs_f64().max(0.001);
        let converted = stats.latest_updates.saturating_sub(previous.latest_updates);
        println!(
            r#"{{"type":"GPU_CONVERT_STATS","mode":"gpu_convert_probe","conversion_backend":"d3d11-video-processor","raw_frames":{},"frames_converted":{},"pacing_skipped":{},"dropped":{},"raw_fps":{:.2},"converted_fps":{:.2},"target_fps":{},"gpu_convert_ms_avg":{:.3},"copy_ms_avg":{:.3},"cpu_convert_ms_avg":0.0,"width":{},"height":{},"fallback_reason":null,{}}}"#,
            stats.raw_frames,
            stats.latest_updates,
            stats.pacing_skipped,
            stats.dropped,
            stats.raw_frames.saturating_sub(previous.raw_frames) as f64 / elapsed_sec,
            converted as f64 / elapsed_sec,
            config.target_fps,
            average_delta(
                stats.gpu_convert_ms_total,
                previous.gpu_convert_ms_total,
                converted,
            ),
            average_delta(stats.copy_ms_total, previous.copy_ms_total, converted),
            config.out_width,
            config.out_height,
            config.color_spec.json_fragment(),
        );
        io::stdout().flush().ok();
    }

    fn dump_frame(
        config: &GpuConvertProbeConfig,
        frame: &CapturedNv12Frame,
        index: usize,
    ) -> Result<(), String> {
        if let Some(directory) = config.debug_dump_nv12.as_deref() {
            let directory = prepare_directory(directory)?;
            fs::write(
                directory.join(format!("frame_{index:06}.nv12")),
                &frame.nv12,
            )
            .map_err(|err| format!("write NV12 debug frame failed: {err}"))?;
            let metadata = format!(
                r#"{{"stored_layout":"tight","mapped_y_stride":{},"mapped_uv_stride":{},"mapped_uv_offset":{},"allocated_height":{},"visible_width":{},"visible_height":{},"gpu_convert_ms":{:.3},"copy_ms":{:.3},{}}}"#,
                frame.layout.y_stride,
                frame.layout.uv_stride,
                frame.layout.uv_offset,
                frame.layout.allocated_height,
                frame.layout.visible_width,
                frame.layout.visible_height,
                frame.gpu_convert_ms,
                frame.copy_ms,
                config.color_spec.json_fragment(),
            );
            fs::write(directory.join(format!("frame_{index:06}.json")), metadata)
                .map_err(|err| format!("write NV12 debug metadata failed: {err}"))?;
        }
        if let Some(directory) = config.debug_dump_bgra.as_deref() {
            let directory = prepare_directory(directory)?;
            let mut bgra = Vec::new();
            crate::nv12_to_bgra::convert_with_layout_and_spec(
                &frame.nv12,
                config.out_width,
                config.out_height,
                config.out_width as usize,
                config.out_width as usize,
                config.out_width as usize * config.out_height as usize,
                &mut bgra,
                config.color_spec,
            )?;
            fs::write(directory.join(format!("frame_{index:06}.bgra")), bgra)
                .map_err(|err| format!("write BGRA debug frame failed: {err}"))?;
            let metadata = format!(
                r#"{{"source":"nv12-roundtrip","bgra_stride":{},"visible_width":{},"visible_height":{},{} }}"#,
                config.out_width as usize * 4,
                config.out_width,
                config.out_height,
                config.color_spec.json_fragment(),
            );
            fs::write(directory.join(format!("frame_{index:06}.json")), metadata)
                .map_err(|err| format!("write BGRA debug metadata failed: {err}"))?;
        }
        Ok(())
    }

    fn prepare_directory(path: &str) -> Result<PathBuf, String> {
        let directory = Path::new(path);
        fs::create_dir_all(directory)
            .map_err(|err| format!("create debug dump directory failed: {err}"))?;
        Ok(directory.to_path_buf())
    }

    fn validate_config(config: &GpuConvertProbeConfig) -> Result<(), String> {
        if config.duration_sec == 0 || config.target_fps == 0 {
            return Err("duration-sec and target-fps must be greater than zero".to_string());
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

    fn average_ms(total: f64, count: u64) -> f64 {
        if count == 0 {
            0.0
        } else {
            total / count as f64
        }
    }

    fn average_delta(current: f64, previous: f64, count: u64) -> f64 {
        if count == 0 {
            0.0
        } else {
            (current - previous).max(0.0) / count as f64
        }
    }
}

#[cfg(windows)]
pub use platform::run;

#[cfg(not(windows))]
pub fn run(_config: GpuConvertProbeConfig) -> Result<(), String> {
    Err("gpu-convert-probe is only supported on Windows".to_string())
}
