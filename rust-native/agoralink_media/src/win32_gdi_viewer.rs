#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderScaleMode {
    Exact,
    Fit,
    Stretch,
}

impl RenderScaleMode {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Fit => "fit",
            Self::Stretch => "stretch",
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        match text.to_ascii_lowercase().as_str() {
            "exact" => Ok(Self::Exact),
            "fit" => Ok(Self::Fit),
            "stretch" => Ok(Self::Stretch),
            _ => Err("render-scale must be exact, fit, or stretch".to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowMode {
    Windowed,
    BorderlessFullscreen,
}

impl WindowMode {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Windowed => "windowed",
            Self::BorderlessFullscreen => "borderless-fullscreen",
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        match text.to_ascii_lowercase().as_str() {
            "windowed" => Ok(Self::Windowed),
            "borderless-fullscreen" => Ok(Self::BorderlessFullscreen),
            _ => Err("window-mode must be windowed or borderless-fullscreen".to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DpiAwarenessInfo {
    pub set: bool,
    pub mode: &'static str,
}

impl Default for DpiAwarenessInfo {
    fn default() -> Self {
        Self {
            set: false,
            mode: "unavailable",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct GdiRenderStats {
    pub scale_mode: RenderScaleMode,
    pub window_mode: WindowMode,
    pub initialized: bool,
    pub init_waiting: bool,
    pub init_frame_id: Option<u64>,
    pub resize_count: u64,
    pub video_width: u32,
    pub video_height: u32,
    pub client_width: u32,
    pub client_height: u32,
    pub draw_width: u32,
    pub draw_height: u32,
    pub scaled: bool,
    pub last_error: Option<String>,
    pub exact_resize_attempts: u64,
    pub exact_resize_wait_ms: f64,
    pub exact_resize_transient_1x1_count: u64,
    pub exact_resize_final_client_width: u32,
    pub exact_resize_final_client_height: u32,
    pub exact_resize_failure_reason: Option<String>,
    pub dpi: DpiAwarenessInfo,
    pub display: crate::display_capability::DisplayCapability,
}

impl Default for RenderScaleMode {
    fn default() -> Self {
        Self::Exact
    }
}

impl Default for WindowMode {
    fn default() -> Self {
        Self::Windowed
    }
}

impl GdiRenderStats {
    pub fn json_fragment_without_backend(&self) -> String {
        let init_frame_id = self
            .init_frame_id
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string());
        let last_error = self
            .last_error
            .as_deref()
            .map(|value| format!(r#""{}""#, json_escape(value)))
            .unwrap_or_else(|| "null".to_string());
        let exact_resize_failure_reason = self
            .exact_resize_failure_reason
            .as_deref()
            .map(|value| format!(r#""{}""#, json_escape(value)))
            .unwrap_or_else(|| "null".to_string());
        let base = format!(
            r#""render_scale_mode":"{}","window_mode":"{}","render_initialized":{},"render_init_waiting":{},"render_init_frame_id":{},"render_resize_count":{},"render_video_width":{},"render_video_height":{},"render_client_width":{},"render_client_height":{},"render_draw_width":{},"render_draw_height":{},"render_scaled":{},"render_last_error":{},"exact_resize_attempts":{},"exact_resize_wait_ms":{:.3},"exact_resize_transient_1x1_count":{},"exact_resize_final_client_width":{},"exact_resize_final_client_height":{},"exact_resize_failure_reason":{},"dpi_awareness_set":{},"dpi_awareness_mode":"{}""#,
            self.scale_mode.name(),
            self.window_mode.name(),
            self.initialized,
            self.init_waiting,
            init_frame_id,
            self.resize_count,
            self.video_width,
            self.video_height,
            self.client_width,
            self.client_height,
            self.draw_width,
            self.draw_height,
            self.scaled,
            last_error,
            self.exact_resize_attempts,
            self.exact_resize_wait_ms,
            self.exact_resize_transient_1x1_count,
            self.exact_resize_final_client_width,
            self.exact_resize_final_client_height,
            exact_resize_failure_reason,
            self.dpi.set,
            self.dpi.mode,
        );
        format!("{base},{}", self.display.json_fragment("render"))
    }
}

fn json_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DrawRect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

fn calculate_draw_rect(
    mode: RenderScaleMode,
    window_mode: WindowMode,
    video_width: u32,
    video_height: u32,
    client_width: u32,
    client_height: u32,
) -> DrawRect {
    match mode {
        RenderScaleMode::Exact => DrawRect {
            x: if window_mode == WindowMode::BorderlessFullscreen {
                (i64::from(client_width) - i64::from(video_width)) as i32 / 2
            } else {
                0
            },
            y: if window_mode == WindowMode::BorderlessFullscreen {
                (i64::from(client_height) - i64::from(video_height)) as i32 / 2
            } else {
                0
            },
            width: video_width,
            height: video_height,
        },
        RenderScaleMode::Stretch => DrawRect {
            x: 0,
            y: 0,
            width: client_width,
            height: client_height,
        },
        RenderScaleMode::Fit => {
            let video_aspect = u64::from(video_width) * u64::from(client_height);
            let client_aspect = u64::from(client_width) * u64::from(video_height);
            let (width, height) = if video_aspect > client_aspect {
                let height = (u64::from(client_width) * u64::from(video_height)
                    / u64::from(video_width)) as u32;
                (client_width, height.max(1))
            } else {
                let width = (u64::from(client_height) * u64::from(video_width)
                    / u64::from(video_height)) as u32;
                (width.max(1), client_height)
            };
            DrawRect {
                x: ((client_width - width) / 2) as i32,
                y: ((client_height - height) / 2) as i32,
                width,
                height,
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::mem::{size_of, transmute};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::OnceLock;
    use std::thread;
    use std::time::{Duration, Instant};

    use windows::core::{s, w, BOOL, PCWSTR};
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        GetDC, GetMonitorInfoW, MonitorFromWindow, PatBlt, ReleaseDC, SetDIBitsToDevice,
        StretchDIBits, UpdateWindow, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLACKNESS,
        DIB_RGB_COLORS, MONITORINFO, MONITOR_DEFAULTTONEAREST, SRCCOPY,
    };
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
    use windows::Win32::UI::WindowsAndMessaging::{
        AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        GetClientRect, GetWindowRect, IsIconic, IsWindowVisible, LoadCursorW, PeekMessageW,
        PostQuitMessage, RegisterClassW, SetProcessDPIAware, SetWindowPos, SetWindowTextW,
        ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, IDC_ARROW, MINMAXINFO,
        MSG, PM_REMOVE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_SHOW, WINDOW_EX_STYLE,
        WINDOW_STYLE, WM_CLOSE, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_GETMINMAXINFO,
        WM_KEYDOWN, WM_MOVE, WM_QUIT, WM_WINDOWPOSCHANGED, WNDCLASSW, WS_OVERLAPPEDWINDOW,
        WS_POPUP, WS_VISIBLE,
    };

    use super::{
        calculate_draw_rect, DpiAwarenessInfo, GdiRenderStats, RenderScaleMode, WindowMode,
    };

    const WINDOW_CLASS: PCWSTR = w!("AgoraLinkNativeGdiViewer");
    const WINDOW_EX_STYLE_VALUE: WINDOW_EX_STYLE = WINDOW_EX_STYLE(0);
    const PER_MONITOR_AWARE_V2: isize = -4;
    const DISPLAY_REFRESH_POLL: Duration = Duration::from_millis(500);
    const EXACT_RESIZE_TIMEOUT: Duration = Duration::from_millis(1_000);
    const EXACT_RESIZE_RETRY: Duration = Duration::from_millis(10);

    static DPI_AWARENESS: OnceLock<DpiAwarenessInfo> = OnceLock::new();
    static DISPLAY_CHANGE_EPOCH: AtomicU64 = AtomicU64::new(1);

    pub struct GdiViewerWindow {
        hwnd: HWND,
        closed: bool,
        base_title: String,
        scale_mode: RenderScaleMode,
        window_mode: WindowMode,
        window_style: WINDOW_STYLE,
        stats: GdiRenderStats,
        title_state: Option<(u32, u32, u32, u32, bool)>,
        display_refresh_detect: crate::display_capability::DisplayRefreshDetect,
        display_check_at: Instant,
        display_epoch_seen: u64,
    }

    impl GdiViewerWindow {
        pub fn create_with_display_detection(
            title: &str,
            scale_mode: RenderScaleMode,
            window_mode: WindowMode,
            display_refresh_detect: crate::display_capability::DisplayRefreshDetect,
        ) -> Result<Self, String> {
            let dpi = *DPI_AWARENESS.get_or_init(set_process_dpi_awareness);
            let module = unsafe { GetModuleHandleW(None) }
                .map_err(|err| format!("GetModuleHandleW failed: {err}"))?;
            let instance: HINSTANCE = module.into();
            let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }
                .map_err(|err| format!("LoadCursorW failed: {err}"))?;
            let class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                hCursor: cursor,
                lpszClassName: WINDOW_CLASS,
                ..Default::default()
            };
            let atom = unsafe { RegisterClassW(&class) };
            if atom == 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(1410) {
                    return Err(format!("RegisterClassW failed: {error}"));
                }
            }

            let title_wide: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
            let window_style = match window_mode {
                WindowMode::Windowed => WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 | WS_VISIBLE.0),
                WindowMode::BorderlessFullscreen => WINDOW_STYLE(WS_POPUP.0 | WS_VISIBLE.0),
            };
            let hwnd = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE_VALUE,
                    WINDOW_CLASS,
                    PCWSTR(title_wide.as_ptr()),
                    window_style,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    960,
                    600,
                    None,
                    None,
                    Some(instance),
                    None,
                )
            }
            .map_err(|err| format!("CreateWindowExW failed: {err}"))?;
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = UpdateWindow(hwnd);
            }
            let mut viewer = Self {
                hwnd,
                closed: false,
                base_title: title.to_string(),
                scale_mode,
                window_mode,
                window_style,
                stats: GdiRenderStats {
                    scale_mode,
                    window_mode,
                    init_waiting: true,
                    dpi,
                    ..GdiRenderStats::default()
                },
                title_state: None,
                display_refresh_detect,
                display_check_at: Instant::now(),
                display_epoch_seen: DISPLAY_CHANGE_EPOCH.load(Ordering::Acquire),
            };
            if window_mode == WindowMode::BorderlessFullscreen {
                viewer.size_to_current_monitor()?;
            }
            if !viewer.pump_messages() {
                return Err("viewer window closed during initial message processing".to_string());
            }
            viewer.refresh_display_capability(true);
            Ok(viewer)
        }

        pub fn pump_messages(&mut self) -> bool {
            if self.closed {
                return false;
            }
            let mut message = MSG::default();
            while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                if message.message == WM_QUIT {
                    self.closed = true;
                    return false;
                }
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            self.refresh_display_capability(false);
            true
        }

        pub fn render_stats(&self) -> GdiRenderStats {
            self.stats.clone()
        }

        pub fn hwnd(&self) -> HWND {
            self.hwnd
        }

        pub fn prepare_video(
            &mut self,
            width: u32,
            height: u32,
            frame_id: Option<u64>,
        ) -> Result<(), String> {
            let result = self.prepare_video_inner(width, height, frame_id);
            match &result {
                Ok(()) => self.stats.last_error = None,
                Err(err) => self.stats.last_error = Some(err.clone()),
            }
            result
        }

        pub fn render_bgra_with_stride(
            &mut self,
            bgra: &[u8],
            width: u32,
            height: u32,
            stride: usize,
        ) -> Result<(), String> {
            if !self.pump_messages() {
                return Err("viewer window was closed".to_string());
            }
            validate_bgra(bgra, width, height, stride)?;
            self.prepare_video(width, height, None)?;
            let draw = calculate_draw_rect(
                self.scale_mode,
                self.window_mode,
                width,
                height,
                self.stats.client_width,
                self.stats.client_height,
            );

            let expected = stride * height as usize;
            let mut bitmap = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    biSizeImage: expected as u32,
                    ..Default::default()
                },
                ..Default::default()
            };
            let hdc = unsafe { GetDC(Some(self.hwnd)) };
            if hdc.is_invalid() {
                return Err("GetDC returned an invalid device context".to_string());
            }
            if draw.x != 0
                || draw.y != 0
                || draw.width != self.stats.client_width
                || draw.height != self.stats.client_height
            {
                unsafe {
                    let _ = PatBlt(
                        hdc,
                        0,
                        0,
                        self.stats.client_width as i32,
                        self.stats.client_height as i32,
                        BLACKNESS,
                    );
                }
            }
            let rendered = if self.stats.scaled {
                unsafe {
                    StretchDIBits(
                        hdc,
                        draw.x,
                        draw.y,
                        draw.width as i32,
                        draw.height as i32,
                        0,
                        0,
                        width as i32,
                        height as i32,
                        Some(bgra.as_ptr().cast()),
                        &raw mut bitmap,
                        DIB_RGB_COLORS,
                        SRCCOPY,
                    )
                }
            } else {
                unsafe {
                    SetDIBitsToDevice(
                        hdc,
                        draw.x,
                        draw.y,
                        width,
                        height,
                        0,
                        0,
                        0,
                        height,
                        bgra.as_ptr().cast(),
                        &raw mut bitmap,
                        DIB_RGB_COLORS,
                    )
                }
            };
            unsafe {
                ReleaseDC(Some(self.hwnd), hdc);
            }
            if rendered == 0 {
                let err = format!("GDI frame draw failed: {}", std::io::Error::last_os_error());
                self.stats.last_error = Some(err.clone());
                return Err(err);
            }
            self.stats.last_error = None;
            Ok(())
        }

        fn prepare_video_inner(
            &mut self,
            width: u32,
            height: u32,
            frame_id: Option<u64>,
        ) -> Result<(), String> {
            if width == 0 || height == 0 {
                self.stats.init_waiting = true;
                return Err("renderer is waiting for a non-zero video size".to_string());
            }
            let dimensions_changed = self.stats.video_width != width
                || self.stats.video_height != height
                || !self.stats.initialized;
            if self.window_mode == WindowMode::Windowed && self.scale_mode == RenderScaleMode::Exact
            {
                self.ensure_exact_client_size(width, height)?;
            } else if self.window_mode == WindowMode::BorderlessFullscreen
                && !self.stats.initialized
            {
                self.size_to_current_monitor()?;
            }
            let (client_width, client_height) = self.client_size()?;
            let draw = calculate_draw_rect(
                self.scale_mode,
                self.window_mode,
                width,
                height,
                client_width,
                client_height,
            );
            let layout_changed = dimensions_changed
                || self.stats.client_width != client_width
                || self.stats.client_height != client_height
                || self.stats.draw_width != draw.width
                || self.stats.draw_height != draw.height;
            if layout_changed {
                self.stats.resize_count = self.stats.resize_count.saturating_add(1);
            }
            self.stats.initialized = true;
            self.stats.init_waiting = false;
            if self.stats.init_frame_id.is_none() {
                self.stats.init_frame_id = frame_id;
            }
            self.stats.video_width = width;
            self.stats.video_height = height;
            self.stats.client_width = client_width;
            self.stats.client_height = client_height;
            self.stats.draw_width = draw.width;
            self.stats.draw_height = draw.height;
            self.stats.scaled = draw.width != width || draw.height != height;
            self.update_title();
            Ok(())
        }

        fn client_size(&self) -> Result<(u32, u32), String> {
            let mut client = RECT::default();
            unsafe { GetClientRect(self.hwnd, &mut client) }
                .map_err(|err| format!("GetClientRect failed: {err}"))?;
            Ok((
                (client.right - client.left).max(1) as u32,
                (client.bottom - client.top).max(1) as u32,
            ))
        }

        fn ensure_exact_client_size(&mut self, width: u32, height: u32) -> Result<(), String> {
            let initial = self.client_size()?;
            if initial == (width, height) {
                self.stats.exact_resize_final_client_width = initial.0;
                self.stats.exact_resize_final_client_height = initial.1;
                self.stats.exact_resize_failure_reason = None;
                return Ok(());
            }
            let started = Instant::now();
            let deadline = started + EXACT_RESIZE_TIMEOUT;
            let mut outer = RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
            let dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96);
            unsafe {
                AdjustWindowRectExForDpi(
                    &mut outer,
                    self.window_style,
                    false,
                    WINDOW_EX_STYLE_VALUE,
                    dpi,
                )
            }
            .or_else(|_| unsafe {
                AdjustWindowRectEx(&mut outer, self.window_style, false, WINDOW_EX_STYLE_VALUE)
            })
            .map_err(|err| format!("DPI-aware window rect adjustment failed: {err}"))?;
            let mut outer_width = outer.right - outer.left;
            let mut outer_height = outer.bottom - outer.top;

            loop {
                self.stats.exact_resize_attempts =
                    self.stats.exact_resize_attempts.saturating_add(1);
                unsafe {
                    SetWindowPos(
                        self.hwnd,
                        None,
                        0,
                        0,
                        outer_width,
                        outer_height,
                        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                    )
                }
                .map_err(|err| format!("SetWindowPos exact-size failed: {err}"))?;
                unsafe {
                    let _ = UpdateWindow(self.hwnd);
                }
                if !self.pump_messages() {
                    let reason =
                        "viewer window closed while applying exact client size".to_string();
                    self.stats.exact_resize_failure_reason = Some(reason.clone());
                    return Err(reason);
                }

                let actual = self.client_size()?;
                self.stats.exact_resize_final_client_width = actual.0;
                self.stats.exact_resize_final_client_height = actual.1;
                if actual == (width, height) {
                    self.stats.exact_resize_wait_ms = started.elapsed().as_secs_f64() * 1_000.0;
                    self.stats.exact_resize_failure_reason = None;
                    return Ok(());
                }
                if actual == (1, 1) {
                    self.stats.exact_resize_transient_1x1_count = self
                        .stats
                        .exact_resize_transient_1x1_count
                        .saturating_add(1);
                } else {
                    outer_width = (outer_width + width as i32 - actual.0 as i32).max(1);
                    outer_height = (outer_height + height as i32 - actual.1 as i32).max(1);
                }

                if Instant::now() >= deadline {
                    self.stats.exact_resize_wait_ms = started.elapsed().as_secs_f64() * 1_000.0;
                    let reason = format!(
                        "exact render mode could not set client size before deadline: requested={}x{}, actual={}x{}, attempts={}, transient_1x1={}, {}",
                        width,
                        height,
                        actual.0,
                        actual.1,
                        self.stats.exact_resize_attempts,
                        self.stats.exact_resize_transient_1x1_count,
                        self.exact_window_diagnostics(dpi),
                    );
                    self.stats.exact_resize_failure_reason = Some(reason.clone());
                    return Err(reason);
                }
                thread::sleep(EXACT_RESIZE_RETRY);
            }
        }

        fn exact_window_diagnostics(&self, dpi: u32) -> String {
            let mut window = RECT::default();
            let mut client = RECT::default();
            let window_result = unsafe { GetWindowRect(self.hwnd, &mut window) };
            let client_result = unsafe { GetClientRect(self.hwnd, &mut client) };
            let window_text = if window_result.is_ok() {
                format!(
                    "window_rect=({},{}..{},{};{}x{})",
                    window.left,
                    window.top,
                    window.right,
                    window.bottom,
                    (window.right - window.left).max(0),
                    (window.bottom - window.top).max(0),
                )
            } else {
                "window_rect=unavailable".to_string()
            };
            let client_text = if client_result.is_ok() {
                format!(
                    "client_rect=({},{}..{},{};{}x{})",
                    client.left,
                    client.top,
                    client.right,
                    client.bottom,
                    (client.right - client.left).max(0),
                    (client.bottom - client.top).max(0),
                )
            } else {
                "client_rect=unavailable".to_string()
            };
            format!(
                "dpi={}, {}, {}, visible={}, minimized={}, window_mode={}",
                dpi,
                window_text,
                client_text,
                unsafe { IsWindowVisible(self.hwnd) }.as_bool(),
                unsafe { IsIconic(self.hwnd) }.as_bool(),
                self.window_mode.name(),
            )
        }

        fn size_to_current_monitor(&self) -> Result<(), String> {
            let monitor = unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST) };
            if monitor.is_invalid() {
                return Err("MonitorFromWindow returned an invalid monitor".to_string());
            }
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
                return Err(format!(
                    "GetMonitorInfoW failed: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let monitor_rect = info.rcMonitor;
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    None,
                    monitor_rect.left,
                    monitor_rect.top,
                    monitor_rect.right - monitor_rect.left,
                    monitor_rect.bottom - monitor_rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
            }
            .map_err(|err| format!("SetWindowPos fullscreen failed: {err}"))
        }

        fn update_title(&mut self) {
            let state = (
                self.stats.video_width,
                self.stats.video_height,
                self.stats.draw_width,
                self.stats.draw_height,
                self.stats.scaled,
            );
            if self.title_state == Some(state) {
                return;
            }
            self.title_state = Some(state);
            let title = format!(
                "{} - {}x{} {} {} {}",
                self.base_title,
                self.stats.video_width,
                self.stats.video_height,
                self.scale_mode.name(),
                self.window_mode.name(),
                if self.stats.scaled {
                    "scaled"
                } else {
                    "no-scale"
                }
            );
            let wide: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
            let _ = unsafe { SetWindowTextW(self.hwnd, PCWSTR(wide.as_ptr())) };
        }

        fn refresh_display_capability(&mut self, force: bool) {
            let now = Instant::now();
            let epoch = DISPLAY_CHANGE_EPOCH.load(Ordering::Acquire);
            if !force && epoch == self.display_epoch_seen && now < self.display_check_at {
                return;
            }
            self.display_epoch_seen = epoch;
            self.display_check_at = now + DISPLAY_REFRESH_POLL;
            let detected = crate::display_capability::detect_window_display(
                self.hwnd,
                self.display_refresh_detect,
                0,
            );
            let changed = !self.stats.display.same_active_mode(&detected);
            self.stats.display = crate::display_capability::reconcile_display_generation(
                &self.stats.display,
                detected,
            );
            if !changed {
                return;
            }
            if self.window_mode == WindowMode::BorderlessFullscreen && self.stats.initialized {
                if let Err(err) = self.size_to_current_monitor() {
                    self.stats.last_error = Some(err);
                }
            }
        }
    }

    impl Drop for GdiViewerWindow {
        fn drop(&mut self) {
            if !self.closed {
                let _ = unsafe { DestroyWindow(self.hwnd) };
            }
        }
    }

    fn validate_bgra(bgra: &[u8], width: u32, height: u32, stride: usize) -> Result<(), String> {
        let expected_stride = (width as usize)
            .checked_mul(4)
            .ok_or_else(|| "BGRA stride overflow".to_string())?;
        if stride != expected_stride {
            return Err(format!(
                "GDI BGRA stride must be width * 4: width={width}, stride={stride}, expected={expected_stride}"
            ));
        }
        let expected = stride
            .checked_mul(height as usize)
            .ok_or_else(|| "BGRA frame size overflow".to_string())?;
        if bgra.len() < expected {
            return Err(format!(
                "BGRA frame too small: expected {expected}, got {}",
                bgra.len()
            ));
        }
        Ok(())
    }

    fn set_process_dpi_awareness() -> DpiAwarenessInfo {
        type SetProcessDpiAwarenessContextFn = unsafe extern "system" fn(isize) -> BOOL;

        if let Ok(user32) = unsafe { GetModuleHandleW(w!("user32.dll")) } {
            if let Some(proc) =
                unsafe { GetProcAddress(user32, s!("SetProcessDpiAwarenessContext")) }
            {
                let set_context: SetProcessDpiAwarenessContextFn = unsafe { transmute(proc) };
                if unsafe { set_context(PER_MONITOR_AWARE_V2) }.as_bool() {
                    return DpiAwarenessInfo {
                        set: true,
                        mode: "per-monitor-aware-v2",
                    };
                }
            }
        }
        if unsafe { SetProcessDPIAware() }.as_bool() {
            DpiAwarenessInfo {
                set: true,
                mode: "system-aware-fallback",
            }
        } else {
            DpiAwarenessInfo::default()
        }
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_GETMINMAXINFO if lparam.0 != 0 => {
                // Exact windowed rendering intentionally permits a video-sized client area
                // whose non-client frame extends beyond the monitor work area. Windows' default
                // max-track size otherwise clamps 1920x1080 to the taskbar work area.
                let limits = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
                limits.ptMaxTrackSize.x = i16::MAX as i32;
                limits.ptMaxTrackSize.y = i16::MAX as i32;
                LRESULT(0)
            }
            WM_DISPLAYCHANGE | WM_MOVE | WM_WINDOWPOSCHANGED | WM_DPICHANGED => {
                DISPLAY_CHANGE_EPOCH.fetch_add(1, Ordering::AcqRel);
                unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
            }
            WM_KEYDOWN if wparam.0 == 0x1b => {
                let _ = unsafe { DestroyWindow(hwnd) };
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = unsafe { DestroyWindow(hwnd) };
                LRESULT(0)
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }

    pub use GdiViewerWindow as Window;
}

#[cfg(windows)]
pub use platform::Window as GdiViewerWindow;

pub fn run_self_test() -> Result<(), String> {
    if RenderScaleMode::parse("exact")? != RenderScaleMode::Exact
        || RenderScaleMode::parse("fit")? != RenderScaleMode::Fit
        || RenderScaleMode::parse("stretch")? != RenderScaleMode::Stretch
        || RenderScaleMode::parse("invalid").is_ok()
    {
        return Err("render scale mode parsing failed".to_string());
    }
    if WindowMode::parse("windowed")? != WindowMode::Windowed
        || WindowMode::parse("borderless-fullscreen")? != WindowMode::BorderlessFullscreen
        || WindowMode::parse("invalid").is_ok()
    {
        return Err("window mode parsing failed".to_string());
    }
    let exact = calculate_draw_rect(
        RenderScaleMode::Exact,
        WindowMode::Windowed,
        1600,
        900,
        1600,
        900,
    );
    let fullscreen_exact = calculate_draw_rect(
        RenderScaleMode::Exact,
        WindowMode::BorderlessFullscreen,
        1600,
        900,
        1920,
        1080,
    );
    let fullscreen_crop = calculate_draw_rect(
        RenderScaleMode::Exact,
        WindowMode::BorderlessFullscreen,
        2560,
        1440,
        1920,
        1080,
    );
    let fit = calculate_draw_rect(
        RenderScaleMode::Fit,
        WindowMode::Windowed,
        1600,
        900,
        960,
        600,
    );
    let stretch = calculate_draw_rect(
        RenderScaleMode::Stretch,
        WindowMode::Windowed,
        1600,
        900,
        960,
        600,
    );
    if exact.width != 1600
        || exact.height != 900
        || fullscreen_exact.x != 160
        || fullscreen_exact.y != 90
        || fullscreen_crop.x != -320
        || fullscreen_crop.y != -180
        || fit.width != 960
        || fit.height != 540
        || fit.y != 30
        || stretch.width != 960
        || stretch.height != 600
    {
        return Err("GDI render layout calculation failed".to_string());
    }
    Ok(())
}
