#[derive(Debug)]
pub struct H264FileViewerConfig {
    pub input: String,
}

#[cfg(windows)]
mod platform {
    use std::fs;
    use std::io::{self, Write};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::H264FileViewerConfig;
    use crate::h264_annex_b::{dimensions_from_sps, split_access_units};
    use crate::nv12_to_bgra;
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
        let mut window = GdiViewerWindow::create(&title)?;

        eprintln!(
            "h264-file-viewer input={} bytes={} access_units={} decoder=\"{}\" output=NV12 render=GDI size={}x{} playback_fps={}",
            config.input,
            bytes.len(),
            access_units.len(),
            DECODER_NAME,
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
        let mut bgra = Vec::new();
        let mut closed_by_user = false;

        for (frame_index, access_unit) in access_units.iter().enumerate() {
            if !window.pump_messages() {
                closed_by_user = true;
                break;
            }
            let decoded = decoder.decode_access_unit(access_unit, frame_index as u64)?;
            frames_decoded += decoded.len() as u64;
            for frame in decoded {
                if !wait_until_frame(&mut window, next_frame_at) {
                    closed_by_user = true;
                    break;
                }
                render_frame(
                    &mut window,
                    &frame,
                    dimensions.width,
                    dimensions.height,
                    &mut bgra,
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
                if !wait_until_frame(&mut window, next_frame_at) {
                    closed_by_user = true;
                    break;
                }
                render_frame(
                    &mut window,
                    &frame,
                    dimensions.width,
                    dimensions.height,
                    &mut bgra,
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
                );
            }
        }

        let elapsed = started_at.elapsed().as_secs_f64().max(0.001);
        println!(
            r#"{{"type":"VIEWER_DONE","mode":"h264_file_viewer","decoder":"{}","render":"GDI","frames_decoded":{},"frames_rendered":{},"fps":{:.2},"width":{},"height":{},"closed_by_user":{},"input":"{}"}}"#,
            DECODER_NAME,
            frames_decoded,
            frames_rendered,
            frames_rendered as f64 / elapsed,
            dimensions.width,
            dimensions.height,
            closed_by_user,
            json_escape(&config.input)
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
        bgra: &mut Vec<u8>,
    ) -> Result<(), String> {
        nv12_to_bgra::convert(&frame.nv12, width, height, bgra)?;
        window.render_bgra(bgra, width, height)
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
    ) {
        let now = Instant::now();
        let elapsed = now.duration_since(*report_at);
        if elapsed < Duration::from_secs(1) {
            return;
        }
        let rendered_delta = frames_rendered.saturating_sub(*previous_rendered);
        println!(
            r#"{{"type":"VIEWER_STATS","mode":"h264_file_viewer","frames_decoded":{},"frames_rendered":{},"fps":{:.2},"width":{},"height":{}}}"#,
            frames_decoded,
            frames_rendered,
            rendered_delta as f64 / elapsed.as_secs_f64().max(0.001),
            width,
            height
        );
        io::stdout().flush().ok();
        *previous_rendered = frames_rendered;
        *report_at = now;
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
