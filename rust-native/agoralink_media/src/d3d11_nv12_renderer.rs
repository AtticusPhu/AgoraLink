#[cfg(windows)]
mod platform {
    use std::ptr;
    use std::time::Instant;

    use windows::core::s;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
    use windows::Win32::Graphics::Direct3D::{
        ID3DBlob, D3D_DRIVER_TYPE_HARDWARE, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDeviceAndSwapChain, ID3D11Device, ID3D11DeviceContext, ID3D11PixelShader,
        ID3D11RenderTargetView, ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D,
        ID3D11VertexShader, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_WRITE,
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_WRITE_DISCARD,
        D3D11_SAMPLER_DESC, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DYNAMIC,
        D3D11_VIEWPORT,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R8_UNORM, DXGI_MODE_DESC,
        DXGI_RATIONAL, DXGI_SAMPLE_DESC,
    };
    use windows::Win32::Graphics::Dxgi::{
        IDXGISwapChain, DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_EFFECT_DISCARD,
        DXGI_USAGE_RENDER_TARGET_OUTPUT,
    };

    use crate::win32_gdi_viewer::GdiRenderStats;
    use crate::wmf_h264_decoder::DecodedFrame;

    const VERTEX_SHADER: &[u8] = br#"
struct VSOut { float4 position : SV_POSITION; float2 uv : TEXCOORD0; };
VSOut main(uint id : SV_VertexID) {
    VSOut output;
    float2 uv = float2((id << 1) & 2, id & 2);
    output.position = float4(uv * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0);
    output.uv = uv;
    return output;
}
"#;

    const PIXEL_SHADER: &[u8] = br#"
Texture2D y_plane : register(t0);
Texture2D uv_plane : register(t1);
SamplerState plane_sampler : register(s0);
float4 main(float4 position : SV_POSITION, float2 texcoord : TEXCOORD0) : SV_TARGET {
    float y_code = y_plane.Sample(plane_sampler, texcoord).r * 255.0;
    float2 uv_code = uv_plane.Sample(plane_sampler, texcoord).rg * 255.0;
    float y = max(0.0, (y_code - 16.0) / 219.0);
    float cb = (uv_code.x - 128.0) / 224.0;
    float cr = (uv_code.y - 128.0) / 224.0;
    float3 rgb = float3(
        y + 1.5748 * cr,
        y - 0.1873 * cb - 0.4681 * cr,
        y + 1.8556 * cb
    );
    return float4(saturate(rgb), 1.0);
}
"#;

    #[derive(Clone, Debug, Default)]
    pub struct D3d11RenderStats {
        pub device_created: bool,
        pub swapchain_created: bool,
        pub present_count: u64,
        pub present_errors: u64,
        pub present_ms_total: f64,
        pub present_ms_max: f64,
        pub upload_ms_total: f64,
        pub upload_ms_max: f64,
        pub shader_nv12_to_rgb: bool,
    }

    impl D3d11RenderStats {
        pub fn json_fragment(&self) -> String {
            format!(
                concat!(
                    r#""d3d11_device_created":{},"d3d11_swapchain_created":{},"#,
                    r#""d3d11_present_count":{},"d3d11_present_errors":{},"#,
                    r#""d3d11_present_ms_avg":{:.3},"d3d11_present_ms_max":{:.3},"#,
                    r#""d3d11_upload_ms_avg":{:.3},"d3d11_upload_ms_max":{:.3},"#,
                    r#""d3d11_shader_nv12_to_rgb":{},"d3d11_vsync_mode":"off""#
                ),
                self.device_created,
                self.swapchain_created,
                self.present_count,
                self.present_errors,
                average(self.present_ms_total, self.present_count),
                self.present_ms_max,
                average(self.upload_ms_total, self.present_count),
                self.upload_ms_max,
                self.shader_nv12_to_rgb,
            )
        }
    }

    pub struct D3d11Nv12Renderer {
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        swapchain: IDXGISwapChain,
        render_target: Option<ID3D11RenderTargetView>,
        vertex_shader: ID3D11VertexShader,
        pixel_shader: ID3D11PixelShader,
        sampler: ID3D11SamplerState,
        y_texture: Option<ID3D11Texture2D>,
        uv_texture: Option<ID3D11Texture2D>,
        y_view: Option<ID3D11ShaderResourceView>,
        uv_view: Option<ID3D11ShaderResourceView>,
        texture_width: u32,
        texture_height: u32,
        client_width: u32,
        client_height: u32,
        stats: D3d11RenderStats,
    }

    impl D3d11Nv12Renderer {
        pub fn new(
            hwnd: windows::Win32::Foundation::HWND,
            client_width: u32,
            client_height: u32,
        ) -> Result<Self, String> {
            let swap_desc = DXGI_SWAP_CHAIN_DESC {
                BufferDesc: DXGI_MODE_DESC {
                    Width: client_width.max(1),
                    Height: client_height.max(1),
                    RefreshRate: DXGI_RATIONAL {
                        Numerator: 0,
                        Denominator: 1,
                    },
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    ..Default::default()
                },
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                OutputWindow: hwnd,
                Windowed: true.into(),
                SwapEffect: DXGI_SWAP_EFFECT_DISCARD,
                ..Default::default()
            };
            let mut device = None;
            let mut context = None;
            let mut swapchain = None;
            unsafe {
                D3D11CreateDeviceAndSwapChain(
                    None,
                    D3D_DRIVER_TYPE_HARDWARE,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&raw const swap_desc),
                    Some(&raw mut swapchain),
                    Some(&raw mut device),
                    None,
                    Some(&raw mut context),
                )
            }
            .map_err(|err| format!("D3D11CreateDeviceAndSwapChain failed: {err}"))?;
            let device = device.ok_or_else(|| "D3D11 returned no device".to_string())?;
            let context = context.ok_or_else(|| "D3D11 returned no context".to_string())?;
            let swapchain = swapchain.ok_or_else(|| "DXGI returned no swapchain".to_string())?;

            let vertex_bytecode = compile_shader(VERTEX_SHADER, s!("main"), s!("vs_4_0"))?;
            let pixel_bytecode = compile_shader(PIXEL_SHADER, s!("main"), s!("ps_4_0"))?;
            let mut vertex_shader = None;
            unsafe {
                device.CreateVertexShader(&vertex_bytecode, None, Some(&raw mut vertex_shader))
            }
            .map_err(|err| format!("CreateVertexShader failed: {err}"))?;
            let mut pixel_shader = None;
            unsafe { device.CreatePixelShader(&pixel_bytecode, None, Some(&raw mut pixel_shader)) }
                .map_err(|err| format!("CreatePixelShader failed: {err}"))?;
            let sampler_desc = D3D11_SAMPLER_DESC {
                Filter: windows::Win32::Graphics::Direct3D11::D3D11_FILTER_MIN_MAG_LINEAR_MIP_POINT,
                AddressU: windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            let mut sampler = None;
            unsafe { device.CreateSamplerState(&sampler_desc, Some(&raw mut sampler)) }
                .map_err(|err| format!("CreateSamplerState failed: {err}"))?;

            let mut renderer = Self {
                device,
                context,
                swapchain,
                render_target: None,
                vertex_shader: vertex_shader
                    .ok_or_else(|| "CreateVertexShader returned no shader".to_string())?,
                pixel_shader: pixel_shader
                    .ok_or_else(|| "CreatePixelShader returned no shader".to_string())?,
                sampler: sampler
                    .ok_or_else(|| "CreateSamplerState returned no sampler".to_string())?,
                y_texture: None,
                uv_texture: None,
                y_view: None,
                uv_view: None,
                texture_width: 0,
                texture_height: 0,
                client_width: client_width.max(1),
                client_height: client_height.max(1),
                stats: D3d11RenderStats {
                    device_created: true,
                    swapchain_created: true,
                    shader_nv12_to_rgb: true,
                    ..D3d11RenderStats::default()
                },
            };
            renderer.recreate_render_target()?;
            Ok(renderer)
        }

        pub fn stats(&self) -> D3d11RenderStats {
            self.stats.clone()
        }

        pub fn render(
            &mut self,
            frame: &DecodedFrame,
            width: u32,
            height: u32,
            layout: &GdiRenderStats,
        ) -> Result<(), String> {
            self.ensure_client_size(layout.client_width, layout.client_height)?;
            self.ensure_textures(width, height)?;
            let upload_started = Instant::now();
            self.upload_plane(
                self.y_texture.as_ref().expect("Y texture initialized"),
                &frame.nv12,
                0,
                frame.y_stride,
                width as usize,
                height as usize,
            )?;
            self.upload_plane(
                self.uv_texture.as_ref().expect("UV texture initialized"),
                &frame.nv12,
                frame.uv_offset,
                frame.uv_stride,
                width as usize,
                height as usize / 2,
            )?;
            let upload_ms = upload_started.elapsed().as_secs_f64() * 1000.0;
            self.stats.upload_ms_total += upload_ms;
            self.stats.upload_ms_max = self.stats.upload_ms_max.max(upload_ms);

            let render_target = self
                .render_target
                .as_ref()
                .ok_or_else(|| "D3D11 render target is unavailable".to_string())?;
            let resources = [self.y_view.clone(), self.uv_view.clone()];
            let samplers = [Some(self.sampler.clone())];
            let targets = [Some(render_target.clone())];
            let viewport = viewport_for_layout(layout);
            unsafe {
                self.context
                    .ClearRenderTargetView(render_target, &[0.0, 0.0, 0.0, 1.0]);
                self.context.OMSetRenderTargets(Some(&targets), None);
                self.context.RSSetViewports(Some(&[viewport]));
                self.context
                    .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                self.context.VSSetShader(&self.vertex_shader, None);
                self.context.PSSetShader(&self.pixel_shader, None);
                self.context.PSSetShaderResources(0, Some(&resources));
                self.context.PSSetSamplers(0, Some(&samplers));
                self.context.Draw(3, 0);
                self.context.PSSetShaderResources(0, Some(&[None, None]));
            }
            let present_started = Instant::now();
            let result = unsafe { self.swapchain.Present(0, Default::default()) };
            let present_ms = present_started.elapsed().as_secs_f64() * 1000.0;
            if result.is_err() {
                self.stats.present_errors += 1;
                return Err(format!("DXGI Present failed: {result:?}"));
            }
            self.stats.present_count += 1;
            self.stats.present_ms_total += present_ms;
            self.stats.present_ms_max = self.stats.present_ms_max.max(present_ms);
            Ok(())
        }

        fn ensure_client_size(&mut self, width: u32, height: u32) -> Result<(), String> {
            let width = width.max(1);
            let height = height.max(1);
            if self.client_width == width && self.client_height == height {
                return Ok(());
            }
            self.render_target = None;
            unsafe {
                self.swapchain.ResizeBuffers(
                    0,
                    width,
                    height,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    Default::default(),
                )
            }
            .map_err(|err| format!("ResizeBuffers failed: {err}"))?;
            self.client_width = width;
            self.client_height = height;
            self.recreate_render_target()
        }

        fn recreate_render_target(&mut self) -> Result<(), String> {
            let backbuffer: ID3D11Texture2D = unsafe { self.swapchain.GetBuffer(0) }
                .map_err(|err| format!("swapchain GetBuffer failed: {err}"))?;
            let mut target = None;
            unsafe {
                self.device
                    .CreateRenderTargetView(&backbuffer, None, Some(&raw mut target))
            }
            .map_err(|err| format!("CreateRenderTargetView failed: {err}"))?;
            self.render_target = Some(
                target.ok_or_else(|| "CreateRenderTargetView returned no target".to_string())?,
            );
            Ok(())
        }

        fn ensure_textures(&mut self, width: u32, height: u32) -> Result<(), String> {
            if self.texture_width == width && self.texture_height == height {
                return Ok(());
            }
            let (y_texture, y_view) =
                create_dynamic_plane(&self.device, width, height, DXGI_FORMAT_R8_UNORM)?;
            let (uv_texture, uv_view) =
                create_dynamic_plane(&self.device, width / 2, height / 2, DXGI_FORMAT_R8G8_UNORM)?;
            self.y_texture = Some(y_texture);
            self.uv_texture = Some(uv_texture);
            self.y_view = Some(y_view);
            self.uv_view = Some(uv_view);
            self.texture_width = width;
            self.texture_height = height;
            Ok(())
        }

        fn upload_plane(
            &self,
            texture: &ID3D11Texture2D,
            source: &[u8],
            source_offset: usize,
            source_stride: usize,
            row_bytes: usize,
            rows: usize,
        ) -> Result<(), String> {
            let required = source_offset
                .checked_add(source_stride.saturating_mul(rows.saturating_sub(1)))
                .and_then(|value| value.checked_add(row_bytes))
                .ok_or_else(|| "NV12 upload bounds overflow".to_string())?;
            if required > source.len() {
                return Err(format!(
                    "NV12 plane is too small: required={required}, available={}",
                    source.len()
                ));
            }
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            unsafe {
                self.context.Map(
                    texture,
                    0,
                    D3D11_MAP_WRITE_DISCARD,
                    0,
                    Some(&raw mut mapped),
                )
            }
            .map_err(|err| format!("Map NV12 upload texture failed: {err}"))?;
            let destination = mapped.pData.cast::<u8>();
            for row in 0..rows {
                unsafe {
                    ptr::copy_nonoverlapping(
                        source.as_ptr().add(source_offset + row * source_stride),
                        destination.add(row * mapped.RowPitch as usize),
                        row_bytes,
                    );
                }
            }
            unsafe { self.context.Unmap(texture, 0) };
            Ok(())
        }
    }

    fn create_dynamic_plane(
        device: &ID3D11Device,
        width: u32,
        height: u32,
        format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
    ) -> Result<(ID3D11Texture2D, ID3D11ShaderResourceView), String> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            ..Default::default()
        };
        let mut texture = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&raw mut texture)) }
            .map_err(|err| format!("CreateTexture2D upload plane failed: {err}"))?;
        let texture = texture.ok_or_else(|| "CreateTexture2D returned no texture".to_string())?;
        let mut view = None;
        unsafe { device.CreateShaderResourceView(&texture, None, Some(&raw mut view)) }
            .map_err(|err| format!("CreateShaderResourceView failed: {err}"))?;
        Ok((
            texture,
            view.ok_or_else(|| "CreateShaderResourceView returned no view".to_string())?,
        ))
    }

    fn compile_shader(
        source: &[u8],
        entry: windows::core::PCSTR,
        target: windows::core::PCSTR,
    ) -> Result<Vec<u8>, String> {
        let mut bytecode: Option<ID3DBlob> = None;
        let mut errors: Option<ID3DBlob> = None;
        let result = unsafe {
            D3DCompile(
                source.as_ptr().cast(),
                source.len(),
                None,
                None,
                None,
                entry,
                target,
                0,
                0,
                &raw mut bytecode,
                Some(&raw mut errors),
            )
        };
        if let Err(err) = result {
            let detail = errors
                .as_ref()
                .map(blob_bytes)
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                .unwrap_or_default();
            return Err(format!("D3DCompile failed: {err}; {detail}"));
        }
        bytecode
            .as_ref()
            .map(blob_bytes)
            .map(Vec::from)
            .ok_or_else(|| "D3DCompile returned no bytecode".to_string())
    }

    fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
        unsafe { std::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize()) }
    }

    fn viewport_for_layout(layout: &GdiRenderStats) -> D3D11_VIEWPORT {
        let x = if layout.scale_mode == crate::win32_gdi_viewer::RenderScaleMode::Exact
            && layout.window_mode == crate::win32_gdi_viewer::WindowMode::BorderlessFullscreen
        {
            (i64::from(layout.client_width) - i64::from(layout.video_width)) as f32 / 2.0
        } else if layout.scale_mode == crate::win32_gdi_viewer::RenderScaleMode::Fit {
            (layout.client_width.saturating_sub(layout.draw_width)) as f32 / 2.0
        } else {
            0.0
        };
        let y = if layout.scale_mode == crate::win32_gdi_viewer::RenderScaleMode::Exact
            && layout.window_mode == crate::win32_gdi_viewer::WindowMode::BorderlessFullscreen
        {
            (i64::from(layout.client_height) - i64::from(layout.video_height)) as f32 / 2.0
        } else if layout.scale_mode == crate::win32_gdi_viewer::RenderScaleMode::Fit {
            (layout.client_height.saturating_sub(layout.draw_height)) as f32 / 2.0
        } else {
            0.0
        };
        D3D11_VIEWPORT {
            TopLeftX: x,
            TopLeftY: y,
            Width: layout.draw_width as f32,
            Height: layout.draw_height as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        }
    }

    fn average(total: f64, count: u64) -> f64 {
        if count == 0 {
            0.0
        } else {
            total / count as f64
        }
    }

    pub fn run_self_test() -> Result<(), String> {
        let vertex = compile_shader(VERTEX_SHADER, s!("main"), s!("vs_4_0"))?;
        let pixel = compile_shader(PIXEL_SHADER, s!("main"), s!("ps_4_0"))?;
        if vertex.is_empty() || pixel.is_empty() {
            return Err("D3D11 shader compilation returned empty bytecode".to_string());
        }
        Ok(())
    }

    pub use D3d11Nv12Renderer as Renderer;
}

#[cfg(windows)]
pub use platform::{run_self_test, D3d11RenderStats, Renderer as D3d11Nv12Renderer};
