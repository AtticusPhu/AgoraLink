#[derive(Debug)]
pub struct ColorTestPatternConfig {
    pub output: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub duration_sec: u64,
    pub bitrate_mbps: f64,
    pub color_spec: crate::color_spec::ColorSpec,
}

#[cfg(windows)]
mod platform {
    use std::io::{self, Write};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::ColorTestPatternConfig;
    use crate::bgra_to_nv12;
    use crate::wmf_h264_encoder::{EncoderStats, WmfH264Encoder, ENCODER_NAME};

    pub fn run(config: ColorTestPatternConfig) -> Result<(), String> {
        validate(&config)?;
        let base = build_pattern(config.width, config.height);
        let mut bgra = base.clone();
        let mut nv12 = vec![0u8; bgra_to_nv12::buffer_size(config.width, config.height)?];
        let mut encoder = WmfH264Encoder::new_with_color(
            config.width,
            config.height,
            config.fps,
            config.bitrate_mbps,
            &config.output,
            config.color_spec,
        )?;
        let total_frames = u64::from(config.fps)
            .checked_mul(config.duration_sec)
            .ok_or_else(|| "color test frame count overflow".to_string())?;
        let frame_interval = Duration::from_nanos(1_000_000_000 / u64::from(config.fps));
        let started_at = Instant::now();
        let mut next_frame_at = started_at;
        let mut report_at = started_at;
        let mut previous = EncoderStats::default();

        eprintln!(
            "color-test-pattern encoder=\"{}\" size={}x{} fps={} bitrate_mbps={} matrix={} range={} output={} encoder_input_metadata={:?} encoder_output_metadata={:?}",
            ENCODER_NAME,
            config.width,
            config.height,
            config.fps,
            config.bitrate_mbps,
            config.color_spec.yuv_matrix(),
            config.color_spec.color_range(),
            config.output,
            encoder.input_color_metadata(),
            encoder.output_color_metadata()
        );

        for frame_index in 0..total_frames {
            sleep_until(next_frame_at);
            bgra.copy_from_slice(&base);
            draw_motion_marker(
                &mut bgra,
                config.width as usize,
                config.height as usize,
                frame_index,
            );
            bgra_to_nv12::convert_with_spec(
                &bgra,
                config.width as usize * 4,
                config.width,
                config.height,
                &mut nv12,
                config.color_spec,
            )?;
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
        let wall_time_sec = started_at.elapsed().as_secs_f64().max(0.001);
        println!(
            r#"{{"type":"COLOR_TEST_DONE","mode":"color_test_pattern","encoder":"{}","frames_in":{},"samples_out":{},"bytes_out":{},"fps":{:.2},"mbps":{:.3},"width":{},"height":{},"output":"{}",{},{},{}}}"#,
            ENCODER_NAME,
            stats.frames_in,
            stats.samples_out,
            stats.bytes_out,
            stats.frames_in as f64 / wall_time_sec,
            stats.bytes_out as f64 * 8.0
                / (stats.frames_in as f64 / f64::from(config.fps)).max(0.001)
                / 1_000_000.0,
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
        Ok(())
    }

    fn print_stats(
        config: &ColorTestPatternConfig,
        current: EncoderStats,
        previous: EncoderStats,
        elapsed: Duration,
    ) {
        let elapsed_sec = elapsed.as_secs_f64().max(0.001);
        let frame_delta = current.frames_in.saturating_sub(previous.frames_in);
        let bytes_delta = current.bytes_out.saturating_sub(previous.bytes_out);
        println!(
            r#"{{"type":"COLOR_TEST_STATS","mode":"color_test_pattern","frames_in":{},"samples_out":{},"bytes_out":{},"fps":{:.2},"mbps":{:.3},"width":{},"height":{},{} }}"#,
            current.frames_in,
            current.samples_out,
            current.bytes_out,
            frame_delta as f64 / elapsed_sec,
            bytes_delta as f64 * 8.0 / elapsed_sec / 1_000_000.0,
            config.width,
            config.height,
            config.color_spec.json_fragment()
        );
        io::stdout().flush().ok();
    }

    fn validate(config: &ColorTestPatternConfig) -> Result<(), String> {
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
        if !config.bitrate_mbps.is_finite() || config.bitrate_mbps <= 0.0 {
            return Err("bitrate-mbps must be greater than zero".to_string());
        }
        if config.output.trim().is_empty() {
            return Err("output path must not be empty".to_string());
        }
        Ok(())
    }

    fn build_pattern(width: u32, height: u32) -> Vec<u8> {
        let width = width as usize;
        let height = height as usize;
        let mut output = vec![0u8; width * height * 4];
        fill_rect(&mut output, width, 0, 0, width, height, (40, 44, 52));

        let bar_height = (height / 3).max(2);
        let bars = [
            (235, 235, 235),
            (235, 235, 16),
            (16, 235, 235),
            (16, 235, 16),
            (235, 16, 235),
            (235, 16, 16),
            (16, 16, 235),
        ];
        for (index, color) in bars.iter().enumerate() {
            let x0 = index * width / bars.len();
            let x1 = (index + 1) * width / bars.len();
            fill_rect(&mut output, width, x0, 0, x1 - x0, bar_height, *color);
        }

        let middle_y = bar_height;
        let middle_height = (height / 3).max(2);
        let quarter = width / 4;
        fill_rect(
            &mut output,
            width,
            0,
            middle_y,
            quarter,
            middle_height,
            (0, 0, 0),
        );
        fill_rect(
            &mut output,
            width,
            quarter,
            middle_y,
            quarter,
            middle_height,
            (255, 255, 255),
        );
        fill_rect(
            &mut output,
            width,
            quarter * 2,
            middle_y,
            quarter,
            middle_height,
            (255, 0, 0),
        );
        fill_rect(
            &mut output,
            width,
            quarter * 3,
            middle_y,
            width - quarter * 3,
            middle_height,
            (0, 0, 255),
        );

        let grid_y = middle_y + middle_height;
        fill_rect(
            &mut output,
            width,
            0,
            grid_y,
            width,
            height - grid_y,
            (112, 120, 132),
        );
        let grid_step = (width / 80).clamp(8, 32);
        for y in (grid_y..height).step_by(grid_step) {
            fill_rect(&mut output, width, 0, y, width, 1, (230, 230, 230));
        }
        for x in (0..width).step_by(grid_step) {
            fill_rect(
                &mut output,
                width,
                x,
                grid_y,
                1,
                height - grid_y,
                (20, 20, 20),
            );
        }
        let scale = (width / 320).clamp(2, 8);
        draw_text(
            &mut output,
            width,
            24,
            grid_y + 24,
            scale,
            "REC709 NV12",
            (248, 248, 248),
        );
        output
    }

    fn draw_motion_marker(bgra: &mut [u8], width: usize, height: usize, frame_index: u64) {
        let marker_width = (width / 16).max(8);
        let marker_height = (height / 32).max(4);
        let span = width.saturating_sub(marker_width).max(1);
        let x = (frame_index as usize * 11) % span;
        let y = height.saturating_sub(marker_height + 8);
        fill_rect(
            bgra,
            width,
            x,
            y,
            marker_width,
            marker_height,
            (245, 180, 32),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn fill_rect(
        bgra: &mut [u8],
        stride_width: usize,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        rgb: (u8, u8, u8),
    ) {
        let image_height = bgra.len() / (stride_width * 4);
        let x_end = x.saturating_add(width).min(stride_width);
        let y_end = y.saturating_add(height).min(image_height);
        for row in y..y_end {
            for column in x..x_end {
                let offset = (row * stride_width + column) * 4;
                bgra[offset] = rgb.2;
                bgra[offset + 1] = rgb.1;
                bgra[offset + 2] = rgb.0;
                bgra[offset + 3] = 255;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text(
        bgra: &mut [u8],
        width: usize,
        x: usize,
        y: usize,
        scale: usize,
        text: &str,
        rgb: (u8, u8, u8),
    ) {
        let mut cursor = x;
        for character in text.chars() {
            let glyph = glyph(character);
            for (row, bits) in glyph.iter().enumerate() {
                for column in 0..5 {
                    if bits & (1 << (4 - column)) != 0 {
                        fill_rect(
                            bgra,
                            width,
                            cursor + column * scale,
                            y + row * scale,
                            scale,
                            scale,
                            rgb,
                        );
                    }
                }
            }
            cursor += 6 * scale;
        }
    }

    fn glyph(character: char) -> [u8; 7] {
        match character {
            'R' => [0x1e, 0x11, 0x11, 0x1e, 0x14, 0x12, 0x11],
            'E' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
            'C' => [0x0f, 0x10, 0x10, 0x10, 0x10, 0x10, 0x0f],
            'N' => [0x11, 0x19, 0x19, 0x15, 0x13, 0x13, 0x11],
            'V' => [0x11, 0x11, 0x11, 0x11, 0x0a, 0x0a, 0x04],
            '0' => [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
            '1' => [0x04, 0x0c, 0x14, 0x04, 0x04, 0x04, 0x1f],
            '2' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
            '7' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
            '9' => [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x11, 0x0e],
            _ => [0; 7],
        }
    }

    fn sleep_until(target: Instant) {
        let now = Instant::now();
        if target > now {
            thread::sleep(target.duration_since(now));
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
pub fn run(_config: ColorTestPatternConfig) -> Result<(), String> {
    Err("color-test-pattern is only supported on Windows".to_string())
}
