#[cfg(windows)]
mod platform {
    use std::io::{self, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use windows::core::{factory, IInspectable, Interface, Ref, Result as WindowsResult, BOOL};
    use windows::Foundation::TypedEventHandler;
    use windows::Graphics::Capture::{
        Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
    };
    use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
    use windows::Graphics::DirectX::DirectXPixelFormat;
    use windows::Win32::Foundation::{HMODULE, POINT};
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
    };
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    use windows::Win32::Graphics::Gdi::{MonitorFromPoint, HMONITOR, MONITOR_DEFAULTTOPRIMARY};
    use windows::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
    };
    use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
    use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};

    const PIXEL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
    const PIXEL_FORMAT_NAME: &str = "B8G8R8A8";
    const FRAME_POOL_BUFFERS: i32 = 2;

    static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

    #[derive(Clone, Copy, Debug, Default)]
    struct CaptureCounters {
        raw_frames: u64,
        accepted_frames: u64,
        skipped_frames: u64,
        dropped: u64,
        width: i32,
        height: i32,
    }

    #[derive(Debug)]
    struct CaptureState {
        counters: CaptureCounters,
        pacing_started_at: Instant,
        target_fps: u32,
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

    unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> BOOL {
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

    pub fn run(duration_sec: u64, target_fps: u32) -> Result<(), String> {
        if duration_sec == 0 {
            return Err("duration-sec must be greater than zero".to_string());
        }
        if target_fps == 0 {
            return Err("target-fps must be greater than zero".to_string());
        }

        let _winrt = WinRtGuard::initialize()?;
        let _console_ctrl = ConsoleCtrlGuard::install()?;

        match GraphicsCaptureSession::IsSupported() {
            Ok(true) => {}
            Ok(false) => {
                return Err("Windows Graphics Capture is not supported on this system".to_string())
            }
            Err(err) => {
                eprintln!(
                    "GraphicsCaptureSession::IsSupported query failed; continuing with direct probe: {err}"
                );
            }
        }

        let (d3d_device, driver_name) = create_d3d11_device()?;
        let capture_item = create_primary_monitor_item()?;
        let initial_size = capture_item
            .Size()
            .map_err(|err| format!("GraphicsCaptureItem::Size failed: {err}"))?;
        if initial_size.Width <= 0 || initial_size.Height <= 0 {
            return Err(format!(
                "invalid primary monitor size: {}x{}",
                initial_size.Width, initial_size.Height
            ));
        }

        eprintln!(
            "capture-probe target=primary-monitor size={}x{} format={} d3d_driver={} target_fps={}",
            initial_size.Width, initial_size.Height, PIXEL_FORMAT_NAME, driver_name, target_fps
        );

        let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &d3d_device,
            PIXEL_FORMAT,
            FRAME_POOL_BUFFERS,
            initial_size,
        )
        .map_err(|err| format!("CreateFreeThreaded frame pool failed: {err}"))?;

        let counters = Arc::new(Mutex::new(CaptureState {
            counters: CaptureCounters {
                width: initial_size.Width,
                height: initial_size.Height,
                ..CaptureCounters::default()
            },
            pacing_started_at: Instant::now(),
            target_fps,
        }));
        let callback_counters = Arc::clone(&counters);
        let frame_handler: TypedEventHandler<Direct3D11CaptureFramePool, IInspectable> =
            TypedEventHandler::new(
                move |sender: Ref<Direct3D11CaptureFramePool>, _args: Ref<IInspectable>| {
                    let Some(pool) = sender.as_ref() else {
                        increment_dropped(&callback_counters);
                        return Ok(());
                    };

                    match pool.TryGetNextFrame() {
                        Ok(frame) => {
                            let frame_result = (|| -> WindowsResult<_> {
                                let size = frame.ContentSize()?;
                                let _surface = frame.Surface()?;
                                Ok(size)
                            })();

                            match frame_result {
                                Ok(size) => {
                                    if let Ok(mut state) = callback_counters.lock() {
                                        state.counters.raw_frames += 1;
                                        state.counters.width = size.Width;
                                        state.counters.height = size.Height;

                                        let elapsed =
                                            state.pacing_started_at.elapsed().as_secs_f64();
                                        let accepted_budget =
                                            (elapsed * f64::from(state.target_fps)).floor() as u64;
                                        if state.counters.accepted_frames < accepted_budget {
                                            state.counters.accepted_frames += 1;
                                        } else {
                                            state.counters.skipped_frames += 1;
                                        }
                                    }
                                }
                                Err(_) => increment_dropped(&callback_counters),
                            }
                            let _ = frame.Close();
                        }
                        Err(_) => increment_dropped(&callback_counters),
                    }
                    Ok(())
                },
            );
        let frame_token = frame_pool
            .FrameArrived(&frame_handler)
            .map_err(|err| format!("FrameArrived registration failed: {err}"))?;

        let session = frame_pool
            .CreateCaptureSession(&capture_item)
            .map_err(|err| format!("CreateCaptureSession failed: {err}"))?;
        session
            .StartCapture()
            .map_err(|err| format!("StartCapture failed: {err}"))?;

        let started_at = Instant::now();
        let mut report_at = started_at;
        let mut previous_raw_frames = 0u64;
        let mut previous_accepted_frames = 0u64;
        let mut previous_report_at = started_at;

        while started_at.elapsed() < Duration::from_secs(duration_sec)
            && !STOP_REQUESTED.load(Ordering::SeqCst)
        {
            thread::sleep(Duration::from_millis(10));
            let now = Instant::now();
            if now.duration_since(report_at) >= Duration::from_secs(1) {
                let snapshot = capture_snapshot(&counters);
                let elapsed = now
                    .duration_since(previous_report_at)
                    .as_secs_f64()
                    .max(0.001);
                let raw_fps =
                    snapshot.raw_frames.saturating_sub(previous_raw_frames) as f64 / elapsed;
                let accepted_fps = snapshot
                    .accepted_frames
                    .saturating_sub(previous_accepted_frames)
                    as f64
                    / elapsed;
                print_stats(snapshot, raw_fps, accepted_fps, target_fps);
                previous_raw_frames = snapshot.raw_frames;
                previous_accepted_frames = snapshot.accepted_frames;
                previous_report_at = now;
                report_at = now;
            }
        }

        let stopped_at = Instant::now();
        let final_elapsed = stopped_at.duration_since(previous_report_at).as_secs_f64();
        if final_elapsed >= 0.1 {
            let snapshot = capture_snapshot(&counters);
            let raw_fps = snapshot.raw_frames.saturating_sub(previous_raw_frames) as f64
                / final_elapsed.max(0.001);
            let accepted_fps = snapshot
                .accepted_frames
                .saturating_sub(previous_accepted_frames) as f64
                / final_elapsed.max(0.001);
            print_stats(snapshot, raw_fps, accepted_fps, target_fps);
        }

        let _ = frame_pool.RemoveFrameArrived(frame_token);
        let _ = session.Close();
        let _ = frame_pool.Close();
        eprintln!(
            "capture-probe stopped reason={}",
            if STOP_REQUESTED.load(Ordering::SeqCst) {
                "console-control"
            } else {
                "duration-complete"
            }
        );
        Ok(())
    }

    fn create_d3d11_device() -> Result<(IDirect3DDevice, &'static str), String> {
        match create_d3d11_device_with_driver(D3D_DRIVER_TYPE_HARDWARE) {
            Ok(device) => Ok((device, "hardware")),
            Err(hardware_error) => {
                eprintln!(
                    "hardware D3D11 device creation failed, trying WARP: {}",
                    hardware_error
                );
                create_d3d11_device_with_driver(D3D_DRIVER_TYPE_WARP)
                    .map(|device| (device, "warp"))
                    .map_err(|warp_error| {
                        format!(
                            "D3D11 device creation failed; hardware={hardware_error}; warp={warp_error}"
                        )
                    })
            }
        }
    }

    fn create_d3d11_device_with_driver(
        driver_type: windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE,
    ) -> Result<IDirect3DDevice, String> {
        let mut device: Option<ID3D11Device> = None;
        unsafe {
            D3D11CreateDevice(
                None,
                driver_type,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        }
        .map_err(|err| format!("D3D11CreateDevice failed: {err}"))?;

        let device = device.ok_or_else(|| "D3D11CreateDevice returned no device".to_string())?;
        let dxgi_device: IDXGIDevice = device
            .cast()
            .map_err(|err| format!("ID3D11Device to IDXGIDevice cast failed: {err}"))?;
        let inspectable: IInspectable =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device) }
                .map_err(|err| format!("CreateDirect3D11DeviceFromDXGIDevice failed: {err}"))?;
        inspectable
            .cast::<IDirect3DDevice>()
            .map_err(|err| format!("IInspectable to IDirect3DDevice cast failed: {err}"))
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

    fn increment_dropped(counters: &Arc<Mutex<CaptureState>>) {
        if let Ok(mut state) = counters.lock() {
            state.counters.dropped += 1;
        }
    }

    fn capture_snapshot(counters: &Arc<Mutex<CaptureState>>) -> CaptureCounters {
        counters
            .lock()
            .map(|state| state.counters)
            .unwrap_or_default()
    }

    fn print_stats(snapshot: CaptureCounters, raw_fps: f64, accepted_fps: f64, target_fps: u32) {
        println!(
            r#"{{"type":"CAPTURE_STATS","mode":"capture_probe","raw_frames":{},"accepted_frames":{},"skipped_frames":{},"raw_fps":{:.2},"accepted_fps":{:.2},"target_fps":{},"width":{},"height":{},"format":"{}","dropped":{}}}"#,
            snapshot.raw_frames,
            snapshot.accepted_frames,
            snapshot.skipped_frames,
            raw_fps,
            accepted_fps,
            target_fps,
            snapshot.width,
            snapshot.height,
            PIXEL_FORMAT_NAME,
            snapshot.dropped
        );
        io::stdout().flush().ok();
    }
}

#[cfg(windows)]
pub use platform::run;

#[cfg(not(windows))]
pub fn run(_duration_sec: u64, _target_fps: u32) -> Result<(), String> {
    Err("capture-probe is only supported on Windows".to_string())
}
