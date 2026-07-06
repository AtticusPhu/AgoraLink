use std::fs;
use std::path::Path;

use crate::nv12_to_bgra;
use crate::win32_gdi_viewer::GdiViewerWindow;
use crate::wmf_h264_decoder::DecodedFrame;
use crate::{color_spec::ColorSpec, color_spec::MediaColorMetadata};

pub struct OwnedBgraFrame {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub generation: u64,
    pub nv12_y_stride: usize,
    pub nv12_uv_stride: usize,
    pub nv12_uv_offset: usize,
    pub nv12_allocated_height: usize,
    pub nv12_buffer_len: usize,
    pub expected_tight_len: usize,
    pub decoder_used_2d_buffer: bool,
    pub color_spec: ColorSpec,
    pub color_metadata: MediaColorMetadata,
}

impl OwnedBgraFrame {
    pub fn from_decoded(
        frame: &DecodedFrame,
        width: u32,
        height: u32,
        generation: u64,
    ) -> Result<Self, String> {
        let mut pixels = Vec::with_capacity(nv12_to_bgra::bgra_size(width, height)?);
        nv12_to_bgra::convert_with_layout_and_spec(
            &frame.nv12,
            width,
            height,
            frame.y_stride,
            frame.uv_stride,
            frame.uv_offset,
            &mut pixels,
            frame.color_spec,
        )?;
        Ok(Self {
            pixels,
            width,
            height,
            stride: width as usize * 4,
            generation,
            nv12_y_stride: frame.y_stride,
            nv12_uv_stride: frame.uv_stride,
            nv12_uv_offset: frame.uv_offset,
            nv12_allocated_height: frame.allocated_height,
            nv12_buffer_len: frame.source_buffer_len,
            expected_tight_len: frame.expected_tight_len,
            decoder_used_2d_buffer: frame.used_2d_buffer,
            color_spec: frame.color_spec,
            color_metadata: frame.color_metadata,
        })
    }

    pub fn render(&self, window: &mut GdiViewerWindow) -> Result<(), String> {
        window.render_bgra_with_stride(&self.pixels, self.width, self.height, self.stride)
    }

    pub fn render_for_frame(
        &self,
        window: &mut GdiViewerWindow,
        frame_id: u64,
    ) -> Result<(), String> {
        window.prepare_video(self.width, self.height, Some(frame_id))?;
        self.render(window)
    }

    pub fn dump_raw(&self, directory: &Path, index: u64) -> Result<(), String> {
        fs::create_dir_all(directory)
            .map_err(|err| format!("create debug frame directory failed: {err}"))?;
        let stem = format!("frame_{index:06}");
        fs::write(directory.join(format!("{stem}.bgra")), &self.pixels)
            .map_err(|err| format!("write debug BGRA frame failed: {err}"))?;
        let metadata = format!(
            concat!(
                "{{\n",
                "  \"format\": \"BGRA8\",\n",
                "  \"width\": {},\n",
                "  \"height\": {},\n",
                "  \"bgra_stride\": {},\n",
                "  \"generation\": {},\n",
                "  \"nv12_y_stride\": {},\n",
                "  \"nv12_uv_stride\": {},\n",
                "  \"nv12_uv_offset\": {},\n",
                "  \"nv12_allocated_height\": {},\n",
                "  \"nv12_buffer_len\": {},\n",
                "  \"expected_tight_len\": {},\n",
                "  \"decoder_used_2d_buffer\": {},\n",
                "  {},\n",
                "  {}\n",
                "}}\n"
            ),
            self.width,
            self.height,
            self.stride,
            self.generation,
            self.nv12_y_stride,
            self.nv12_uv_stride,
            self.nv12_uv_offset,
            self.nv12_allocated_height,
            self.nv12_buffer_len,
            self.expected_tight_len,
            self.decoder_used_2d_buffer,
            self.color_spec.json_fragment(),
            self.color_metadata.json_fragment("decoder_output")
        );
        fs::write(directory.join(format!("{stem}.json")), metadata)
            .map_err(|err| format!("write debug frame metadata failed: {err}"))
    }
}
