#[cfg(windows)]
mod platform {
    use std::collections::HashSet;
    use std::ffi::c_void;
    use std::io::{self, Write};
    use std::ptr;
    use std::slice;

    use windows::core::{GUID, PWSTR};
    use windows::Win32::Media::MediaFoundation::{
        IMFActivate, MFMediaType_Video, MFShutdown, MFStartup, MFTEnumEx, MFTGetInfo,
        MFT_ENUM_HARDWARE_URL_Attribute, MFT_ENUM_HARDWARE_VENDOR_ID_Attribute,
        MFT_FRIENDLY_NAME_Attribute, MFT_TRANSFORM_CLSID_Attribute, MFVideoFormat_ARGB32,
        MFVideoFormat_H264, MFVideoFormat_H264_ES, MFVideoFormat_I420, MFVideoFormat_IYUV,
        MFVideoFormat_NV12, MFVideoFormat_P010, MFVideoFormat_P016, MFVideoFormat_RGB24,
        MFVideoFormat_RGB32, MFVideoFormat_YUY2, MFVideoFormat_YV12, MFSTARTUP_FULL,
        MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_ALL, MFT_ENUM_FLAG_SORTANDFILTER,
        MFT_REGISTER_TYPE_INFO, MF_TRANSFORM_ASYNC, MF_VERSION,
    };
    use windows::Win32::System::Com::{
        CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED,
    };

    struct ComGuard;

    impl ComGuard {
        fn initialize() -> Result<Self, String> {
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
                .ok()
                .map_err(|err| format!("CoInitializeEx failed: {err}"))?;
            Ok(Self)
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    struct MediaFoundationGuard;

    impl MediaFoundationGuard {
        fn startup() -> Result<Self, String> {
            unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }
                .map_err(|err| format!("MFStartup failed: {err}"))?;
            Ok(Self)
        }
    }

    impl Drop for MediaFoundationGuard {
        fn drop(&mut self) {
            let _ = unsafe { MFShutdown() };
        }
    }

    #[derive(Debug, Default)]
    struct RegisteredTypes {
        name: Option<String>,
        input_formats: Vec<String>,
        output_formats: Vec<String>,
    }

    pub fn run() -> Result<(), String> {
        let _com = ComGuard::initialize()?;
        let _mf = MediaFoundationGuard::startup()?;

        let output_type = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };
        let flags = MFT_ENUM_FLAG_ALL | MFT_ENUM_FLAG_SORTANDFILTER;
        let mut activates: *mut Option<IMFActivate> = ptr::null_mut();
        let mut count = 0u32;

        unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                flags,
                None,
                Some(&output_type),
                &mut activates,
                &mut count,
            )
        }
        .map_err(|err| format!("MFTEnumEx failed: {err}"))?;

        eprintln!("wmf-probe H.264 encoder activation count={count}");
        let mut emitted = 0u32;
        let mut duplicates_skipped = 0u32;
        let mut seen_clsids = HashSet::new();

        if !activates.is_null() {
            for index in 0..count as usize {
                let activate = unsafe { ptr::read(activates.add(index)) };
                let Some(activate) = activate else {
                    continue;
                };

                match inspect_encoder(&activate) {
                    Ok((clsid, line)) if seen_clsids.insert(clsid) => {
                        println!("{line}");
                        emitted += 1;
                    }
                    Ok((_clsid, _line)) => {
                        duplicates_skipped += 1;
                    }
                    Err(err) => {
                        eprintln!("wmf-probe encoder index={index} inspect failed: {err}");
                    }
                }
            }
            unsafe { CoTaskMemFree(Some(activates.cast::<c_void>())) };
        }

        println!(
            r#"{{"type":"WMF_PROBE_DONE","h264_encoders":{},"enumerated":{},"duplicates_skipped":{}}}"#,
            emitted, count, duplicates_skipped
        );
        io::stdout().flush().ok();
        Ok(())
    }

    fn inspect_encoder(activate: &IMFActivate) -> Result<(GUID, String), String> {
        let clsid = unsafe { activate.GetGUID(&MFT_TRANSFORM_CLSID_Attribute) }
            .map_err(|err| format!("missing transform CLSID: {err}"))?;
        let friendly_name =
            get_allocated_string(activate, &MFT_FRIENDLY_NAME_Attribute).unwrap_or_default();
        let hardware_url =
            get_allocated_string(activate, &MFT_ENUM_HARDWARE_URL_Attribute).unwrap_or_default();
        let hardware_vendor =
            get_allocated_string(activate, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute)
                .unwrap_or_default();
        let async_mft = unsafe { activate.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) != 0;
        let hardware = !hardware_url.is_empty() || !hardware_vendor.is_empty();
        let registered = get_registered_types(clsid).unwrap_or_default();
        let name = if friendly_name.is_empty() {
            registered
                .name
                .unwrap_or_else(|| "Unnamed H.264 encoder".to_string())
        } else {
            friendly_name
        };

        Ok((
            clsid,
            format!(
                r#"{{"type":"WMF_ENCODER","codec":"h264","name":"{}","clsid":"{}","hardware":{},"async":{},"is_hardware_or_async_hint":{},"input_formats":{},"output_formats":{}}}"#,
                json_escape(&name),
                guid_string(&clsid),
                hardware,
                async_mft,
                hardware || async_mft,
                json_string_array(&registered.input_formats),
                json_string_array(&registered.output_formats)
            ),
        ))
    }

    fn get_registered_types(clsid: GUID) -> Result<RegisteredTypes, String> {
        let mut name = PWSTR::null();
        let mut input_types: *mut MFT_REGISTER_TYPE_INFO = ptr::null_mut();
        let mut input_count = 0u32;
        let mut output_types: *mut MFT_REGISTER_TYPE_INFO = ptr::null_mut();
        let mut output_count = 0u32;

        let info_result = unsafe {
            MFTGetInfo(
                clsid,
                Some(&mut name),
                Some(&mut input_types),
                Some(&mut input_count),
                Some(&mut output_types),
                Some(&mut output_count),
                None,
            )
        };
        let result = if info_result.is_ok() {
            Some(RegisteredTypes {
                name: pwstr_to_string(name),
                input_formats: type_names(input_types, input_count),
                output_formats: type_names(output_types, output_count),
            })
        } else {
            None
        };
        unsafe {
            if !name.is_null() {
                CoTaskMemFree(Some(name.as_ptr().cast::<c_void>()));
            }
            if !input_types.is_null() {
                CoTaskMemFree(Some(input_types.cast::<c_void>()));
            }
            if !output_types.is_null() {
                CoTaskMemFree(Some(output_types.cast::<c_void>()));
            }
        }
        match (info_result, result) {
            (Ok(()), Some(result)) => Ok(result),
            (Err(err), _) => Err(format!("MFTGetInfo failed: {err}")),
            _ => Err("MFTGetInfo returned no result".to_string()),
        }
    }

    fn get_allocated_string(activate: &IMFActivate, key: &GUID) -> Option<String> {
        let mut value = PWSTR::null();
        let mut length = 0u32;
        if unsafe { activate.GetAllocatedString(key, &mut value, &mut length) }.is_err() {
            return None;
        }
        let result = if value.is_null() {
            None
        } else {
            let text = unsafe {
                String::from_utf16_lossy(slice::from_raw_parts(value.0, length as usize))
            };
            Some(text)
        };
        if !value.is_null() {
            unsafe { CoTaskMemFree(Some(value.as_ptr().cast::<c_void>())) };
        }
        result
    }

    fn pwstr_to_string(value: PWSTR) -> Option<String> {
        if value.is_null() {
            None
        } else {
            unsafe { value.to_string().ok() }
        }
    }

    fn type_names(types: *const MFT_REGISTER_TYPE_INFO, count: u32) -> Vec<String> {
        if types.is_null() || count == 0 {
            return Vec::new();
        }
        unsafe { slice::from_raw_parts(types, count as usize) }
            .iter()
            .filter(|entry| entry.guidMajorType == MFMediaType_Video)
            .map(|entry| video_format_name(&entry.guidSubtype))
            .collect()
    }

    fn video_format_name(subtype: &GUID) -> String {
        let known = [
            (&MFVideoFormat_NV12, "NV12"),
            (&MFVideoFormat_I420, "I420"),
            (&MFVideoFormat_IYUV, "IYUV"),
            (&MFVideoFormat_YV12, "YV12"),
            (&MFVideoFormat_YUY2, "YUY2"),
            (&MFVideoFormat_P010, "P010"),
            (&MFVideoFormat_P016, "P016"),
            (&MFVideoFormat_RGB24, "RGB24"),
            (&MFVideoFormat_RGB32, "RGB32"),
            (&MFVideoFormat_ARGB32, "ARGB32"),
            (&MFVideoFormat_H264, "H264"),
            (&MFVideoFormat_H264_ES, "H264_ES"),
        ];
        known
            .iter()
            .find_map(|(guid, name)| (*guid == subtype).then_some((*name).to_string()))
            .unwrap_or_else(|| guid_string(subtype))
    }

    fn guid_string(guid: &GUID) -> String {
        format!("{{{guid:?}}}")
    }

    fn json_string_array(values: &[String]) -> String {
        let items: Vec<String> = values
            .iter()
            .map(|value| format!(r#""{}""#, json_escape(value)))
            .collect();
        format!("[{}]", items.join(","))
    }

    fn json_escape(text: &str) -> String {
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\r', "\\r")
            .replace('\n', "\\n")
    }
}

#[cfg(windows)]
pub use platform::run;

#[cfg(not(windows))]
pub fn run() -> Result<(), String> {
    Err("wmf-probe is only supported on Windows".to_string())
}
