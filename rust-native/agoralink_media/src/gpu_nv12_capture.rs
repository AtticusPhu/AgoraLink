#[cfg(windows)]
mod platform {
    use std::mem::ManuallyDrop;
    use std::slice;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, SyncSender, TrySendError};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use crate::color_spec::{ColorMatrix, ColorSpec};
    use windows::core::{factory, IInspectable, Interface, Ref};
    use windows::Foundation::TypedEventHandler;
    use windows::Graphics::Capture::{
        Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
        GraphicsCaptureSession,
    };
    use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
    use windows::Graphics::DirectX::DirectXPixelFormat;
    use windows::Win32::Foundation::{HMODULE, POINT, RECT};
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoContext,
        ID3D11VideoContext1, ID3D11VideoDevice, ID3D11VideoProcessor,
        ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorEnumerator1,
        ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView, D3D11_BIND_RENDER_TARGET,
        D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
        D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEX2D_VPIV,
        D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
        D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
        D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT,
        D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
        D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
        D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM_SRGB, DXGI_FORMAT_NV12,
        DXGI_RATIONAL, DXGI_SAMPLE_DESC,
    };
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    use windows::Win32::Graphics::Gdi::{MonitorFromPoint, HMONITOR, MONITOR_DEFAULTTOPRIMARY};
    use windows::Win32::System::WinRT::Direct3D11::{
        CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
    };
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
    use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};

    const PIXEL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
    const FRAME_POOL_BUFFERS: i32 = 2;
    const FRAME_QUEUE_CAPACITY: usize = 2;

    #[derive(Clone, Copy, Debug)]
    pub struct GpuCaptureInfo {
        pub source_width: u32,
        pub source_height: u32,
        pub output_width: u32,
        pub output_height: u32,
        pub driver_name: &'static str,
        pub color_space_api: &'static str,
    }

    #[derive(Clone, Copy, Debug, Default)]
    pub struct GpuCaptureStats {
        pub raw_frames: u64,
        pub latest_updates: u64,
        pub callback_skipped: u64,
        pub pacing_skipped: u64,
        pub dropped: u64,
        pub gpu_convert_ms_total: f64,
        pub copy_ms_total: f64,
    }

    #[derive(Clone, Copy, Debug, Default)]
    pub struct Nv12ReadbackLayout {
        pub y_stride: usize,
        pub uv_stride: usize,
        pub uv_offset: usize,
        pub allocated_height: u32,
        pub visible_width: u32,
        pub visible_height: u32,
    }

    #[derive(Debug)]
    pub struct CapturedNv12Frame {
        pub version: u64,
        pub nv12: Vec<u8>,
        pub layout: Nv12ReadbackLayout,
        pub gpu_convert_ms: f64,
        pub copy_ms: f64,
    }

    #[derive(Default)]
    struct SharedState {
        latest: Option<Arc<CapturedNv12Frame>>,
        stats: GpuCaptureStats,
        error: Option<String>,
    }

    pub struct GpuNv12Capture {
        info: GpuCaptureInfo,
        shared: Arc<Mutex<SharedState>>,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl GpuNv12Capture {
        pub fn start(
            output_width: u32,
            output_height: u32,
            target_fps: u32,
            color_spec: ColorSpec,
        ) -> Result<Self, String> {
            validate_output(output_width, output_height, target_fps, color_spec)?;
            let shared = Arc::new(Mutex::new(SharedState::default()));
            let stop = Arc::new(AtomicBool::new(false));
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let thread_shared = Arc::clone(&shared);
            let thread_stop = Arc::clone(&stop);
            let thread = thread::Builder::new()
                .name("agoralink-wgc-gpu-nv12".to_string())
                .spawn(move || {
                    if let Err(err) = capture_thread(
                        thread_shared.clone(),
                        thread_stop,
                        ready_tx,
                        output_width,
                        output_height,
                        target_fps,
                    ) {
                        if let Ok(mut state) = thread_shared.lock() {
                            state.error = Some(err);
                        }
                    }
                })
                .map_err(|err| format!("spawn GPU capture thread failed: {err}"))?;
            let info = ready_rx
                .recv_timeout(Duration::from_secs(5))
                .map_err(|err| format!("GPU capture initialization timed out: {err}"))??;
            Ok(Self {
                info,
                shared,
                stop,
                thread: Some(thread),
            })
        }

        pub fn info(&self) -> GpuCaptureInfo {
            self.info
        }

        pub fn latest(&self) -> Option<Arc<CapturedNv12Frame>> {
            self.shared
                .lock()
                .ok()
                .and_then(|state| state.latest.clone())
        }

        pub fn stats(&self) -> GpuCaptureStats {
            self.shared
                .lock()
                .map(|state| state.stats)
                .unwrap_or_default()
        }

        pub fn error(&self) -> Option<String> {
            self.shared
                .lock()
                .ok()
                .and_then(|state| state.error.clone())
        }

        pub fn stop(mut self) -> Result<(), String> {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                thread
                    .join()
                    .map_err(|_| "GPU capture thread panicked".to_string())?;
            }
            if let Some(error) = self.error() {
                Err(error)
            } else {
                Ok(())
            }
        }
    }

    impl Drop for GpuNv12Capture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    struct WinRtGuard;

    impl WinRtGuard {
        fn initialize() -> Result<Self, String> {
            unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
                .map_err(|err| format!("RoInitialize failed: {err}"))?;
            Ok(Self)
        }
    }

    impl Drop for WinRtGuard {
        fn drop(&mut self) {
            unsafe { RoUninitialize() };
        }
    }

    struct D3dDevices {
        native: ID3D11Device,
        context: ID3D11DeviceContext,
        winrt: IDirect3DDevice,
        driver_name: &'static str,
    }

    struct D3dVideoConverter {
        devices: D3dDevices,
        video_device: ID3D11VideoDevice,
        video_context: ID3D11VideoContext,
        video_context1: ID3D11VideoContext1,
        enumerator: ID3D11VideoProcessorEnumerator,
        processor: ID3D11VideoProcessor,
        output_texture: ID3D11Texture2D,
        output_view: ID3D11VideoProcessorOutputView,
        staging_texture: ID3D11Texture2D,
        source_width: u32,
        source_height: u32,
        output_width: u32,
        output_height: u32,
    }

    impl D3dVideoConverter {
        fn new(
            devices: D3dDevices,
            source_width: u32,
            source_height: u32,
            output_width: u32,
            output_height: u32,
            target_fps: u32,
        ) -> Result<Self, String> {
            let video_device: ID3D11VideoDevice = devices
                .native
                .cast()
                .map_err(|err| format!("ID3D11VideoDevice unavailable: {err}"))?;
            let video_context: ID3D11VideoContext = devices
                .context
                .cast()
                .map_err(|err| format!("ID3D11VideoContext unavailable: {err}"))?;
            let video_context1: ID3D11VideoContext1 = devices.context.cast().map_err(|err| {
                format!("ID3D11VideoContext1 required for explicit Rec.709 color spaces: {err}")
            })?;
            let content_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL {
                    Numerator: target_fps,
                    Denominator: 1,
                },
                InputWidth: source_width,
                InputHeight: source_height,
                OutputFrameRate: DXGI_RATIONAL {
                    Numerator: target_fps,
                    Denominator: 1,
                },
                OutputWidth: output_width,
                OutputHeight: output_height,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };
            let enumerator =
                unsafe { video_device.CreateVideoProcessorEnumerator(&raw const content_desc) }
                    .map_err(|err| format!("CreateVideoProcessorEnumerator failed: {err}"))?;
            check_format_support(&enumerator)?;
            let processor = unsafe { video_device.CreateVideoProcessor(&enumerator, 0) }
                .map_err(|err| format!("CreateVideoProcessor failed: {err}"))?;
            let output_texture = create_output_texture(
                &devices.native,
                output_width,
                output_height,
                D3D11_USAGE_DEFAULT,
                D3D11_BIND_RENDER_TARGET.0 as u32,
                0,
            )?;
            let staging_texture = create_output_texture(
                &devices.native,
                output_width,
                output_height,
                D3D11_USAGE_STAGING,
                0,
                D3D11_CPU_ACCESS_READ.0 as u32,
            )?;
            let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let mut output_view = None;
            unsafe {
                video_device.CreateVideoProcessorOutputView(
                    &output_texture,
                    &enumerator,
                    &raw const output_desc,
                    Some(&mut output_view),
                )
            }
            .map_err(|err| format!("CreateVideoProcessorOutputView NV12 failed: {err}"))?;
            let output_view = output_view
                .ok_or_else(|| "CreateVideoProcessorOutputView returned no view".to_string())?;

            unsafe {
                video_context.VideoProcessorSetStreamFrameFormat(
                    &processor,
                    0,
                    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                );
                video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
                video_context1.VideoProcessorSetStreamColorSpace1(
                    &processor,
                    0,
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                );
                video_context1.VideoProcessorSetOutputColorSpace1(
                    &processor,
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                );
            }

            Ok(Self {
                devices,
                video_device,
                video_context,
                video_context1,
                enumerator,
                processor,
                output_texture,
                output_view,
                staging_texture,
                source_width,
                source_height,
                output_width,
                output_height,
            })
        }

        fn convert(
            &self,
            texture: &ID3D11Texture2D,
        ) -> Result<(Vec<u8>, Nv12ReadbackLayout, f64, f64), String> {
            let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: 0,
                    },
                },
            };
            let mut input_view: Option<ID3D11VideoProcessorInputView> = None;
            unsafe {
                self.video_device.CreateVideoProcessorInputView(
                    texture,
                    &self.enumerator,
                    &raw const input_desc,
                    Some(&mut input_view),
                )
            }
            .map_err(|err| format!("CreateVideoProcessorInputView failed: {err}"))?;
            let input_view = input_view
                .ok_or_else(|| "CreateVideoProcessorInputView returned no view".to_string())?;
            let source_rect = RECT {
                left: 0,
                top: 0,
                right: self.source_width as i32,
                bottom: self.source_height as i32,
            };
            let dest_rect = RECT {
                left: 0,
                top: 0,
                right: self.output_width as i32,
                bottom: self.output_height as i32,
            };
            unsafe {
                self.video_context.VideoProcessorSetStreamSourceRect(
                    &self.processor,
                    0,
                    true,
                    Some(&raw const source_rect),
                );
                self.video_context.VideoProcessorSetStreamDestRect(
                    &self.processor,
                    0,
                    true,
                    Some(&raw const dest_rect),
                );
                self.video_context1.VideoProcessorSetStreamColorSpace1(
                    &self.processor,
                    0,
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                );
                self.video_context1.VideoProcessorSetOutputColorSpace1(
                    &self.processor,
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                );
            }
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(Some(input_view)),
                ..Default::default()
            };
            let convert_started = Instant::now();
            let blt_result = unsafe {
                self.video_context.VideoProcessorBlt(
                    &self.processor,
                    &self.output_view,
                    0,
                    slice::from_ref(&stream),
                )
            };
            unsafe { ManuallyDrop::drop(&mut stream.pInputSurface) };
            blt_result.map_err(|err| format!("VideoProcessorBlt BGRA->NV12 failed: {err}"))?;
            let submit_ms = convert_started.elapsed().as_secs_f64() * 1000.0;

            let copy_started = Instant::now();
            unsafe {
                self.devices
                    .context
                    .CopyResource(&self.staging_texture, &self.output_texture)
            };
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            unsafe {
                self.devices.context.Map(
                    &self.staging_texture,
                    0,
                    D3D11_MAP_READ,
                    0,
                    Some(&mut mapped),
                )
            }
            .map_err(|err| format!("NV12 staging Map failed: {err}"))?;
            let readback = read_mapped_nv12(&mapped, self.output_width, self.output_height);
            unsafe { self.devices.context.Unmap(&self.staging_texture, 0) };
            let copy_ms = copy_started.elapsed().as_secs_f64() * 1000.0;
            let (nv12, layout) = readback?;
            Ok((nv12, layout, submit_ms, copy_ms))
        }
    }

    fn capture_thread(
        shared: Arc<Mutex<SharedState>>,
        stop: Arc<AtomicBool>,
        ready: SyncSender<Result<GpuCaptureInfo, String>>,
        output_width: u32,
        output_height: u32,
        target_fps: u32,
    ) -> Result<(), String> {
        let _winrt = match WinRtGuard::initialize() {
            Ok(guard) => guard,
            Err(err) => {
                let _ = ready.send(Err(err.clone()));
                return Err(err);
            }
        };
        let setup = setup_capture();
        let (devices, frame_pool, session, source_width, source_height) = match setup {
            Ok(value) => value,
            Err(err) => {
                let _ = ready.send(Err(err.clone()));
                return Err(err);
            }
        };
        let driver_name = devices.driver_name;
        let converter = match D3dVideoConverter::new(
            devices,
            source_width,
            source_height,
            output_width,
            output_height,
            target_fps,
        ) {
            Ok(value) => value,
            Err(err) => {
                let _ = ready.send(Err(err.clone()));
                return Err(err);
            }
        };
        let (frame_tx, frame_rx) = mpsc::sync_channel(FRAME_QUEUE_CAPACITY);
        let callback_shared = Arc::clone(&shared);
        let frame_handler = create_frame_handler(callback_shared, frame_tx);
        let frame_token = frame_pool
            .FrameArrived(&frame_handler)
            .map_err(|err| format!("FrameArrived registration failed: {err}"))?;
        session
            .StartCapture()
            .map_err(|err| format!("StartCapture failed: {err}"))?;
        ready
            .send(Ok(GpuCaptureInfo {
                source_width,
                source_height,
                output_width,
                output_height,
                driver_name,
                color_space_api: "ID3D11VideoContext1",
            }))
            .map_err(|_| "GPU capture initialization receiver disconnected".to_string())?;

        let frame_interval = Duration::from_nanos(1_000_000_000u64 / u64::from(target_fps));
        let mut next_convert_at = Instant::now();
        while !stop.load(Ordering::SeqCst) {
            match frame_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(frame) => {
                    let now = Instant::now();
                    if now < next_convert_at {
                        let _ = frame.Close();
                        if let Ok(mut state) = shared.lock() {
                            state.stats.pacing_skipped += 1;
                        }
                        continue;
                    }
                    next_convert_at += frame_interval;
                    if now > next_convert_at + frame_interval {
                        next_convert_at = now + frame_interval;
                    }
                    let result =
                        frame_texture(&frame).and_then(|texture| converter.convert(&texture));
                    let _ = frame.Close();
                    match result {
                        Ok((nv12, layout, gpu_convert_ms, copy_ms)) => {
                            if let Ok(mut state) = shared.lock() {
                                state.stats.latest_updates += 1;
                                state.stats.gpu_convert_ms_total += gpu_convert_ms;
                                state.stats.copy_ms_total += copy_ms;
                                let version = state.stats.latest_updates;
                                state.latest = Some(Arc::new(CapturedNv12Frame {
                                    version,
                                    nv12,
                                    layout,
                                    gpu_convert_ms,
                                    copy_ms,
                                }));
                            }
                        }
                        Err(err) => {
                            if let Ok(mut state) = shared.lock() {
                                state.stats.dropped += 1;
                            }
                            eprintln!("WGC GPU NV12 conversion failed: {err}");
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        let _ = frame_pool.RemoveFrameArrived(frame_token);
        let _ = session.Close();
        let _ = frame_pool.Close();
        Ok(())
    }

    fn setup_capture() -> Result<
        (
            D3dDevices,
            Direct3D11CaptureFramePool,
            GraphicsCaptureSession,
            u32,
            u32,
        ),
        String,
    > {
        check_capture_support()?;
        let devices = create_d3d11_devices()?;
        let capture_item = create_primary_monitor_item()?;
        let size = capture_item
            .Size()
            .map_err(|err| format!("GraphicsCaptureItem::Size failed: {err}"))?;
        if size.Width <= 0 || size.Height <= 0 {
            return Err(format!(
                "invalid capture size: {}x{}",
                size.Width, size.Height
            ));
        }
        let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &devices.winrt,
            PIXEL_FORMAT,
            FRAME_POOL_BUFFERS,
            size,
        )
        .map_err(|err| format!("CreateFreeThreaded frame pool failed: {err}"))?;
        let session = frame_pool
            .CreateCaptureSession(&capture_item)
            .map_err(|err| format!("CreateCaptureSession failed: {err}"))?;
        Ok((
            devices,
            frame_pool,
            session,
            size.Width as u32,
            size.Height as u32,
        ))
    }

    fn create_frame_handler(
        shared: Arc<Mutex<SharedState>>,
        sender: SyncSender<Direct3D11CaptureFrame>,
    ) -> TypedEventHandler<Direct3D11CaptureFramePool, IInspectable> {
        TypedEventHandler::new(
            move |pool: Ref<Direct3D11CaptureFramePool>, _args: Ref<IInspectable>| {
                let Some(pool) = pool.as_ref() else {
                    increment_dropped(&shared);
                    return Ok(());
                };
                match pool.TryGetNextFrame() {
                    Ok(frame) => {
                        if let Ok(mut state) = shared.lock() {
                            state.stats.raw_frames += 1;
                        }
                        match sender.try_send(frame) {
                            Ok(()) => {}
                            Err(TrySendError::Full(frame))
                            | Err(TrySendError::Disconnected(frame)) => {
                                let _ = frame.Close();
                                if let Ok(mut state) = shared.lock() {
                                    state.stats.callback_skipped += 1;
                                }
                            }
                        }
                    }
                    Err(_) => increment_dropped(&shared),
                }
                Ok(())
            },
        )
    }

    fn frame_texture(frame: &Direct3D11CaptureFrame) -> Result<ID3D11Texture2D, String> {
        let surface = frame
            .Surface()
            .map_err(|err| format!("frame Surface failed: {err}"))?;
        let access: IDirect3DDxgiInterfaceAccess = surface
            .cast()
            .map_err(|err| format!("surface DXGI access cast failed: {err}"))?;
        let texture: ID3D11Texture2D = unsafe { access.GetInterface() }
            .map_err(|err| format!("surface texture access failed: {err}"))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        if !matches!(
            desc.Format,
            DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM_SRGB
        ) {
            return Err(format!(
                "unexpected captured DXGI format: {:?}",
                desc.Format
            ));
        }
        Ok(texture)
    }

    fn check_format_support(enumerator: &ID3D11VideoProcessorEnumerator) -> Result<(), String> {
        let input_support =
            unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM) }
                .map_err(|err| format!("check BGRA VideoProcessor support failed: {err}"))?;
        if input_support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 == 0 {
            return Err("D3D11 VideoProcessor does not support BGRA input".to_string());
        }
        let output_support = unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12) }
            .map_err(|err| format!("check NV12 VideoProcessor support failed: {err}"))?;
        if output_support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 == 0 {
            return Err("D3D11 VideoProcessor does not support NV12 output".to_string());
        }
        let enumerator1: ID3D11VideoProcessorEnumerator1 = enumerator.cast().map_err(|err| {
            format!("ID3D11VideoProcessorEnumerator1 is required for color conversion check: {err}")
        })?;
        let supported = unsafe {
            enumerator1.CheckVideoProcessorFormatConversion(
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                DXGI_FORMAT_NV12,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            )
        }
        .map_err(|err| format!("check Rec.709 BGRA->NV12 conversion failed: {err}"))?;
        if !supported.as_bool() {
            return Err(
                "D3D11 VideoProcessor does not support Rec.709 BGRA->NV12 conversion".to_string(),
            );
        }
        Ok(())
    }

    fn create_output_texture(
        device: &ID3D11Device,
        width: u32,
        height: u32,
        usage: windows::Win32::Graphics::Direct3D11::D3D11_USAGE,
        bind_flags: u32,
        cpu_access_flags: u32,
    ) -> Result<ID3D11Texture2D, String> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: usage,
            BindFlags: bind_flags,
            CPUAccessFlags: cpu_access_flags,
            MiscFlags: 0,
        };
        let mut texture = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }
            .map_err(|err| format!("CreateTexture2D NV12 failed: {err}"))?;
        texture.ok_or_else(|| "CreateTexture2D NV12 returned no texture".to_string())
    }

    fn read_mapped_nv12(
        mapped: &D3D11_MAPPED_SUBRESOURCE,
        width: u32,
        height: u32,
    ) -> Result<(Vec<u8>, Nv12ReadbackLayout), String> {
        if mapped.pData.is_null() {
            return Err("mapped NV12 texture returned null data".to_string());
        }
        let row_pitch = mapped.RowPitch as usize;
        if row_pitch < width as usize {
            return Err(format!(
                "NV12 row pitch {row_pitch} is smaller than width {width}"
            ));
        }
        let allocated_height = height;
        let uv_offset = row_pitch
            .checked_mul(allocated_height as usize)
            .ok_or_else(|| "NV12 UV offset overflow".to_string())?;
        let mapped_len = uv_offset
            .checked_add(row_pitch * (height as usize / 2))
            .ok_or_else(|| "NV12 mapped length overflow".to_string())?;
        let source = unsafe { slice::from_raw_parts(mapped.pData.cast::<u8>(), mapped_len) };
        let tight_len = width as usize * height as usize * 3 / 2;
        let mut tight = vec![0u8; tight_len];
        for row in 0..height as usize {
            let source_start = row * row_pitch;
            let target_start = row * width as usize;
            tight[target_start..target_start + width as usize]
                .copy_from_slice(&source[source_start..source_start + width as usize]);
        }
        let tight_uv_offset = width as usize * height as usize;
        for row in 0..height as usize / 2 {
            let source_start = uv_offset + row * row_pitch;
            let target_start = tight_uv_offset + row * width as usize;
            tight[target_start..target_start + width as usize]
                .copy_from_slice(&source[source_start..source_start + width as usize]);
        }
        Ok((
            tight,
            Nv12ReadbackLayout {
                y_stride: row_pitch,
                uv_stride: row_pitch,
                uv_offset,
                allocated_height,
                visible_width: width,
                visible_height: height,
            },
        ))
    }

    fn create_d3d11_devices() -> Result<D3dDevices, String> {
        match create_d3d11_devices_with_driver(D3D_DRIVER_TYPE_HARDWARE) {
            Ok(devices) => Ok(D3dDevices {
                driver_name: "hardware",
                ..devices
            }),
            Err(hardware_error) => create_d3d11_devices_with_driver(D3D_DRIVER_TYPE_WARP)
                .map(|devices| D3dDevices {
                    driver_name: "warp",
                    ..devices
                })
                .map_err(|warp_error| {
                    format!(
                        "D3D11 device creation failed; hardware={hardware_error}; warp={warp_error}"
                    )
                }),
        }
    }

    fn create_d3d11_devices_with_driver(
        driver_type: windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE,
    ) -> Result<D3dDevices, String> {
        let mut native = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                None,
                driver_type,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut native),
                None,
                Some(&mut context),
            )
        }
        .map_err(|err| format!("D3D11CreateDevice failed: {err}"))?;
        let native = native.ok_or_else(|| "D3D11CreateDevice returned no device".to_string())?;
        let context = context.ok_or_else(|| "D3D11CreateDevice returned no context".to_string())?;
        let dxgi: IDXGIDevice = native
            .cast()
            .map_err(|err| format!("ID3D11Device to IDXGIDevice cast failed: {err}"))?;
        let inspectable: IInspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
            .map_err(|err| format!("CreateDirect3D11DeviceFromDXGIDevice failed: {err}"))?;
        let winrt = inspectable
            .cast::<IDirect3DDevice>()
            .map_err(|err| format!("IInspectable to IDirect3DDevice cast failed: {err}"))?;
        Ok(D3dDevices {
            native,
            context,
            winrt,
            driver_name: "",
        })
    }

    fn create_primary_monitor_item() -> Result<GraphicsCaptureItem, String> {
        let monitor: HMONITOR =
            unsafe { MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY) };
        if monitor.is_invalid() {
            return Err("MonitorFromPoint did not return the primary monitor".to_string());
        }
        let interop: IGraphicsCaptureItemInterop =
            factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
                .map_err(|err| format!("GraphicsCaptureItem factory failed: {err}"))?;
        unsafe { interop.CreateForMonitor::<GraphicsCaptureItem>(monitor) }
            .map_err(|err| format!("CreateForMonitor failed: {err}"))
    }

    fn check_capture_support() -> Result<(), String> {
        match GraphicsCaptureSession::IsSupported() {
            Ok(true) => Ok(()),
            Ok(false) => Err("Windows Graphics Capture is not supported".to_string()),
            Err(_) => Ok(()),
        }
    }

    fn validate_output(
        width: u32,
        height: u32,
        target_fps: u32,
        color_spec: ColorSpec,
    ) -> Result<(), String> {
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            return Err("GPU output width and height must be non-zero even values".to_string());
        }
        if target_fps == 0 {
            return Err("GPU target-fps must be greater than zero".to_string());
        }
        if color_spec.matrix != ColorMatrix::Bt709 {
            return Err("D3D11 GPU conversion currently requires bt709".to_string());
        }
        Ok(())
    }

    fn increment_dropped(shared: &Arc<Mutex<SharedState>>) {
        if let Ok(mut state) = shared.lock() {
            state.stats.dropped += 1;
        }
    }
}

#[cfg(windows)]
pub use platform::{CapturedNv12Frame, GpuCaptureStats, GpuNv12Capture, Nv12ReadbackLayout};

#[cfg(not(windows))]
pub struct GpuNv12Capture;

#[cfg(not(windows))]
impl GpuNv12Capture {
    pub fn start(
        _output_width: u32,
        _output_height: u32,
        _target_fps: u32,
        _color_spec: crate::color_spec::ColorSpec,
    ) -> Result<Self, String> {
        Err("D3D11 GPU conversion is only supported on Windows".to_string())
    }
}
