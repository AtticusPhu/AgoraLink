#[cfg(windows)]
mod platform {
    use std::mem::size_of;

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        GetDC, ReleaseDC, StretchDIBits, UpdateWindow, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS, SRCCOPY,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
        LoadCursorW, PeekMessageW, PostQuitMessage, RegisterClassW, ShowWindow, TranslateMessage,
        CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, IDC_ARROW, MSG, PM_REMOVE, SW_SHOW, WINDOW_EX_STYLE,
        WM_CLOSE, WM_DESTROY, WM_QUIT, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    const WINDOW_CLASS: PCWSTR = w!("AgoraLinkH264FileViewer");

    pub struct GdiViewerWindow {
        hwnd: HWND,
        closed: bool,
    }

    impl GdiViewerWindow {
        pub fn create(title: &str) -> Result<Self, String> {
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
                return Err(format!(
                    "RegisterClassW failed: {}",
                    std::io::Error::last_os_error()
                ));
            }

            let title_wide: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
            let hwnd = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    WINDOW_CLASS,
                    PCWSTR(title_wide.as_ptr()),
                    WS_OVERLAPPEDWINDOW | WS_VISIBLE,
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
            Ok(Self {
                hwnd,
                closed: false,
            })
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
            true
        }

        pub fn render_bgra(&mut self, bgra: &[u8], width: u32, height: u32) -> Result<(), String> {
            if !self.pump_messages() {
                return Err("viewer window was closed".to_string());
            }
            let expected = (width as usize)
                .checked_mul(height as usize)
                .and_then(|pixels| pixels.checked_mul(4))
                .ok_or_else(|| "BGRA frame size overflow".to_string())?;
            if bgra.len() < expected {
                return Err(format!(
                    "BGRA frame too small: expected {expected}, got {}",
                    bgra.len()
                ));
            }

            let mut client = RECT::default();
            unsafe { GetClientRect(self.hwnd, &mut client) }
                .map_err(|err| format!("GetClientRect failed: {err}"))?;
            let client_width = (client.right - client.left).max(1);
            let client_height = (client.bottom - client.top).max(1);
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
            let rendered = unsafe {
                StretchDIBits(
                    hdc,
                    0,
                    0,
                    client_width,
                    client_height,
                    0,
                    0,
                    width as i32,
                    height as i32,
                    Some(bgra.as_ptr().cast()),
                    &raw mut bitmap,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                )
            };
            unsafe {
                ReleaseDC(Some(self.hwnd), hdc);
            }
            if rendered == 0 {
                return Err(format!(
                    "StretchDIBits failed: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(())
        }
    }

    impl Drop for GdiViewerWindow {
        fn drop(&mut self) {
            if !self.closed {
                let _ = unsafe { DestroyWindow(self.hwnd) };
            }
        }
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
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
}

#[cfg(windows)]
pub use platform::GdiViewerWindow;
