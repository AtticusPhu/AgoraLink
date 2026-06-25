#[cfg(windows)]
mod platform {
    use std::slice;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, SyncSender, TrySendError};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use windows::core::{factory, IInspectable, Interface, Ref};
    use windows::Foundation::TypedEventHandler;
    use windows::Graphics::Capture::{
        Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
        GraphicsCaptureSession,
    };
    use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
    use windows::Graphics::DirectX::DirectXPixelFormat;
    use windows::Win32::Foundation::{HMODULE, POINT};
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
        D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
        D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
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
    pub struct CaptureInfo {
        pub width: u32,
        pub height: u32,
        pub driver_name: &'static str,
    }

    #[derive(Clone, Copy, Debug, Default)]
    pub struct LatestCaptureStats {
        pub raw_frames: u64,
        pub latest_updates: u64,
        pub callback_skipped: u64,
        pub dropped: u64,
        pub copy_ms_total: f64,
    }

    #[derive(Debug)]
    pub struct CapturedBgraFrame {
        pub version: u64,
        pub bgra: Vec<u8>,
        pub row_pitch: usize,
        pub width: u32,
        pub height: u32,
    }

    #[derive(Default)]
    struct SharedState {
        latest: Option<Arc<CapturedBgraFrame>>,
        stats: LatestCaptureStats,
        error: Option<String>,
    }

    pub struct LatestCapture {
        info: CaptureInfo,
        shared: Arc<Mutex<SharedState>>,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl LatestCapture {
        pub fn start() -> Result<Self, String> {
            let shared = Arc::new(Mutex::new(SharedState::default()));
            let stop = Arc::new(AtomicBool::new(false));
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let thread_shared = Arc::clone(&shared);
            let thread_stop = Arc::clone(&stop);
            let thread = thread::Builder::new()
                .name("agoralink-wgc-capture".to_string())
                .spawn(move || {
                    if let Err(err) = capture_thread(thread_shared.clone(), thread_stop, ready_tx) {
                        if let Ok(mut state) = thread_shared.lock() {
                            state.error = Some(err);
                        }
                    }
                })
                .map_err(|err| format!("spawn WGC capture thread failed: {err}"))?;
            let info = ready_rx
                .recv_timeout(Duration::from_secs(5))
                .map_err(|err| format!("WGC capture initialization timed out: {err}"))??;
            Ok(Self {
                info,
                shared,
                stop,
                thread: Some(thread),
            })
        }

        pub fn info(&self) -> CaptureInfo {
            self.info
        }

        pub fn latest(&self) -> Option<Arc<CapturedBgraFrame>> {
            self.shared
                .lock()
                .ok()
                .and_then(|state| state.latest.clone())
        }

        pub fn stats(&self) -> LatestCaptureStats {
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
                    .map_err(|_| "WGC capture thread panicked".to_string())?;
            }
            if let Some(error) = self.error() {
                Err(error)
            } else {
                Ok(())
            }
        }
    }

    impl Drop for LatestCapture {
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

    struct ReadbackState {
        staging: Option<ID3D11Texture2D>,
        desc: Option<D3D11_TEXTURE2D_DESC>,
    }

    fn capture_thread(
        shared: Arc<Mutex<SharedState>>,
        stop: Arc<AtomicBool>,
        ready: SyncSender<Result<CaptureInfo, String>>,
    ) -> Result<(), String> {
        let _winrt = match WinRtGuard::initialize() {
            Ok(guard) => guard,
            Err(err) => {
                let _ = ready.send(Err(err.clone()));
                return Err(err);
            }
        };
        let setup = setup_capture();
        let (devices, frame_pool, session, info) = match setup {
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
            .send(Ok(info))
            .map_err(|_| "capture initialization receiver disconnected".to_string())?;

        let mut readback = ReadbackState {
            staging: None,
            desc: None,
        };
        while !stop.load(Ordering::SeqCst) {
            match frame_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(frame) => {
                    let result = readback_frame(&frame, &devices, &mut readback);
                    let _ = frame.Close();
                    match result {
                        Ok((bgra, row_pitch, width, height, copy_ms)) => {
                            if let Ok(mut state) = shared.lock() {
                                state.stats.latest_updates += 1;
                                state.stats.copy_ms_total += copy_ms;
                                let version = state.stats.latest_updates;
                                state.latest = Some(Arc::new(CapturedBgraFrame {
                                    version,
                                    bgra,
                                    row_pitch,
                                    width,
                                    height,
                                }));
                            }
                        }
                        Err(err) => {
                            if let Ok(mut state) = shared.lock() {
                                state.stats.dropped += 1;
                            }
                            eprintln!("WGC latest-frame readback failed: {err}");
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
            CaptureInfo,
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
        let info = CaptureInfo {
            width: size.Width as u32,
            height: size.Height as u32,
            driver_name: devices.driver_name,
        };
        Ok((devices, frame_pool, session, info))
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

    fn readback_frame(
        frame: &Direct3D11CaptureFrame,
        devices: &D3dDevices,
        readback: &mut ReadbackState,
    ) -> Result<(Vec<u8>, usize, u32, u32, f64), String> {
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
        ensure_staging_texture(devices, readback, desc)?;
        let staging = readback
            .staging
            .as_ref()
            .ok_or_else(|| "staging texture was not created".to_string())?;
        let started = Instant::now();
        unsafe { devices.context.CopyResource(staging, &texture) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            devices
                .context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }
        .map_err(|err| format!("staging texture Map failed: {err}"))?;
        let row_pitch = mapped.RowPitch as usize;
        let mapped_len = row_pitch
            .checked_mul(desc.Height as usize)
            .ok_or_else(|| "mapped texture length overflow".to_string())?;
        let bytes = if mapped.pData.is_null() {
            Err("mapped texture returned null data".to_string())
        } else {
            Ok(unsafe { slice::from_raw_parts(mapped.pData.cast::<u8>(), mapped_len) }.to_vec())
        };
        unsafe { devices.context.Unmap(staging, 0) };
        Ok((
            bytes?,
            row_pitch,
            desc.Width,
            desc.Height,
            started.elapsed().as_secs_f64() * 1000.0,
        ))
    }

    fn ensure_staging_texture(
        devices: &D3dDevices,
        readback: &mut ReadbackState,
        source_desc: D3D11_TEXTURE2D_DESC,
    ) -> Result<(), String> {
        let recreate = readback.desc.is_none_or(|existing| {
            existing.Width != source_desc.Width
                || existing.Height != source_desc.Height
                || existing.Format != source_desc.Format
        });
        if !recreate {
            return Ok(());
        }
        let mut staging_desc = source_desc;
        staging_desc.Usage = D3D11_USAGE_STAGING;
        staging_desc.BindFlags = 0;
        staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        staging_desc.MiscFlags = 0;
        let mut staging = None;
        unsafe {
            devices
                .native
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))
        }
        .map_err(|err| format!("CreateTexture2D staging failed: {err}"))?;
        readback.staging =
            Some(staging.ok_or_else(|| "CreateTexture2D returned no staging texture".to_string())?);
        readback.desc = Some(source_desc);
        Ok(())
    }

    fn create_d3d11_devices() -> Result<D3dDevices, String> {
        match create_d3d11_devices_with_driver(D3D_DRIVER_TYPE_HARDWARE) {
            Ok(devices) => Ok(D3dDevices {
                driver_name: "hardware",
                ..devices
            }),
            Err(hardware_error) => {
                eprintln!("hardware D3D11 device creation failed, trying WARP: {hardware_error}");
                create_d3d11_devices_with_driver(D3D_DRIVER_TYPE_WARP)
                    .map(|devices| D3dDevices {
                        driver_name: "warp",
                        ..devices
                    })
                    .map_err(|warp_error| {
                        format!(
                            "D3D11 device creation failed; hardware={hardware_error}; warp={warp_error}"
                        )
                    })
            }
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
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
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
            Err(err) => {
                eprintln!(
                    "GraphicsCaptureSession::IsSupported query failed; continuing with direct probe: {err}"
                );
                Ok(())
            }
        }
    }

    fn increment_dropped(shared: &Arc<Mutex<SharedState>>) {
        if let Ok(mut state) = shared.lock() {
            state.stats.dropped += 1;
        }
    }
}

#[cfg(windows)]
pub use platform::{LatestCapture, LatestCaptureStats};
