use crate::d3d11_nv12_renderer::{D3d11Nv12Renderer, D3d11RenderStats};
use crate::decoded_frame_renderer::OwnedBgraFrame;
use crate::win32_gdi_viewer::{GdiRenderStats, GdiViewerWindow, RenderScaleMode, WindowMode};
use crate::wmf_h264_decoder::DecodedFrame;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderBackend {
    Gdi,
    D3d11,
}

impl RenderBackend {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Gdi => "gdi",
            Self::D3d11 => "d3d11",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "gdi" => Ok(Self::Gdi),
            "d3d11" => Ok(Self::D3d11),
            _ => Err("render-backend must be gdi or d3d11".to_string()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct VideoRenderStats {
    pub requested: RenderBackend,
    pub selected: RenderBackend,
    pub fallback: bool,
    pub fallback_reason: Option<String>,
    pub window: GdiRenderStats,
    pub d3d11: D3d11RenderStats,
}

impl Default for VideoRenderStats {
    fn default() -> Self {
        Self {
            requested: RenderBackend::Gdi,
            selected: RenderBackend::Gdi,
            fallback: false,
            fallback_reason: None,
            window: GdiRenderStats::default(),
            d3d11: D3d11RenderStats::default(),
        }
    }
}

impl VideoRenderStats {
    pub fn json_fragment(&self) -> String {
        let fallback_reason = self
            .fallback_reason
            .as_deref()
            .map(|value| format!(r#""{}""#, json_escape(value)))
            .unwrap_or_else(|| "null".to_string());
        format!(
            r#""render_backend":"{}","render_backend_requested":"{}","render_backend_selected":"{}","render_backend_fallback":{},"render_backend_fallback_reason":{},{},{}"#,
            self.selected.name(),
            self.requested.name(),
            self.selected.name(),
            self.fallback,
            fallback_reason,
            self.window.json_fragment_without_backend(),
            self.d3d11.json_fragment(),
        )
    }
}

pub struct VideoRenderer {
    window: GdiViewerWindow,
    requested: RenderBackend,
    selected: RenderBackend,
    fallback_reason: Option<String>,
    d3d11: Option<D3d11Nv12Renderer>,
    generation: u64,
}

impl VideoRenderer {
    pub fn create(
        title: &str,
        scale_mode: RenderScaleMode,
        window_mode: WindowMode,
        requested: RenderBackend,
    ) -> Result<Self, String> {
        Self::create_with_display_detection(
            title,
            scale_mode,
            window_mode,
            requested,
            crate::display_capability::DisplayRefreshDetect::Auto,
        )
    }

    pub fn create_with_display_detection(
        title: &str,
        scale_mode: RenderScaleMode,
        window_mode: WindowMode,
        requested: RenderBackend,
        display_refresh_detect: crate::display_capability::DisplayRefreshDetect,
    ) -> Result<Self, String> {
        let window = GdiViewerWindow::create_with_display_detection(
            title,
            scale_mode,
            window_mode,
            display_refresh_detect,
        )?;
        let (selected, fallback_reason, d3d11) = match requested {
            RenderBackend::Gdi => (RenderBackend::Gdi, None, None),
            RenderBackend::D3d11 => match D3d11Nv12Renderer::new(window.hwnd(), 960, 600) {
                Ok(renderer) => (RenderBackend::D3d11, None, Some(renderer)),
                Err(err) => (
                    RenderBackend::Gdi,
                    Some(format!("D3D11 renderer initialization failed: {err}")),
                    None,
                ),
            },
        };
        Ok(Self {
            window,
            requested,
            selected,
            fallback_reason,
            d3d11,
            generation: 0,
        })
    }

    pub fn pump_messages(&mut self) -> bool {
        self.window.pump_messages()
    }

    pub fn prepare_video(
        &mut self,
        width: u32,
        height: u32,
        frame_id: Option<u64>,
    ) -> Result<(), String> {
        self.window.prepare_video(width, height, frame_id)
    }

    pub fn render_decoded(
        &mut self,
        frame: &DecodedFrame,
        width: u32,
        height: u32,
        frame_id: u64,
    ) -> Result<Option<OwnedBgraFrame>, String> {
        self.prepare_video(width, height, Some(frame_id))?;
        if self.selected == RenderBackend::D3d11 {
            let layout = self.window.render_stats();
            let result = self
                .d3d11
                .as_mut()
                .ok_or_else(|| "D3D11 renderer is unavailable".to_string())?
                .render(frame, width, height, &layout);
            if result.is_ok() {
                return Ok(None);
            }
            let err = result.unwrap_err();
            self.selected = RenderBackend::Gdi;
            self.fallback_reason = Some(format!("D3D11 renderer failed at runtime: {err}"));
            self.d3d11 = None;
        }
        self.generation += 1;
        let owned = OwnedBgraFrame::from_decoded(frame, width, height, self.generation)?;
        owned.render_for_frame(&mut self.window, frame_id)?;
        Ok(Some(owned))
    }

    pub fn stats(&self) -> VideoRenderStats {
        VideoRenderStats {
            requested: self.requested,
            selected: self.selected,
            fallback: self.selected != self.requested,
            fallback_reason: self.fallback_reason.clone(),
            window: self.window.render_stats(),
            d3d11: self
                .d3d11
                .as_ref()
                .map(D3d11Nv12Renderer::stats)
                .unwrap_or_default(),
        }
    }
}

fn json_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}
