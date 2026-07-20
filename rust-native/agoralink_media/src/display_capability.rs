use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayRefreshDetect {
    Auto,
    Off,
}

impl DisplayRefreshDetect {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "off",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "off" => Ok(Self::Off),
            _ => Err("display-refresh-detect must be auto or off".to_string()),
        }
    }
}

impl Default for DisplayRefreshDetect {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RefreshRate {
    pub numerator: u32,
    pub denominator: u32,
}

impl RefreshRate {
    pub const fn new(numerator: u32, denominator: u32) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    pub fn validate(self) -> Result<Self, String> {
        if self.numerator == 0 || self.denominator == 0 {
            return Err("display refresh numerator and denominator must be non-zero".to_string());
        }
        let hz = self.hz();
        if !hz.is_finite() || !(10.0..=1000.0).contains(&hz) {
            return Err(format!(
                "display refresh is outside the supported range: {hz:.3}Hz"
            ));
        }
        Ok(self)
    }

    pub fn hz(self) -> f64 {
        if self.denominator == 0 {
            0.0
        } else {
            f64::from(self.numerator) / f64::from(self.denominator)
        }
    }

    pub fn rounded_hz(self) -> u32 {
        self.hz().round().clamp(0.0, f64::from(u32::MAX)) as u32
    }
}

impl Default for RefreshRate {
    fn default() -> Self {
        Self::new(0, 1)
    }
}

impl fmt::Display for RefreshRate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}/{} ({:.3}Hz)",
            self.numerator,
            self.denominator,
            self.hz()
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DisplayCapability {
    pub refresh: RefreshRate,
    pub display_width: u32,
    pub display_height: u32,
    pub display_identifier: String,
    pub display_generation: u64,
    pub detection_method: String,
    pub detection_error: Option<String>,
}

impl Default for DisplayCapability {
    fn default() -> Self {
        Self::unavailable(0, "not-detected", None)
    }
}

impl DisplayCapability {
    pub fn unavailable(generation: u64, method: &str, detection_error: Option<String>) -> Self {
        Self {
            refresh: RefreshRate::default(),
            display_width: 0,
            display_height: 0,
            display_identifier: "unknown".to_string(),
            display_generation: generation,
            detection_method: method.to_string(),
            detection_error,
        }
    }

    pub fn detection_off(generation: u64) -> Self {
        Self::unavailable(generation, "off", None)
    }

    pub fn is_available(&self) -> bool {
        self.refresh.validate().is_ok()
            && self.display_width > 0
            && self.display_height > 0
            && self.display_identifier != "unknown"
    }

    pub fn same_active_mode(&self, other: &Self) -> bool {
        self.refresh == other.refresh
            && self.display_width == other.display_width
            && self.display_height == other.display_height
            && self.display_identifier == other.display_identifier
            && self.detection_method == other.detection_method
            && self.detection_error == other.detection_error
    }

    pub fn json_fragment(&self, prefix: &str) -> String {
        format!(
            concat!(
                r#""{}_display_id":"{}","{}_display_generation":{},"#,
                r#""{}_refresh_num":{},"{}_refresh_den":{},"{}_refresh_hz":{:.3},"#,
                r#""{}_display_width":{},"{}_display_height":{},"#,
                r#""{}_display_detection_method":"{}","{}_display_detection_error":{}"#
            ),
            prefix,
            json_escape(&self.display_identifier),
            prefix,
            self.display_generation,
            prefix,
            self.refresh.numerator,
            prefix,
            self.refresh.denominator,
            prefix,
            self.refresh.hz(),
            prefix,
            self.display_width,
            prefix,
            self.display_height,
            prefix,
            json_escape(&self.detection_method),
            prefix,
            optional_json_string(self.detection_error.as_deref()),
        )
    }
}

pub fn reconcile_display_generation(
    previous: &DisplayCapability,
    mut detected: DisplayCapability,
) -> DisplayCapability {
    detected.display_generation = if previous.same_active_mode(&detected) {
        previous.display_generation
    } else {
        previous.display_generation.saturating_add(1).max(1)
    };
    detected
}

fn json_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

fn optional_json_string(value: Option<&str>) -> String {
    value.map_or_else(
        || "null".to_string(),
        |value| format!(r#""{}""#, json_escape(value)),
    )
}

#[cfg(windows)]
mod platform {
    use std::mem::size_of;

    use windows::core::PCWSTR;
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
        QDC_ONLY_ACTIVE_PATHS,
    };
    use windows::Win32::Foundation::{
        ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HWND, POINT, WIN32_ERROR,
    };
    use windows::Win32::Graphics::Gdi::{
        EnumDisplaySettingsExW, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, DEVMODEW,
        ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_FLAGS, HMONITOR, MONITORINFOEXW,
        MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
    };

    use super::{DisplayCapability, RefreshRate};

    const QUERY_RETRIES: usize = 4;

    pub fn detect_primary(generation: u64) -> DisplayCapability {
        let monitor = unsafe { MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY) };
        detect_monitor(monitor, generation)
    }

    pub fn detect_window(hwnd: HWND, generation: u64) -> DisplayCapability {
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        detect_monitor(monitor, generation)
    }

    fn detect_monitor(monitor: HMONITOR, generation: u64) -> DisplayCapability {
        if monitor.is_invalid() {
            return DisplayCapability::unavailable(
                generation,
                "monitor-selection",
                Some("MonitorFromWindow/MonitorFromPoint returned an invalid monitor".to_string()),
            );
        }
        let (device_name, width, height) = match monitor_identity(monitor) {
            Ok(identity) => identity,
            Err(err) => {
                return DisplayCapability::unavailable(generation, "GetMonitorInfoW", Some(err))
            }
        };
        match query_display_config_refresh(&device_name) {
            Ok(refresh) => DisplayCapability {
                refresh,
                display_width: width,
                display_height: height,
                display_identifier: device_name,
                display_generation: generation,
                detection_method: "QueryDisplayConfig".to_string(),
                detection_error: None,
            },
            Err(query_error) => match enum_display_settings_refresh(&device_name) {
                Ok(refresh) => DisplayCapability {
                    refresh,
                    display_width: width,
                    display_height: height,
                    display_identifier: device_name,
                    display_generation: generation,
                    detection_method: "EnumDisplaySettingsExW".to_string(),
                    detection_error: Some(query_error),
                },
                Err(fallback_error) => DisplayCapability {
                    refresh: RefreshRate::default(),
                    display_width: width,
                    display_height: height,
                    display_identifier: device_name,
                    display_generation: generation,
                    detection_method: "unavailable".to_string(),
                    detection_error: Some(format!(
                        "QueryDisplayConfig: {query_error}; EnumDisplaySettingsExW: {fallback_error}"
                    )),
                },
            },
        }
    }

    fn monitor_identity(monitor: HMONITOR) -> Result<(String, u32, u32), String> {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if !unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
            return Err(format!(
                "GetMonitorInfoW failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let device_name = wide_string(&info.szDevice);
        if device_name.is_empty() {
            return Err("GetMonitorInfoW returned an empty display device name".to_string());
        }
        let rect = info.monitorInfo.rcMonitor;
        Ok((
            device_name,
            (rect.right - rect.left).max(0) as u32,
            (rect.bottom - rect.top).max(0) as u32,
        ))
    }

    fn query_display_config_refresh(device_name: &str) -> Result<RefreshRate, String> {
        let flags = QDC_ONLY_ACTIVE_PATHS;
        for _ in 0..QUERY_RETRIES {
            let mut path_count = 0u32;
            let mut mode_count = 0u32;
            win32_ok(
                unsafe { GetDisplayConfigBufferSizes(flags, &mut path_count, &mut mode_count) },
                "GetDisplayConfigBufferSizes",
            )?;
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
            let result = unsafe {
                QueryDisplayConfig(
                    flags,
                    &mut path_count,
                    paths.as_mut_ptr(),
                    &mut mode_count,
                    modes.as_mut_ptr(),
                    None,
                )
            };
            if result == ERROR_INSUFFICIENT_BUFFER {
                continue;
            }
            win32_ok(result, "QueryDisplayConfig")?;
            paths.truncate(path_count as usize);
            for path in paths {
                let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
                source.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: path.sourceInfo.adapterId,
                    id: path.sourceInfo.id,
                };
                let result = unsafe { DisplayConfigGetDeviceInfo(&mut source.header) };
                if result != 0
                    || !wide_string(&source.viewGdiDeviceName).eq_ignore_ascii_case(device_name)
                {
                    continue;
                }
                return RefreshRate::new(
                    path.targetInfo.refreshRate.Numerator,
                    path.targetInfo.refreshRate.Denominator,
                )
                .validate();
            }
            return Err(format!(
                "active display path was not found for {device_name}"
            ));
        }
        Err("display topology changed repeatedly during QueryDisplayConfig".to_string())
    }

    fn enum_display_settings_refresh(device_name: &str) -> Result<RefreshRate, String> {
        let device_wide: Vec<u16> = device_name.encode_utf16().chain(Some(0)).collect();
        let mut mode = DEVMODEW::default();
        mode.dmSize = size_of::<DEVMODEW>() as u16;
        let ok = unsafe {
            EnumDisplaySettingsExW(
                PCWSTR(device_wide.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                &mut mode,
                ENUM_DISPLAY_SETTINGS_FLAGS(0),
            )
        };
        if !ok.as_bool() {
            return Err(format!(
                "EnumDisplaySettingsExW failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        RefreshRate::new(mode.dmDisplayFrequency, 1).validate()
    }

    fn win32_ok(result: WIN32_ERROR, operation: &str) -> Result<(), String> {
        if result == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("{operation} failed with Win32 error {}", result.0))
        }
    }

    fn wide_string(value: &[u16]) -> String {
        let len = value
            .iter()
            .position(|code| *code == 0)
            .unwrap_or(value.len());
        String::from_utf16_lossy(&value[..len])
    }
}

#[cfg(windows)]
pub fn detect_primary_display(mode: DisplayRefreshDetect, generation: u64) -> DisplayCapability {
    match mode {
        DisplayRefreshDetect::Auto => platform::detect_primary(generation),
        DisplayRefreshDetect::Off => DisplayCapability::detection_off(generation),
    }
}

#[cfg(windows)]
pub fn detect_window_display(
    hwnd: windows::Win32::Foundation::HWND,
    mode: DisplayRefreshDetect,
    generation: u64,
) -> DisplayCapability {
    match mode {
        DisplayRefreshDetect::Auto => platform::detect_window(hwnd, generation),
        DisplayRefreshDetect::Off => DisplayCapability::detection_off(generation),
    }
}

#[cfg(not(windows))]
pub fn detect_primary_display(mode: DisplayRefreshDetect, generation: u64) -> DisplayCapability {
    match mode {
        DisplayRefreshDetect::Off => DisplayCapability::detection_off(generation),
        DisplayRefreshDetect::Auto => DisplayCapability::unavailable(
            generation,
            "unsupported-platform",
            Some("display capability detection is only available on Windows".to_string()),
        ),
    }
}

pub fn run_self_test() -> Result<(), String> {
    let ntsc = RefreshRate::new(60_000, 1001).validate()?;
    if (ntsc.hz() - 59.940_059_94).abs() > 0.000_1 || ntsc.rounded_hz() != 60 {
        return Err("60000/1001 refresh handling failed".to_string());
    }
    for rate in [60, 75, 90, 120, 144] {
        let refresh = RefreshRate::new(rate, 1).validate()?;
        if refresh.rounded_hz() != rate {
            return Err(format!("{rate}Hz refresh handling failed"));
        }
    }
    if RefreshRate::new(0, 1).validate().is_ok() || RefreshRate::new(60, 0).validate().is_ok() {
        return Err("invalid refresh rational was accepted".to_string());
    }
    let display = |refresh, generation| DisplayCapability {
        refresh: RefreshRate::new(refresh, 1),
        display_width: 1920,
        display_height: 1080,
        display_identifier: format!(r#"\\.\DISPLAY{refresh}"#),
        display_generation: generation,
        detection_method: "deterministic-test".to_string(),
        detection_error: None,
    };
    let initial = display(60, 7);
    let unchanged = reconcile_display_generation(&initial, display(60, 0));
    if unchanged.display_generation != 7 {
        return Err("unchanged display mode advanced generation".to_string());
    }
    let changed = reconcile_display_generation(&unchanged, display(144, 0));
    if changed.display_generation != 8 || changed.refresh != RefreshRate::new(144, 1) {
        return Err("monitor refresh change did not advance generation".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn rational_refresh_and_generation_are_deterministic() {
        super::run_self_test().expect("display capability self-test");
    }
}
