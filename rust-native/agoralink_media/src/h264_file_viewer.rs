#[derive(Debug)]
pub struct H264FileViewerConfig {
    pub input: String,
    pub render_scale: crate::win32_gdi_viewer::RenderScaleMode,
}

#[cfg(windows)]
mod platform {
    use std::fs;
    use std::io::{self, Write};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::H264FileViewerConfig;
    use crate::color_spec::{ColorSpec, MediaColorMetadata};
    use crate::decoded_frame_renderer::OwnedBgraFrame;
    use crate::h264_annex_b::{dimensions_from_sps, split_access_units};
    use crate::win32_gdi_viewer::GdiViewerWindow;
    use crate::wmf_h264_decoder::{DecodedFrame, WmfH264Decoder, DECODER_NAME};

    const PLAYBACK_FPS: u32 = 30;

    pub fn run(config: H264FileViewerConfig) -> Result<(), String> {
        if config.input.trim().is_empty() {
            return Err("input path must not be empty".to_string());
        }
        let bytes =
            fs::read(&config.input).map_err(|err| format!("read H.264 input failed: {err}"))?;
        if bytes.is_empty() {
            return Err("H.264 input file is empty".to_string());
        }
        let dimensions = dimensions_from_sps(&bytes)?;
        let access_units = split_access_units(&bytes)?;
        let mut decoder = WmfH264Decoder::new(dimensions.width, dimensions.height, PLAYBACK_FPS)?;
        let title = format!("AgoraLink Native H.264 Viewer - {}", config.input);
        let mut window = GdiViewerWindow::create(&title, config.render_scale)?;

        eprintln!(
            "h264-file-viewer input={} bytes={} access_units={} decoder=\"{}\" output=NV12 render=GDI render_scale={} size={}x{} playback_fps={}",
            config.input,
            bytes.len(),
            access_units.len(),
            DECODER_NAME,
            config.render_scale.name(),
            dimensions.width,
            dimensions.height,
            PLAYBACK_FPS
        );

        let frame_interval = Duration::from_nanos(1_000_000_000u64 / u64::from(PLAYBACK_FPS));
        let started_at = Instant::now();
        let mut next_frame_at = started_at;
        let mut report_at = started_at;
        let mut previous_rendered = 0u64;
        let mut frames_decoded = 0u64;
        let mut frames_rendered = 0u64;
        let mut render_generation = 0u64;
        let mut closed_by_user = false;
        let mut last_color_spec = ColorSpec::default();
        let mut last_color_metadata = MediaColorMetadata::default();
        let mut last_y_stride = dimensions.width as usize;
        let mut last_uv_stride = dimensions.width as usize;
        let mut last_uv_offset = dimensions.width as usize * dimensions.height as usize;
        let mut last_allocated_height = dimensions.height as usize;

        for (frame_index, access_unit) in access_units.iter().enumerate() {
            if !window.pump_messages() {
                closed_by_user = true;
                break;
            }
            let decoded = decoder.decode_access_unit(access_unit, frame_index as u64)?;
            frames_decoded += decoded.len() as u64;
            for frame in decoded {
                update_layout_stats(
                    &frame,
                    &mut last_color_spec,
                    &mut last_color_metadata,
                    &mut last_y_stride,
                    &mut last_uv_stride,
                    &mut last_uv_offset,
                    &mut last_allocated_height,
                );
                if !wait_until_frame(&mut window, next_frame_at) {
                    closed_by_user = true;
                    break;
                }
                render_frame(
                    &mut window,
                    &frame,
                    dimensions.width,
                    dimensions.height,
                    &mut render_generation,
                )?;
                frames_rendered += 1;
                next_frame_at += frame_interval;
                maybe_print_stats(
                    &mut report_at,
                    &mut previous_rendered,
                    frames_decoded,
                    frames_rendered,
                    dimensions.width,
                    dimensions.height,
                    last_color_spec,
                    last_color_metadata,
                    last_y_stride,
                    last_uv_stride,
                    last_uv_offset,
                    last_allocated_height,
                    window.render_stats(),
                );
            }
            if closed_by_user {
                break;
            }
        }

        if !closed_by_user {
            let drained = decoder.finish()?;
            frames_decoded += drained.len() as u64;
            for frame in drained {
                update_layout_stats(
                    &frame,
                    &mut last_color_spec,
                    &mut last_color_metadata,
                    &mut last_y_stride,
                    &mut last_uv_stride,
                    &mut last_uv_offset,
                    &mut last_allocated_height,
                );
                if !wait_until_frame(&mut window, next_frame_at) {
                    closed_by_user = true;
                    break;
                }
                render_frame(
                    &mut window,
                    &frame,
                    dimensions.width,
                    dimensions.height,
                    &mut render_generation,
                )?;
                frames_rendered += 1;
                next_frame_at += frame_interval;
                maybe_print_stats(
                    &mut report_at,
                    &mut previous_rendered,
                    frames_decoded,
                    frames_rendered,
                    dimensions.width,
                    dimensions.height,
                    last_color_spec,
                    last_color_metadata,
                    last_y_stride,
                    last_uv_stride,
                    last_uv_offset,
                    last_allocated_height,
                    window.render_stats(),
                );
            }
        }

        let elapsed = started_at.elapsed().as_secs_f64().max(0.001);
        println!(
            r#"{{"type":"VIEWER_DONE","mode":"h264_file_viewer","decoder":"{}","render":"GDI","frames_decoded":{},"frames_rendered":{},"fps":{:.2},"width":{},"height":{},"closed_by_user":{},"input":"{}","nv12_y_stride":{},"nv12_uv_stride":{},"nv12_uv_offset":{},"nv12_allocated_height":{},{},{},{}}}"#,
            DECODER_NAME,
            frames_decoded,
            frames_rendered,
            frames_rendered as f64 / elapsed,
            dimensions.width,
            dimensions.height,
            closed_by_user,
            json_escape(&config.input),
            last_y_stride,
            last_uv_stride,
            last_uv_offset,
            last_allocated_height,
            last_color_spec.json_fragment(),
            last_color_metadata.json_fragment("decoder_output"),
            window.render_stats().json_fragment(),
        );
        io::stdout().flush().ok();
        eprintln!(
            "h264-file-viewer stopped reason={}",
            if closed_by_user {
                "window-closed"
            } else {
                "end-of-file"
            }
        );
        Ok(())
    }

    fn render_frame(
        window: &mut GdiViewerWindow,
        frame: &DecodedFrame,
        width: u32,
        height: u32,
        generation: &mut u64,
    ) -> Result<(), String> {
        *generation += 1;
        OwnedBgraFrame::from_decoded(frame, width, height, *generation)?.render(window)
    }

    fn wait_until_frame(window: &mut GdiViewerWindow, target: Instant) -> bool {
        loop {
            if !window.pump_messages() {
                return false;
            }
            let now = Instant::now();
            if now >= target {
                return true;
            }
            thread::sleep((target - now).min(Duration::from_millis(4)));
        }
    }

    fn maybe_print_stats(
        report_at: &mut Instant,
        previous_rendered: &mut u64,
        frames_decoded: u64,
        frames_rendered: u64,
        width: u32,
        height: u32,
        color_spec: ColorSpec,
        color_metadata: MediaColorMetadata,
        y_stride: usize,
        uv_stride: usize,
        uv_offset: usize,
        allocated_height: usize,
        render_stats: crate::win32_gdi_viewer::GdiRenderStats,
    ) {
        let now = Instant::now();
        let elapsed = now.duration_since(*report_at);
        if elapsed < Duration::from_secs(1) {
            return;
        }
        let rendered_delta = frames_rendered.saturating_sub(*previous_rendered);
        println!(
            r#"{{"type":"VIEWER_STATS","mode":"h264_file_viewer","frames_decoded":{},"frames_rendered":{},"fps":{:.2},"width":{},"height":{},"nv12_y_stride":{},"nv12_uv_stride":{},"nv12_uv_offset":{},"nv12_allocated_height":{},{},{},{}}}"#,
            frames_decoded,
            frames_rendered,
            rendered_delta as f64 / elapsed.as_secs_f64().max(0.001),
            width,
            height,
            y_stride,
            uv_stride,
            uv_offset,
            allocated_height,
            color_spec.json_fragment(),
            color_metadata.json_fragment("decoder_output"),
            render_stats.json_fragment(),
        );
        io::stdout().flush().ok();
        *previous_rendered = frames_rendered;
        *report_at = now;
    }

    #[allow(clippy::too_many_arguments)]
    fn update_layout_stats(
        frame: &DecodedFrame,
        color_spec: &mut ColorSpec,
        color_metadata: &mut MediaColorMetadata,
        y_stride: &mut usize,
        uv_stride: &mut usize,
        uv_offset: &mut usize,
        allocated_height: &mut usize,
    ) {
        *color_spec = frame.color_spec;
        *color_metadata = frame.color_metadata;
        *y_stride = frame.y_stride;
        *uv_stride = frame.uv_stride;
        *uv_offset = frame.uv_offset;
        *allocated_height = frame.allocated_height;
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
pub fn run(_config: H264FileViewerConfig) -> Result<(), String> {
    Err("h264-file-viewer is only supported on Windows".to_string())
}
