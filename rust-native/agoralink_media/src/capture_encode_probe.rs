#[derive(Debug)]
pub struct CaptureEncodeConfig {
    pub duration_sec: u64,
    pub target_fps: u32,
    pub bitrate_mbps: f64,
    pub output: String,
}

#[cfg(windows)]
mod platform {
    use std::fs::File;
    use std::io::{self, Write};
    use std::path::Path;
    use std::slice;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use windows::core::{factory, IInspectable, Interface, Ref, BOOL};
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
    use windows::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
    };
    use windows::Win32::System::WinRT::Direct3D11::{
        CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
    };
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
    use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};

    use super::CaptureEncodeConfig;
    use crate::bgra_to_nv12;
    use crate::wmf_h264_encoder::{EncodedSample, EncoderStats, WmfH264Encoder, ENCODER_NAME};

    const PIXEL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
    const PIXEL_FORMAT_NAME: &str = "B8G8R8A8";
    const FRAME_POOL_BUFFERS: i32 = 2;
    const FRAME_QUEUE_CAPACITY: usize = 2;

    static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

    #[derive(Clone, Copy, Debug, Default)]
    struct CaptureCounters {
        raw_frames: u64,
        accepted_frames: u64,
        skipped_frames: u64,
        dropped: u64,
    }

    #[derive(Debug)]
    struct CaptureState {
        counters: CaptureCounters,
        pacing_started_at: Instant,
        target_fps: u32,
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct PipelineTimings {
        converted_frames: u64,
        copy_ms_total: f64,
        convert_ms_total: f64,
        encode_ms_total: f64,
    }

    #[derive(Clone, Copy, Debug)]
    pub struct CapturePipelineStats {
        pub raw_frames: u64,
        pub accepted_frames: u64,
        pub skipped_frames: u64,
        pub converted_frames: u64,
        pub frames_in: u64,
        pub samples_out: u64,
        pub bytes_out: u64,
        pub raw_fps: f64,
        pub accepted_fps: f64,
        pub encode_fps: f64,
        pub target_fps: u32,
        pub mbps: f64,
        pub width: u32,
        pub height: u32,
        pub copy_ms_avg: f64,
        pub convert_ms_avg: f64,
        pub encode_ms_avg: f64,
        pub dropped: u64,
    }

    #[derive(Clone, Copy, Debug)]
    pub struct CapturePipelineDone {
        pub raw_frames: u64,
        pub accepted_frames: u64,
        pub skipped_frames: u64,
        pub converted_frames: u64,
        pub encoder: EncoderStats,
        pub media_duration_sec: f64,
        pub wall_time_sec: f64,
        pub processing_fps: f64,
        pub mbps: f64,
        pub width: u32,
        pub height: u32,
        pub copy_ms_avg: f64,
        pub convert_ms_avg: f64,
        pub encode_ms_avg: f64,
        pub dropped: u64,
        pub stopped_by_console: bool,
    }

    pub trait CaptureEncodeObserver {
        fn on_sample(&mut self, sample: EncodedSample) -> Result<(), String>;
        fn on_stats(&mut self, stats: &CapturePipelineStats) -> Result<(), String>;
    }

    struct FileProbeObserver {
        output: File,
    }

    impl CaptureEncodeObserver for FileProbeObserver {
        fn on_sample(&mut self, sample: EncodedSample) -> Result<(), String> {
            self.output
                .write_all(&sample.bytes)
                .map_err(|err| format!("write H.264 output failed: {err}"))
        }

        fn on_stats(&mut self, stats: &CapturePipelineStats) -> Result<(), String> {
            println!(
                r#"{{"type":"CAPTURE_ENCODE_STATS","mode":"capture_encode_probe","raw_frames":{},"accepted_frames":{},"skipped_frames":{},"converted_frames":{},"frames_in":{},"samples_out":{},"bytes_out":{},"raw_fps":{:.2},"accepted_fps":{:.2},"encode_fps":{:.2},"target_fps":{},"mbps":{:.3},"width":{},"height":{},"format_in":"{}","format_encode":"NV12","copy_ms_avg":{:.3},"convert_ms_avg":{:.3},"encode_ms_avg":{:.3},"dropped":{}}}"#,
                stats.raw_frames,
                stats.accepted_frames,
                stats.skipped_frames,
                stats.converted_frames,
                stats.frames_in,
                stats.samples_out,
                stats.bytes_out,
                stats.raw_fps,
                stats.accepted_fps,
                stats.encode_fps,
                stats.target_fps,
                stats.mbps,
                stats.width,
                stats.height,
                PIXEL_FORMAT_NAME,
                stats.copy_ms_avg,
                stats.convert_ms_avg,
                stats.encode_ms_avg,
                stats.dropped
            );
            io::stdout().flush().ok();
            Ok(())
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

    struct D3dDevices {
        native: ID3D11Device,
        context: ID3D11DeviceContext,
        winrt: IDirect3DDevice,
        driver_name: &'static str,
    }

    struct ReadbackState {
        staging: Option<ID3D11Texture2D>,
        desc: Option<D3D11_TEXTURE2D_DESC>,
        nv12: Vec<u8>,
    }

    pub fn run(config: CaptureEncodeConfig) -> Result<(), String> {
        validate_config(&config)?;
        let output = File::create(Path::new(&config.output))
            .map_err(|err| format!("create output failed: {err}"))?;
        let mut observer = FileProbeObserver { output };
        let done = run_with_observer(&config, &mut observer)?;
        observer
            .output
            .flush()
            .map_err(|err| format!("flush output failed: {err}"))?;
        println!(
            r#"{{"type":"CAPTURE_ENCODE_DONE","encoder":"{}","raw_frames":{},"accepted_frames":{},"skipped_frames":{},"converted_frames":{},"frames_in":{},"samples_out":{},"bytes_out":{},"duration_sec":{:.3},"wall_time_sec":{:.3},"fps":{},"processing_fps":{:.2},"mbps":{:.3},"keyframes":{},"width":{},"height":{},"copy_ms_avg":{:.3},"convert_ms_avg":{:.3},"encode_ms_avg":{:.3},"dropped":{},"output":"{}"}}"#,
            ENCODER_NAME,
            done.raw_frames,
            done.accepted_frames,
            done.skipped_frames,
            done.converted_frames,
            done.encoder.frames_in,
            done.encoder.samples_out,
            done.encoder.bytes_out,
            done.media_duration_sec,
            done.wall_time_sec,
            config.target_fps,
            done.processing_fps,
            done.mbps,
            keyframes_json(done.encoder),
            done.width,
            done.height,
            done.copy_ms_avg,
            done.convert_ms_avg,
            done.encode_ms_avg,
            done.dropped,
            json_escape(&config.output)
        );
        io::stdout().flush().ok();
        eprintln!(
            "capture-encode-probe stopped reason={}",
            if done.stopped_by_console {
                "console-control"
            } else {
                "duration-complete"
            }
        );
        Ok(())
    }

    pub fn run_with_observer(
        config: &CaptureEncodeConfig,
        observer: &mut dyn CaptureEncodeObserver,
    ) -> Result<CapturePipelineDone, String> {
        validate_stream_config(config)?;
        let _winrt = WinRtGuard::initialize()?;
        let _console_ctrl = ConsoleCtrlGuard::install()?;
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
        let width = size.Width as u32;
        let height = size.Height as u32;
        if width % 2 != 0 || height % 2 != 0 {
            return Err(format!(
                "capture dimensions must be even for NV12: {width}x{height}"
            ));
        }

        let mut encoder =
            WmfH264Encoder::new_stream(width, height, config.target_fps, config.bitrate_mbps)?;
        let mut readback = ReadbackState {
            staging: None,
            desc: None,
            nv12: vec![0u8; bgra_to_nv12::buffer_size(width, height)?],
        };
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

        let capture_state = Arc::new(Mutex::new(CaptureState {
            counters: CaptureCounters::default(),
            pacing_started_at: Instant::now(),
            target_fps: config.target_fps,
        }));
        let (frame_tx, frame_rx) = mpsc::sync_channel(FRAME_QUEUE_CAPACITY);
        let callback_state = Arc::clone(&capture_state);
        let frame_handler = create_frame_handler(callback_state, frame_tx);
        let frame_token = frame_pool
            .FrameArrived(&frame_handler)
            .map_err(|err| format!("FrameArrived registration failed: {err}"))?;

        eprintln!(
            "capture-encode-probe target=primary-monitor size={}x{} input={} encode=NV12 encoder=\"{}\" target_fps={} bitrate_mbps={} d3d_driver={} output_buffer={} profile_main={}",
            width,
            height,
            PIXEL_FORMAT_NAME,
            ENCODER_NAME,
            config.target_fps,
            config.bitrate_mbps,
            devices.driver_name,
            encoder.output_buffer_size(),
            encoder.profile_main()
        );
        session
            .StartCapture()
            .map_err(|err| format!("StartCapture failed: {err}"))?;

        let started_at = Instant::now();
        let mut report_at = started_at;
        let mut previous_capture = CaptureCounters::default();
        let mut previous_encoder = EncoderStats::default();
        let mut timings = PipelineTimings::default();

        while started_at.elapsed() < Duration::from_secs(config.duration_sec)
            && !STOP_REQUESTED.load(Ordering::SeqCst)
        {
            match frame_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(frame) => process_received_frame(
                    frame,
                    &devices,
                    &mut readback,
                    &mut encoder,
                    &mut timings,
                    &capture_state,
                ),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("WGC frame channel disconnected".to_string())
                }
            }
            emit_encoded_samples(&mut encoder, observer)?;

            let now = Instant::now();
            if now.duration_since(report_at) >= Duration::from_secs(1) {
                let capture = capture_snapshot(&capture_state);
                let encoder_stats = encoder.stats();
                let stats = make_stats(
                    width,
                    height,
                    config.target_fps,
                    capture,
                    previous_capture,
                    encoder_stats,
                    previous_encoder,
                    timings,
                    now.duration_since(report_at),
                );
                observer.on_stats(&stats)?;
                previous_capture = capture;
                previous_encoder = encoder_stats;
                report_at = now;
            }
        }

        let _ = frame_pool.RemoveFrameArrived(frame_token);
        let _ = session.Close();
        let _ = frame_pool.Close();
        drain_frame_queue(
            &frame_rx,
            &devices,
            &mut readback,
            &mut encoder,
            &mut timings,
            &capture_state,
            observer,
        )?;
        emit_encoded_samples(&mut encoder, observer)?;

        let encoder_stats = encoder.finish()?;
        emit_encoded_samples(&mut encoder, observer)?;
        let capture = capture_snapshot(&capture_state);
        let media_duration_sec = encoder_stats.frames_in as f64 / f64::from(config.target_fps);
        let mbps =
            encoder_stats.bytes_out as f64 * 8.0 / media_duration_sec.max(0.001) / 1_000_000.0;
        let wall_time_sec = started_at.elapsed().as_secs_f64();
        Ok(CapturePipelineDone {
            raw_frames: capture.raw_frames,
            accepted_frames: capture.accepted_frames,
            skipped_frames: capture.skipped_frames,
            converted_frames: timings.converted_frames,
            encoder: encoder_stats,
            media_duration_sec,
            wall_time_sec,
            processing_fps: encoder_stats.frames_in as f64 / wall_time_sec.max(0.001),
            mbps,
            width,
            height,
            copy_ms_avg: average_ms(timings.copy_ms_total, timings.converted_frames),
            convert_ms_avg: average_ms(timings.convert_ms_total, timings.converted_frames),
            encode_ms_avg: average_ms(timings.encode_ms_total, encoder_stats.frames_in),
            dropped: capture.dropped,
            stopped_by_console: STOP_REQUESTED.load(Ordering::SeqCst),
        })
    }

    fn create_frame_handler(
        state: Arc<Mutex<CaptureState>>,
        sender: SyncSender<Direct3D11CaptureFrame>,
    ) -> TypedEventHandler<Direct3D11CaptureFramePool, IInspectable> {
        TypedEventHandler::new(
            move |pool: Ref<Direct3D11CaptureFramePool>, _args: Ref<IInspectable>| {
                let Some(pool) = pool.as_ref() else {
                    increment_dropped(&state);
                    return Ok(());
                };
                match pool.TryGetNextFrame() {
                    Ok(frame) => {
                        let should_submit = if let Ok(mut locked) = state.lock() {
                            locked.counters.raw_frames += 1;
                            let budget = (locked.pacing_started_at.elapsed().as_secs_f64()
                                * f64::from(locked.target_fps))
                            .floor() as u64;
                            locked.counters.accepted_frames < budget
                        } else {
                            false
                        };
                        if should_submit {
                            match sender.try_send(frame) {
                                Ok(()) => {
                                    if let Ok(mut locked) = state.lock() {
                                        locked.counters.accepted_frames += 1;
                                    }
                                }
                                Err(TrySendError::Full(frame))
                                | Err(TrySendError::Disconnected(frame)) => {
                                    let _ = frame.Close();
                                    if let Ok(mut locked) = state.lock() {
                                        locked.counters.skipped_frames += 1;
                                    }
                                }
                            }
                        } else {
                            let _ = frame.Close();
                            if let Ok(mut locked) = state.lock() {
                                locked.counters.skipped_frames += 1;
                            }
                        }
                    }
                    Err(_) => increment_dropped(&state),
                }
                Ok(())
            },
        )
    }

    fn process_received_frame(
        frame: Direct3D11CaptureFrame,
        devices: &D3dDevices,
        readback: &mut ReadbackState,
        encoder: &mut WmfH264Encoder,
        timings: &mut PipelineTimings,
        capture_state: &Arc<Mutex<CaptureState>>,
    ) {
        let result = readback_frame(&frame, devices, readback);
        let _ = frame.Close();
        match result {
            Ok((bgra, row_pitch, width, height, copy_elapsed)) => {
                let convert_started = Instant::now();
                if let Err(err) =
                    bgra_to_nv12::convert(&bgra, row_pitch, width, height, &mut readback.nv12)
                {
                    increment_dropped(capture_state);
                    eprintln!("capture frame conversion failed: {err}");
                    return;
                }
                let convert_elapsed = convert_started.elapsed();
                let encode_started = Instant::now();
                let frame_index = encoder.stats().frames_in;
                if let Err(err) = encoder.encode_nv12(&readback.nv12, frame_index) {
                    increment_dropped(capture_state);
                    eprintln!("capture frame encode failed: {err}");
                    return;
                }
                timings.copy_ms_total += copy_elapsed.as_secs_f64() * 1000.0;
                timings.convert_ms_total += convert_elapsed.as_secs_f64() * 1000.0;
                timings.encode_ms_total += encode_started.elapsed().as_secs_f64() * 1000.0;
                timings.converted_frames += 1;
            }
            Err(err) => {
                increment_dropped(capture_state);
                eprintln!("capture frame readback failed: {err}");
            }
        }
    }

    fn readback_frame(
        frame: &Direct3D11CaptureFrame,
        devices: &D3dDevices,
        readback: &mut ReadbackState,
    ) -> Result<(Vec<u8>, usize, u32, u32, Duration), String> {
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
        if desc.Width % 2 != 0 || desc.Height % 2 != 0 {
            return Err(format!(
                "captured texture dimensions are not even: {}x{}",
                desc.Width, desc.Height
            ));
        }
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

        let copy_started = Instant::now();
        unsafe { devices.context.CopyResource(staging, &texture) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            devices
                .context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }
        .map_err(|err| format!("staging texture Map failed: {err}"))?;

        let row_pitch = mapped.RowPitch as usize;
        let copy_result = row_pitch
            .checked_mul(desc.Height as usize)
            .ok_or_else(|| "mapped texture length overflow".to_string())
            .and_then(|mapped_len| {
                if mapped.pData.is_null() {
                    Err("mapped texture returned null data".to_string())
                } else {
                    Ok(
                        unsafe { slice::from_raw_parts(mapped.pData.cast::<u8>(), mapped_len) }
                            .to_vec(),
                    )
                }
            });
        unsafe { devices.context.Unmap(staging, 0) };
        let copy_elapsed = copy_started.elapsed();
        Ok((
            copy_result?,
            row_pitch,
            desc.Width,
            desc.Height,
            copy_elapsed,
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
        readback.nv12 =
            vec![0u8; bgra_to_nv12::buffer_size(source_desc.Width, source_desc.Height)?];
        Ok(())
    }

    fn drain_frame_queue(
        receiver: &Receiver<Direct3D11CaptureFrame>,
        devices: &D3dDevices,
        readback: &mut ReadbackState,
        encoder: &mut WmfH264Encoder,
        timings: &mut PipelineTimings,
        capture_state: &Arc<Mutex<CaptureState>>,
        observer: &mut dyn CaptureEncodeObserver,
    ) -> Result<(), String> {
        while let Ok(frame) = receiver.try_recv() {
            process_received_frame(frame, devices, readback, encoder, timings, capture_state);
            emit_encoded_samples(encoder, observer)?;
        }
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

    fn validate_config(config: &CaptureEncodeConfig) -> Result<(), String> {
        validate_stream_config(config)?;
        if config.output.trim().is_empty() {
            return Err("output path must not be empty".to_string());
        }
        Ok(())
    }

    fn validate_stream_config(config: &CaptureEncodeConfig) -> Result<(), String> {
        if config.duration_sec == 0 || config.target_fps == 0 {
            return Err("duration-sec and target-fps must be greater than zero".to_string());
        }
        if !config.bitrate_mbps.is_finite() || config.bitrate_mbps <= 0.0 {
            return Err("bitrate-mbps must be greater than zero".to_string());
        }
        Ok(())
    }

    fn increment_dropped(state: &Arc<Mutex<CaptureState>>) {
        if let Ok(mut locked) = state.lock() {
            locked.counters.dropped += 1;
        }
    }

    fn capture_snapshot(state: &Arc<Mutex<CaptureState>>) -> CaptureCounters {
        state
            .lock()
            .map(|locked| locked.counters)
            .unwrap_or_default()
    }

    fn emit_encoded_samples(
        encoder: &mut WmfH264Encoder,
        observer: &mut dyn CaptureEncodeObserver,
    ) -> Result<(), String> {
        for sample in encoder.take_encoded_samples() {
            observer.on_sample(sample)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn make_stats(
        width: u32,
        height: u32,
        target_fps: u32,
        capture: CaptureCounters,
        previous_capture: CaptureCounters,
        encoder: EncoderStats,
        previous_encoder: EncoderStats,
        timings: PipelineTimings,
        elapsed: Duration,
    ) -> CapturePipelineStats {
        let elapsed_sec = elapsed.as_secs_f64().max(0.001);
        let raw_fps = capture
            .raw_frames
            .saturating_sub(previous_capture.raw_frames) as f64
            / elapsed_sec;
        let accepted_fps = capture
            .accepted_frames
            .saturating_sub(previous_capture.accepted_frames) as f64
            / elapsed_sec;
        let encode_fps =
            encoder.frames_in.saturating_sub(previous_encoder.frames_in) as f64 / elapsed_sec;
        let mbps = encoder.bytes_out.saturating_sub(previous_encoder.bytes_out) as f64 * 8.0
            / elapsed_sec
            / 1_000_000.0;
        CapturePipelineStats {
            raw_frames: capture.raw_frames,
            accepted_frames: capture.accepted_frames,
            skipped_frames: capture.skipped_frames,
            converted_frames: timings.converted_frames,
            frames_in: encoder.frames_in,
            samples_out: encoder.samples_out,
            bytes_out: encoder.bytes_out,
            raw_fps,
            accepted_fps,
            encode_fps,
            target_fps,
            mbps,
            width,
            height,
            copy_ms_avg: average_ms(timings.copy_ms_total, timings.converted_frames),
            convert_ms_avg: average_ms(timings.convert_ms_total, timings.converted_frames),
            encode_ms_avg: average_ms(timings.encode_ms_total, encoder.frames_in),
            dropped: capture.dropped,
        }
    }

    fn average_ms(total: f64, count: u64) -> f64 {
        if count == 0 {
            0.0
        } else {
            total / count as f64
        }
    }

    fn keyframes_json(stats: EncoderStats) -> String {
        if stats.keyframe_detection_available {
            stats.keyframes.to_string()
        } else {
            "null".to_string()
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
pub use platform::{
    run, run_with_observer, CaptureEncodeObserver, CapturePipelineDone, CapturePipelineStats,
};

#[cfg(not(windows))]
pub fn run(_config: CaptureEncodeConfig) -> Result<(), String> {
    Err("capture-encode-probe is only supported on Windows".to_string())
}
