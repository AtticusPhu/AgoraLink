#[cfg(windows)]
mod platform {
    use std::collections::VecDeque;
    use std::ffi::c_void;
    use std::fs::File;
    use std::io::Write;
    use std::mem::ManuallyDrop;
    use std::path::Path;
    use std::ptr;
    use std::slice;
    use std::thread;
    use std::time::{Duration, Instant};

    use crate::async_mft_wait::{
        poll_until, AsyncMftPollError, AsyncMftWaitFailure, AsyncMftWaitKind,
    };
    use crate::color_spec::{ColorMatrix, ColorSpec, MediaColorMetadata};
    use windows::core::{IUnknown, Interface, Result as WindowsResult, GUID, PWSTR};
    use windows::Win32::Media::MediaFoundation::{
        eAVEncH264VProfile_Main, CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncMPVGOPSize,
        CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVEncVideoNumGOPsPerIDR, ICodecAPI, IMFActivate,
        IMFMediaBuffer, IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFTransform, MEError,
        METransformDrainComplete, METransformHaveOutput, METransformNeedInput, MFCreateMediaType,
        MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFNominalRange_16_235,
        MFSampleExtension_CleanPoint, MFShutdown, MFStartup, MFTEnumEx,
        MFT_ENUM_HARDWARE_URL_Attribute, MFT_ENUM_HARDWARE_VENDOR_ID_Attribute,
        MFT_FRIENDLY_NAME_Attribute, MFT_TRANSFORM_CLSID_Attribute, MFVideoFormat_H264,
        MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFVideoPrimaries_BT709,
        MFVideoPrimaries_SMPTE170M, MFVideoTransFunc_709, MFVideoTransferMatrix_BT601,
        MFVideoTransferMatrix_BT709, MFSTARTUP_FULL, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_ALL,
        MFT_ENUM_FLAG_SORTANDFILTER, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
        MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_END_STREAMING,
        MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
        MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_INFO,
        MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MF_EVENT_FLAG_NO_WAIT,
        MF_E_NOTACCEPTING, MF_E_NO_EVENTS_AVAILABLE, MF_E_TRANSFORM_NEED_MORE_INPUT,
        MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AVG_BITRATE,
        MF_MT_DEFAULT_STRIDE, MF_MT_FIXED_SIZE_SAMPLES, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
        MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO,
        MF_MT_SAMPLE_SIZE, MF_MT_SUBTYPE, MF_MT_TRANSFER_FUNCTION, MF_MT_VIDEO_NOMINAL_RANGE,
        MF_MT_VIDEO_PRIMARIES, MF_MT_YUV_MATRIX, MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK,
        MF_VERSION,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Variant::VARIANT;

    pub const ENCODER_NAME: &str = "Microsoft H264 Encoder MFT";
    const SOFTWARE_H264_ENCODER_CLSID: GUID =
        GUID::from_u128(0x6ca50344_051a_4ded_9779_a43305165e35);
    const HNS_PER_SECOND: i64 = 10_000_000;
    const MIN_OUTPUT_BUFFER_SIZE: u32 = 1_048_576;
    const ASYNC_NEED_INPUT_TIMEOUT: Duration = Duration::from_secs(1);
    const ASYNC_DRAIN_TIMEOUT: Duration = Duration::from_secs(3);
    const ASYNC_EVENT_POLL_INTERVAL: Duration = Duration::from_millis(1);

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum EncoderChoice {
        Auto,
        Hardware,
        Software,
        Microsoft,
        IntelQsv,
    }

    impl EncoderChoice {
        pub const fn name(self) -> &'static str {
            match self {
                Self::Auto => "auto",
                Self::Hardware => "hardware",
                Self::Software => "software",
                Self::Microsoft => "microsoft",
                Self::IntelQsv => "intel-qsv",
            }
        }
    }

    impl Default for EncoderChoice {
        fn default() -> Self {
            Self::Auto
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum EncoderKind {
        Hardware,
        Software,
    }

    impl EncoderKind {
        pub const fn name(self) -> &'static str {
            match self {
                Self::Hardware => "hardware",
                Self::Software => "software",
            }
        }
    }

    #[derive(Clone, Debug)]
    pub struct EncoderSelection {
        pub requested: EncoderChoice,
        pub selected_name: String,
        pub clsid: String,
        pub kind: EncoderKind,
        pub fallback: bool,
        pub fallback_reason: Option<String>,
        pub async_mft: bool,
        pub hardware_url: Option<String>,
        pub hardware_vendor: Option<String>,
        pub activation_entries_skipped: u64,
        pub activation_skip_missing_clsid: u64,
        pub activation_skip_other: u64,
    }

    impl EncoderSelection {
        pub fn hardware_accelerated(&self) -> bool {
            self.kind == EncoderKind::Hardware
        }

        pub fn json_fragment(&self) -> String {
            format!(
                r#""encoder_requested":"{}","encoder_selected":"{}","encoder_clsid":"{}","encoder_kind":"{}","encoder_fallback":{},"encoder_fallback_reason":{},"hardware_accelerated":{},"encoder_async":{},"encoder_hardware_url":{},"encoder_hardware_vendor":{},"encoder_activation_entries_skipped":{},"encoder_activation_skip_reason_counts":{{"missing-transform-clsid":{},"other":{}}}"#,
                self.requested.name(),
                json_escape(&self.selected_name),
                json_escape(&self.clsid),
                self.kind.name(),
                self.fallback,
                optional_json_string(self.fallback_reason.as_deref()),
                self.hardware_accelerated(),
                self.async_mft,
                optional_json_string(self.hardware_url.as_deref()),
                optional_json_string(self.hardware_vendor.as_deref()),
                self.activation_entries_skipped,
                self.activation_skip_missing_clsid,
                self.activation_skip_other,
            )
        }
    }

    impl Default for EncoderSelection {
        fn default() -> Self {
            Self {
                requested: EncoderChoice::Software,
                selected_name: ENCODER_NAME.to_string(),
                clsid: guid_string(&SOFTWARE_H264_ENCODER_CLSID),
                kind: EncoderKind::Software,
                fallback: false,
                fallback_reason: None,
                async_mft: false,
                hardware_url: None,
                hardware_vendor: None,
                activation_entries_skipped: 0,
                activation_skip_missing_clsid: 0,
                activation_skip_other: 0,
            }
        }
    }

    #[derive(Clone, Copy, Debug, Default)]
    pub struct EncoderStats {
        pub frames_in: u64,
        pub samples_out: u64,
        pub bytes_out: u64,
        pub keyframes: u64,
        pub keyframe_detection_available: bool,
        pub async_wait_timeouts: u64,
        pub async_wait_cancelled: u64,
        pub async_drain_timeouts: u64,
    }

    #[derive(Debug)]
    pub struct EncodedSample {
        pub bytes: Vec<u8>,
        pub keyframe: Option<bool>,
        pub sample_time_hns: Option<i64>,
    }

    #[derive(Clone, Debug, Default)]
    pub struct EncoderKeyframeControl {
        pub config_method: String,
        pub config_applied: bool,
        pub config_error: Option<String>,
        pub force_supported: bool,
    }

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

    enum OutputResult {
        Produced,
        NeedMoreInput,
        StreamChange,
    }

    pub struct WmfH264Encoder {
        transform: IMFTransform,
        output_info: MFT_OUTPUT_STREAM_INFO,
        output_buffer_size: u32,
        output: Option<File>,
        collect_samples: bool,
        pending_samples: VecDeque<EncodedSample>,
        frame_size: usize,
        fps: u32,
        stats: EncoderStats,
        finished: bool,
        profile_main: bool,
        selection: EncoderSelection,
        #[allow(dead_code)]
        color_spec: ColorSpec,
        input_color_metadata: MediaColorMetadata,
        output_color_metadata: MediaColorMetadata,
        event_generator: Option<IMFMediaEventGenerator>,
        async_need_input: u32,
        async_drain_complete: bool,
        async_cancellation: Option<crate::shutdown::CancellationToken>,
        keyframe_control: EncoderKeyframeControl,
        _mf: MediaFoundationGuard,
        _com: ComGuard,
    }

    impl WmfH264Encoder {
        #[allow(dead_code)]
        pub fn new(
            width: u32,
            height: u32,
            fps: u32,
            bitrate_mbps: f64,
            output_path: &str,
        ) -> Result<Self, String> {
            Self::new_with_color(
                width,
                height,
                fps,
                bitrate_mbps,
                output_path,
                ColorSpec::default(),
            )
        }

        pub fn new_with_color(
            width: u32,
            height: u32,
            fps: u32,
            bitrate_mbps: f64,
            output_path: &str,
            color_spec: ColorSpec,
        ) -> Result<Self, String> {
            Self::new_internal(
                width,
                height,
                fps,
                bitrate_mbps,
                Some(output_path.to_string()),
                false,
                color_spec,
                EncoderChoice::Software,
                None,
            )
        }

        pub fn new_with_color_and_choice(
            width: u32,
            height: u32,
            fps: u32,
            bitrate_mbps: f64,
            output_path: &str,
            color_spec: ColorSpec,
            encoder_choice: EncoderChoice,
        ) -> Result<Self, String> {
            Self::new_internal(
                width,
                height,
                fps,
                bitrate_mbps,
                Some(output_path.to_string()),
                false,
                color_spec,
                encoder_choice,
                None,
            )
        }

        #[allow(dead_code)]
        pub fn new_stream(
            width: u32,
            height: u32,
            fps: u32,
            bitrate_mbps: f64,
        ) -> Result<Self, String> {
            Self::new_stream_with_color(width, height, fps, bitrate_mbps, ColorSpec::default())
        }

        pub fn new_stream_with_color(
            width: u32,
            height: u32,
            fps: u32,
            bitrate_mbps: f64,
            color_spec: ColorSpec,
        ) -> Result<Self, String> {
            Self::new_internal(
                width,
                height,
                fps,
                bitrate_mbps,
                None,
                true,
                color_spec,
                EncoderChoice::Software,
                None,
            )
        }

        #[allow(dead_code)]
        pub fn new_stream_with_color_and_choice(
            width: u32,
            height: u32,
            fps: u32,
            bitrate_mbps: f64,
            color_spec: ColorSpec,
            encoder_choice: EncoderChoice,
        ) -> Result<Self, String> {
            Self::new_internal(
                width,
                height,
                fps,
                bitrate_mbps,
                None,
                true,
                color_spec,
                encoder_choice,
                None,
            )
        }

        #[allow(clippy::too_many_arguments)]
        pub fn new_stream_with_color_choice_and_keyframe_interval(
            width: u32,
            height: u32,
            fps: u32,
            bitrate_mbps: f64,
            color_spec: ColorSpec,
            encoder_choice: EncoderChoice,
            keyframe_interval_frames: Option<u32>,
        ) -> Result<Self, String> {
            Self::new_internal(
                width,
                height,
                fps,
                bitrate_mbps,
                None,
                true,
                color_spec,
                encoder_choice,
                keyframe_interval_frames,
            )
        }

        fn new_internal(
            width: u32,
            height: u32,
            fps: u32,
            bitrate_mbps: f64,
            output_path: Option<String>,
            collect_samples: bool,
            color_spec: ColorSpec,
            encoder_choice: EncoderChoice,
            keyframe_interval_frames: Option<u32>,
        ) -> Result<Self, String> {
            if encoder_choice == EncoderChoice::Auto {
                let hardware_result = Self::new_internal_selected(
                    width,
                    height,
                    fps,
                    bitrate_mbps,
                    output_path.clone(),
                    collect_samples,
                    color_spec,
                    EncoderChoice::Hardware,
                    EncoderChoice::Auto,
                    None,
                    keyframe_interval_frames,
                );
                return match hardware_result {
                    Ok(mut encoder) => {
                        encoder.selection.requested = EncoderChoice::Auto;
                        Ok(encoder)
                    }
                    Err(hardware_error) => Self::new_internal_selected(
                        width,
                        height,
                        fps,
                        bitrate_mbps,
                        output_path,
                        collect_samples,
                        color_spec,
                        EncoderChoice::Software,
                        EncoderChoice::Auto,
                        Some(hardware_error),
                        keyframe_interval_frames,
                    ),
                };
            }
            Self::new_internal_selected(
                width,
                height,
                fps,
                bitrate_mbps,
                output_path,
                collect_samples,
                color_spec,
                encoder_choice,
                encoder_choice,
                None,
                keyframe_interval_frames,
            )
        }

        #[allow(clippy::too_many_arguments)]
        fn new_internal_selected(
            width: u32,
            height: u32,
            fps: u32,
            bitrate_mbps: f64,
            output_path: Option<String>,
            collect_samples: bool,
            color_spec: ColorSpec,
            creation_choice: EncoderChoice,
            requested_choice: EncoderChoice,
            fallback_reason: Option<String>,
            keyframe_interval_frames: Option<u32>,
        ) -> Result<Self, String> {
            if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
                return Err("encoder width and height must be non-zero even values".to_string());
            }
            if fps == 0 {
                return Err("encoder fps must be greater than zero".to_string());
            }
            let frame_size = crate::bgra_to_nv12::buffer_size(width, height)?;
            let bitrate_bps = bitrate_to_bps(bitrate_mbps)?;
            let com = ComGuard::initialize()?;
            let mf = MediaFoundationGuard::startup()?;
            let (transform, mut selection) = create_encoder(creation_choice)?;
            selection.requested = requested_choice;
            if let Some(reason) = fallback_reason {
                selection.fallback = true;
                selection.fallback_reason = Some(reason);
            }

            // Static CodecAPI properties must be set before output type negotiation and
            // before MFT_MESSAGE_NOTIFY_BEGIN_STREAMING. Hardware MFTs may otherwise
            // accept SetValue while silently retaining their default GOP.
            let keyframe_control = configure_keyframe_control(&transform, keyframe_interval_frames);

            let output_type =
                create_video_type(width, height, fps, MFVideoFormat_H264, bitrate_bps, None)?;
            apply_color_metadata(&output_type, color_spec);
            let profile_attribute_set = unsafe {
                output_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)
            }
            .is_ok();
            let profile_main = match unsafe { transform.SetOutputType(0, &output_type, 0) } {
                Ok(()) => profile_attribute_set,
                Err(profile_error) if profile_attribute_set => {
                    let fallback = create_video_type(
                        width,
                        height,
                        fps,
                        MFVideoFormat_H264,
                        bitrate_bps,
                        None,
                    )?;
                    apply_color_metadata(&fallback, color_spec);
                    unsafe { transform.SetOutputType(0, &fallback, 0) }.map_err(
                        |fallback_error| {
                            format!(
                                "SetOutputType H.264 failed; profile={profile_error}; fallback={fallback_error}"
                            )
                        },
                    )?;
                    false
                }
                Err(err) => return Err(format!("SetOutputType H.264 failed: {err}")),
            };

            let input_type = create_video_type(
                width,
                height,
                fps,
                MFVideoFormat_NV12,
                bitrate_bps,
                Some(frame_size as u32),
            )?;
            apply_color_metadata(&input_type, color_spec);
            if let Err(err) =
                unsafe { input_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, width as i32 as u32) }
            {
                eprintln!("encoder media type could not set default stride: {err}");
            }
            unsafe { transform.SetInputType(0, &input_type, 0) }
                .map_err(|err| format!("SetInputType NV12 failed: {err}"))?;
            let input_color_metadata = unsafe { transform.GetInputCurrentType(0) }
                .ok()
                .map(|media_type| read_color_metadata(&media_type))
                .unwrap_or_default();
            let output_color_metadata = unsafe { transform.GetOutputCurrentType(0) }
                .ok()
                .map(|media_type| read_color_metadata(&media_type))
                .unwrap_or_default();
            let output_info = unsafe { transform.GetOutputStreamInfo(0) }
                .map_err(|err| format!("GetOutputStreamInfo failed: {err}"))?;
            let output_buffer_size =
                output_buffer_size(width, height, frame_size, bitrate_bps, &output_info)?;
            let output = output_path
                .as_deref()
                .map(|path| {
                    File::create(Path::new(path))
                        .map_err(|err| format!("create output failed: {err}"))
                })
                .transpose()?;

            unsafe {
                transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                    .map_err(|err| format!("begin streaming failed: {err}"))?;
                transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                    .map_err(|err| format!("start of stream failed: {err}"))?;
            }
            let event_generator =
                if selection.async_mft {
                    Some(transform.cast::<IMFMediaEventGenerator>().map_err(|err| {
                        format!("async encoder has no media event generator: {err}")
                    })?)
                } else {
                    None
                };

            Ok(Self {
                transform,
                output_info,
                output_buffer_size,
                output,
                collect_samples,
                pending_samples: VecDeque::new(),
                frame_size,
                fps,
                stats: EncoderStats::default(),
                finished: false,
                profile_main,
                selection,
                color_spec,
                input_color_metadata,
                output_color_metadata,
                event_generator,
                async_need_input: 0,
                async_drain_complete: false,
                async_cancellation: None,
                keyframe_control,
                _mf: mf,
                _com: com,
            })
        }

        pub fn encode_nv12(&mut self, data: &[u8], frame_index: u64) -> Result<(), String> {
            if self.finished {
                return Err("encoder has already been finished".to_string());
            }
            if data.len() != self.frame_size {
                return Err(format!(
                    "NV12 input length mismatch: expected {}, got {}",
                    self.frame_size,
                    data.len()
                ));
            }
            let sample_time = (frame_index as i64)
                .checked_mul(HNS_PER_SECOND)
                .ok_or_else(|| "sample time overflow".to_string())?
                / i64::from(self.fps);
            let sample_duration = HNS_PER_SECOND / i64::from(self.fps);
            let sample = create_input_sample(data, sample_time, sample_duration)?;
            if self.event_generator.is_some() {
                self.submit_async_input(&sample)?;
            } else {
                submit_input(
                    &self.transform,
                    &sample,
                    &self.output_info,
                    self.output_buffer_size,
                    &mut self.output,
                    self.collect_samples,
                    &mut self.pending_samples,
                    &mut self.stats,
                )?;
            }
            self.stats.frames_in += 1;
            if self.event_generator.is_some() {
                while self.pump_async_event()? {}
            } else {
                drain_available(
                    &self.transform,
                    &self.output_info,
                    self.output_buffer_size,
                    &mut self.output,
                    self.collect_samples,
                    &mut self.pending_samples,
                    &mut self.stats,
                )?;
            }
            Ok(())
        }

        #[allow(dead_code)]
        pub fn set_keyframe_interval_frames(&mut self, frames: u32) -> Result<(), String> {
            if frames == 0 {
                return Err("keyframe interval frames must be greater than zero".to_string());
            }
            let codec_api = self
                .transform
                .cast::<ICodecAPI>()
                .map_err(|err| format!("encoder does not expose ICodecAPI: {err}"))?;
            unsafe { codec_api.IsSupported(&CODECAPI_AVEncMPVGOPSize) }
                .map_err(|err| format!("encoder does not support GOP size control: {err}"))?;
            let value = VARIANT::from(frames);
            unsafe { codec_api.SetValue(&CODECAPI_AVEncMPVGOPSize, &value) }
                .map_err(|err| format!("set encoder GOP size to {frames} frames failed: {err}"))?;

            if unsafe { codec_api.IsSupported(&CODECAPI_AVEncVideoNumGOPsPerIDR) }.is_ok() {
                let one_gop_per_idr = VARIANT::from(1u32);
                unsafe { codec_api.SetValue(&CODECAPI_AVEncVideoNumGOPsPerIDR, &one_gop_per_idr) }
                    .map_err(|err| format!("set one GOP per IDR failed: {err}"))?;
            }
            Ok(())
        }

        pub fn keyframe_control(&self) -> &EncoderKeyframeControl {
            &self.keyframe_control
        }

        #[allow(dead_code)]
        pub fn supports_force_keyframe(&self) -> bool {
            self.keyframe_control.force_supported
        }

        pub fn request_keyframe(&mut self) -> Result<(), String> {
            let codec_api = self
                .transform
                .cast::<ICodecAPI>()
                .map_err(|err| format!("encoder does not expose ICodecAPI: {err}"))?;
            unsafe { codec_api.IsSupported(&CODECAPI_AVEncVideoForceKeyFrame) }
                .map_err(|err| format!("encoder does not support forced keyframes: {err}"))?;
            // CODECAPI_AVEncVideoForceKeyFrame is ULONG (VT_UI4), not VT_BOOL.
            let force = VARIANT::from(1u32);
            unsafe { codec_api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &force) }
                .map_err(|err| format!("force next encoder frame to IDR failed: {err}"))
        }

        pub fn set_mean_bitrate_mbps(&mut self, bitrate_mbps: f64) -> Result<(), String> {
            if !bitrate_mbps.is_finite() || !(0.1..=1000.0).contains(&bitrate_mbps) {
                return Err("encoder bitrate must be between 0.1 and 1000 Mbps".to_string());
            }
            let bitrate_bps = (bitrate_mbps * 1_000_000.0)
                .round()
                .clamp(1.0, f64::from(u32::MAX)) as u32;
            let codec_api = self
                .transform
                .cast::<ICodecAPI>()
                .map_err(|err| format!("encoder does not expose ICodecAPI: {err}"))?;
            unsafe { codec_api.IsSupported(&CODECAPI_AVEncCommonMeanBitRate) }.map_err(|err| {
                format!("encoder does not support runtime mean bitrate control: {err}")
            })?;
            unsafe { codec_api.IsModifiable(&CODECAPI_AVEncCommonMeanBitRate) }
                .ok()
                .map_err(|err| format!("encoder mean bitrate is not runtime-modifiable: {err}"))?;
            let value = VARIANT::from(bitrate_bps);
            unsafe { codec_api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &value) }.map_err(
                |err| format!("set encoder mean bitrate to {bitrate_mbps:.3} Mbps failed: {err}"),
            )?;
            let observed = unsafe { codec_api.GetValue(&CODECAPI_AVEncCommonMeanBitRate) }
                .map_err(|err| format!("read encoder mean bitrate after update failed: {err}"))?;
            let observed_bps = u32::try_from(&observed)
                .map_err(|err| format!("encoder mean bitrate readback was not UINT32: {err}"))?;
            if observed_bps != bitrate_bps {
                return Err(format!(
                    "encoder mean bitrate readback mismatch: requested {bitrate_bps}, observed {observed_bps}"
                ));
            }
            Ok(())
        }

        pub fn finish(&mut self) -> Result<EncoderStats, String> {
            if self.finished {
                return Ok(self.stats);
            }
            unsafe {
                self.transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
                    .map_err(|err| format!("end of stream failed: {err}"))?;
                self.transform
                    .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                    .map_err(|err| format!("drain command failed: {err}"))?;
            }
            if self.event_generator.is_some() {
                self.async_drain_complete = false;
                let drain_result = self.wait_for_async_state(
                    AsyncMftWaitKind::Drain,
                    ASYNC_DRAIN_TIMEOUT,
                    |encoder| encoder.async_drain_complete,
                );
                if let Err(error) = drain_result {
                    let cancelled = self
                        .async_cancellation
                        .as_ref()
                        .is_some_and(crate::shutdown::CancellationToken::is_cancelled);
                    if !cancelled {
                        return Err(error);
                    }
                    // Cancellation is a normal shutdown path. The bounded wait has
                    // already recorded it; skip any unavailable tail output and let
                    // END_STREAMING release the transform below.
                }
            } else {
                drain_available(
                    &self.transform,
                    &self.output_info,
                    self.output_buffer_size,
                    &mut self.output,
                    self.collect_samples,
                    &mut self.pending_samples,
                    &mut self.stats,
                )?;
            }
            let _ = unsafe {
                self.transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0)
            };
            if let Some(output) = self.output.as_mut() {
                output
                    .flush()
                    .map_err(|err| format!("flush output failed: {err}"))?;
            }
            self.finished = true;
            Ok(self.stats)
        }

        fn submit_async_input(&mut self, sample: &IMFSample) -> Result<(), String> {
            self.wait_for_async_state(
                AsyncMftWaitKind::NeedInput,
                ASYNC_NEED_INPUT_TIMEOUT,
                |encoder| encoder.async_need_input > 0,
            )?;
            unsafe { self.transform.ProcessInput(0, sample, 0) }
                .map_err(|err| format!("async ProcessInput failed: {err}"))?;
            self.async_need_input -= 1;
            Ok(())
        }

        fn wait_for_async_state(
            &mut self,
            kind: AsyncMftWaitKind,
            timeout: Duration,
            ready: fn(&Self) -> bool,
        ) -> Result<(), String> {
            let started = Instant::now();
            let cancellation = self.async_cancellation.clone();
            let result = poll_until(
                kind,
                timeout,
                || started.elapsed(),
                || {
                    cancellation
                        .as_ref()
                        .is_some_and(crate::shutdown::CancellationToken::is_cancelled)
                },
                || {
                    if ready(self) {
                        return Ok(Some(()));
                    }
                    let _ = self.pump_async_event()?;
                    Ok(ready(self).then_some(()))
                },
                || thread::sleep(ASYNC_EVENT_POLL_INTERVAL),
            );
            match result {
                Ok(()) => Ok(()),
                Err(AsyncMftPollError::Source(error)) => Err(error),
                Err(AsyncMftPollError::Wait(failure)) => {
                    match failure {
                        AsyncMftWaitFailure::Timeout { kind, .. } => {
                            self.stats.async_wait_timeouts =
                                self.stats.async_wait_timeouts.saturating_add(1);
                            if kind == AsyncMftWaitKind::Drain {
                                self.stats.async_drain_timeouts =
                                    self.stats.async_drain_timeouts.saturating_add(1);
                            }
                        }
                        AsyncMftWaitFailure::Cancelled { .. } => {
                            self.stats.async_wait_cancelled =
                                self.stats.async_wait_cancelled.saturating_add(1);
                        }
                    }
                    Err(failure.to_string())
                }
            }
        }

        fn pump_async_event(&mut self) -> Result<bool, String> {
            let generator = self
                .event_generator
                .as_ref()
                .ok_or_else(|| "async encoder event generator is unavailable".to_string())?
                .clone();
            let event = match unsafe { generator.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => event,
                Err(err) if err.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(false),
                Err(err) => return Err(format!("async encoder GetEvent failed: {err}")),
            };
            let status = unsafe { event.GetStatus() }
                .map_err(|err| format!("async encoder event status read failed: {err}"))?;
            if status.is_err() {
                return Err(format!("async encoder event reported failure: {status:?}"));
            }
            let event_type = unsafe { event.GetType() }
                .map_err(|err| format!("async encoder event type read failed: {err}"))?;
            if event_type == METransformNeedInput.0 as u32 {
                self.async_need_input = self.async_need_input.saturating_add(1);
            } else if event_type == METransformHaveOutput.0 as u32 {
                match process_one_output(
                    &self.transform,
                    &self.output_info,
                    self.output_buffer_size,
                    &mut self.output,
                    self.collect_samples,
                    &mut self.pending_samples,
                    &mut self.stats,
                )? {
                    OutputResult::Produced => {}
                    OutputResult::NeedMoreInput => {
                        return Err(
                            "async encoder signaled output but requested more input".to_string()
                        )
                    }
                    OutputResult::StreamChange => {}
                }
            } else if event_type == METransformDrainComplete.0 as u32 {
                self.async_drain_complete = true;
            } else if event_type == MEError.0 as u32 {
                return Err("async encoder emitted MEError".to_string());
            }
            Ok(true)
        }

        pub fn set_async_cancellation(&mut self, cancellation: crate::shutdown::CancellationToken) {
            self.async_cancellation = Some(cancellation);
        }

        pub fn take_encoded_samples(&mut self) -> Vec<EncodedSample> {
            self.pending_samples.drain(..).collect()
        }

        pub fn stats(&self) -> EncoderStats {
            self.stats
        }

        pub fn profile_main(&self) -> bool {
            self.profile_main
        }

        pub fn output_buffer_size(&self) -> u32 {
            self.output_buffer_size
        }

        pub fn encoder_selection(&self) -> &EncoderSelection {
            &self.selection
        }

        #[allow(dead_code)]
        pub fn color_spec(&self) -> ColorSpec {
            self.color_spec
        }

        pub fn input_color_metadata(&self) -> MediaColorMetadata {
            self.input_color_metadata
        }

        pub fn output_color_metadata(&self) -> MediaColorMetadata {
            self.output_color_metadata
        }
    }

    impl Drop for WmfH264Encoder {
        fn drop(&mut self) {
            if !self.finished {
                let _ = unsafe {
                    self.transform
                        .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0)
                };
                if let Some(output) = self.output.as_mut() {
                    let _ = output.flush();
                }
            }
        }
    }

    fn create_encoder(choice: EncoderChoice) -> Result<(IMFTransform, EncoderSelection), String> {
        match choice {
            EncoderChoice::Software | EncoderChoice::Microsoft => create_software_encoder(choice),
            EncoderChoice::Hardware => create_hardware_encoder(false, choice),
            EncoderChoice::IntelQsv => create_hardware_encoder(true, choice),
            EncoderChoice::Auto => unreachable!("auto is resolved before create_encoder"),
        }
    }

    fn create_software_encoder(
        requested: EncoderChoice,
    ) -> Result<(IMFTransform, EncoderSelection), String> {
        let transform = unsafe {
            CoCreateInstance::<_, IMFTransform>(
                &SOFTWARE_H264_ENCODER_CLSID,
                None::<&IUnknown>,
                CLSCTX_INPROC_SERVER,
            )
        }
        .map_err(|err| format!("create Microsoft H264 Encoder MFT failed: {err}"))?;
        Ok((
            transform,
            EncoderSelection {
                requested,
                selected_name: ENCODER_NAME.to_string(),
                clsid: guid_string(&SOFTWARE_H264_ENCODER_CLSID),
                kind: EncoderKind::Software,
                fallback: false,
                fallback_reason: None,
                async_mft: false,
                hardware_url: None,
                hardware_vendor: None,
                activation_entries_skipped: 0,
                activation_skip_missing_clsid: 0,
                activation_skip_other: 0,
            },
        ))
    }

    fn create_hardware_encoder(
        intel_only: bool,
        requested: EncoderChoice,
    ) -> Result<(IMFTransform, EncoderSelection), String> {
        let enumeration = enumerate_h264_encoder_activations()?;
        let diagnostics = enumeration.diagnostics;
        let mut hardware_candidates: Vec<EncoderCandidate> = enumeration
            .candidates
            .into_iter()
            .filter(|candidate| candidate.is_hardware_or_async_hint())
            .filter(|candidate| !intel_only || candidate.is_intel_qsv())
            .collect();
        hardware_candidates.sort_by_key(|candidate| if candidate.is_intel_qsv() { 0 } else { 1 });
        if hardware_candidates.is_empty() {
            return Err(if intel_only {
                "Intel Quick Sync Video H.264 Encoder MFT not found".to_string()
            } else {
                "no hardware H.264 encoder MFT with NV12 input and H.264 output was found"
                    .to_string()
            });
        }
        let mut activation_errors = Vec::new();
        for candidate in hardware_candidates {
            match activate_encoder_candidate(&candidate, requested) {
                Ok((transform, mut selection)) => {
                    diagnostics.apply_to(&mut selection);
                    return Ok((transform, selection));
                }
                Err(err) => activation_errors.push(format!("{}: {err}", candidate.name)),
            }
        }
        Err(format!(
            "hardware H.264 encoder activation failed: {}",
            activation_errors.join("; ")
        ))
    }

    fn activate_encoder_candidate(
        candidate: &EncoderCandidate,
        requested: EncoderChoice,
    ) -> Result<(IMFTransform, EncoderSelection), String> {
        let mut errors = Vec::new();
        match unsafe {
            CoCreateInstance::<_, IMFTransform>(
                &candidate.clsid,
                None::<&IUnknown>,
                CLSCTX_INPROC_SERVER,
            )
        } {
            Ok(transform) => {
                unlock_async_transform(&transform)?;
                return Ok((transform, candidate.selection(requested)));
            }
            Err(err) => errors.push(format!("CoCreateInstance failed: {err}")),
        }

        if candidate.async_mft {
            unsafe { candidate.activate.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) }
                .map_err(|err| format!("set activate MF_TRANSFORM_ASYNC_UNLOCK failed: {err}"))?;
        }
        match unsafe { candidate.activate.ActivateObject::<IMFTransform>() } {
            Ok(transform) => {
                unlock_async_transform(&transform)?;
                Ok((transform, candidate.selection(requested)))
            }
            Err(err) => {
                errors.push(format!("ActivateObject failed: {err}"));
                Err(errors.join("; "))
            }
        }
    }

    fn unlock_async_transform(transform: &IMFTransform) -> Result<(), String> {
        let attributes = unsafe { transform.GetAttributes() }
            .map_err(|err| format!("GetAttributes for async MFT failed: {err}"))?;
        unsafe { attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) }
            .map_err(|err| format!("set MF_TRANSFORM_ASYNC_UNLOCK failed: {err}"))
    }

    struct EncoderCandidate {
        activate: IMFActivate,
        name: String,
        clsid: GUID,
        hardware_url: Option<String>,
        hardware_vendor: Option<String>,
        async_mft: bool,
    }

    impl EncoderCandidate {
        fn is_hardware_or_async_hint(&self) -> bool {
            self.hardware_url
                .as_deref()
                .is_some_and(|value| !value.is_empty())
                || self
                    .hardware_vendor
                    .as_deref()
                    .is_some_and(|value| !value.is_empty())
                || self.async_mft
        }

        fn is_intel_qsv(&self) -> bool {
            let lower = self.name.to_ascii_lowercase();
            lower.contains("intel")
                || lower.contains("quick sync")
                || lower.contains("qsv")
                || self
                    .hardware_vendor
                    .as_deref()
                    .is_some_and(|value| value.to_ascii_lowercase().contains("intel"))
        }

        fn selection(&self, requested: EncoderChoice) -> EncoderSelection {
            EncoderSelection {
                requested,
                selected_name: self.name.clone(),
                clsid: guid_string(&self.clsid),
                kind: EncoderKind::Hardware,
                fallback: false,
                fallback_reason: None,
                async_mft: self.async_mft,
                hardware_url: self.hardware_url.clone(),
                hardware_vendor: self.hardware_vendor.clone(),
                activation_entries_skipped: 0,
                activation_skip_missing_clsid: 0,
                activation_skip_other: 0,
            }
        }
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct EncoderActivationDiagnostics {
        entries_skipped: u64,
        missing_clsid: u64,
        other: u64,
    }

    impl EncoderActivationDiagnostics {
        fn observe_skip(&mut self, error: &str) {
            self.entries_skipped = self.entries_skipped.saturating_add(1);
            if error.starts_with("missing transform CLSID") {
                self.missing_clsid = self.missing_clsid.saturating_add(1);
            } else {
                self.other = self.other.saturating_add(1);
            }
        }

        fn apply_to(self, selection: &mut EncoderSelection) {
            selection.activation_entries_skipped = self.entries_skipped;
            selection.activation_skip_missing_clsid = self.missing_clsid;
            selection.activation_skip_other = self.other;
        }
    }

    struct EncoderEnumeration {
        candidates: Vec<EncoderCandidate>,
        diagnostics: EncoderActivationDiagnostics,
    }

    fn enumerate_h264_encoder_activations() -> Result<EncoderEnumeration, String> {
        let input_type = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_NV12,
        };
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
                Some(&input_type),
                Some(&output_type),
                &mut activates,
                &mut count,
            )
        }
        .map_err(|err| format!("MFTEnumEx H.264 encoder failed: {err}"))?;

        let mut candidates = Vec::new();
        let mut diagnostics = EncoderActivationDiagnostics::default();
        if !activates.is_null() {
            for index in 0..count as usize {
                let activate = unsafe { ptr::read(activates.add(index)) };
                let Some(activate) = activate else {
                    continue;
                };
                match inspect_activation(activate) {
                    Ok(candidate) => {
                        if !candidates
                            .iter()
                            .any(|existing: &EncoderCandidate| existing.clsid == candidate.clsid)
                        {
                            candidates.push(candidate);
                        }
                    }
                    Err(err) => diagnostics.observe_skip(&err),
                }
            }
            unsafe { CoTaskMemFree(Some(activates.cast::<c_void>())) };
        }
        Ok(EncoderEnumeration {
            candidates,
            diagnostics,
        })
    }

    fn inspect_activation(activate: IMFActivate) -> Result<EncoderCandidate, String> {
        let clsid = unsafe { activate.GetGUID(&MFT_TRANSFORM_CLSID_Attribute) }
            .map_err(|err| format!("missing transform CLSID: {err}"))?;
        let name = get_allocated_string(&activate, &MFT_FRIENDLY_NAME_Attribute)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("H.264 encoder {}", guid_string(&clsid)));
        let hardware_url = get_allocated_string(&activate, &MFT_ENUM_HARDWARE_URL_Attribute)
            .filter(|value| !value.is_empty());
        let hardware_vendor =
            get_allocated_string(&activate, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute)
                .filter(|value| !value.is_empty());
        let async_mft = unsafe { activate.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) != 0;
        Ok(EncoderCandidate {
            activate,
            name,
            clsid,
            hardware_url,
            hardware_vendor,
            async_mft,
        })
    }

    fn create_video_type(
        width: u32,
        height: u32,
        fps: u32,
        subtype: GUID,
        bitrate_bps: u32,
        sample_size: Option<u32>,
    ) -> Result<IMFMediaType, String> {
        let media_type =
            unsafe { MFCreateMediaType() }.map_err(|err| format!("MFCreateMediaType: {err}"))?;
        let configure_result = (|| -> WindowsResult<()> {
            unsafe {
                media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                media_type.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
                media_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_ratio(width, height))?;
                media_type.SetUINT64(&MF_MT_FRAME_RATE, pack_ratio(fps, 1))?;
                media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_ratio(1, 1))?;
                media_type
                    .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                media_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_bps)?;
                if let Some(size) = sample_size {
                    media_type.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1)?;
                    media_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
                    media_type.SetUINT32(&MF_MT_SAMPLE_SIZE, size)?;
                }
            }
            Ok(())
        })();
        configure_result.map_err(|err| format!("configure media type failed: {err}"))?;
        Ok(media_type)
    }

    fn apply_color_metadata(media_type: &IMFMediaType, color: ColorSpec) {
        let primaries = match color.matrix {
            ColorMatrix::Bt601 => MFVideoPrimaries_SMPTE170M.0 as u32,
            ColorMatrix::Bt709 => MFVideoPrimaries_BT709.0 as u32,
        };
        let matrix = match color.matrix {
            ColorMatrix::Bt601 => MFVideoTransferMatrix_BT601.0 as u32,
            ColorMatrix::Bt709 => MFVideoTransferMatrix_BT709.0 as u32,
        };
        for (attribute, value, name) in [
            (&MF_MT_VIDEO_PRIMARIES, primaries, "video primaries"),
            (
                &MF_MT_TRANSFER_FUNCTION,
                MFVideoTransFunc_709.0 as u32,
                "transfer function",
            ),
            (&MF_MT_YUV_MATRIX, matrix, "YUV matrix"),
            (
                &MF_MT_VIDEO_NOMINAL_RANGE,
                MFNominalRange_16_235.0 as u32,
                "nominal range",
            ),
        ] {
            if let Err(err) = unsafe { media_type.SetUINT32(attribute, value) } {
                eprintln!("encoder media type could not set {name}: {err}");
            }
        }
    }

    fn configure_keyframe_control(
        transform: &IMFTransform,
        keyframe_interval_frames: Option<u32>,
    ) -> EncoderKeyframeControl {
        let method = "codecapi-static-gop+force-next-frame-ui4".to_string();
        let codec_api = match transform.cast::<ICodecAPI>() {
            Ok(codec_api) => codec_api,
            Err(err) => {
                return EncoderKeyframeControl {
                    config_method: method,
                    config_applied: false,
                    config_error: Some(format!("encoder does not expose ICodecAPI: {err}")),
                    force_supported: false,
                };
            }
        };
        let force_supported = unsafe {
            codec_api
                .IsSupported(&CODECAPI_AVEncVideoForceKeyFrame)
                .is_ok()
        };
        let Some(frames) = keyframe_interval_frames else {
            return EncoderKeyframeControl {
                config_method: method,
                config_applied: false,
                config_error: None,
                force_supported,
            };
        };
        if frames == 0 {
            return EncoderKeyframeControl {
                config_method: method,
                config_applied: false,
                config_error: Some(
                    "keyframe interval frames must be greater than zero".to_string(),
                ),
                force_supported,
            };
        }

        let result = (|| {
            unsafe { codec_api.IsSupported(&CODECAPI_AVEncMPVGOPSize) }
                .map_err(|err| format!("encoder does not support GOP size control: {err}"))?;
            unsafe { codec_api.SetValue(&CODECAPI_AVEncMPVGOPSize, &VARIANT::from(frames)) }
                .map_err(|err| format!("set encoder GOP size to {frames} frames failed: {err}"))?;
            if unsafe { codec_api.IsSupported(&CODECAPI_AVEncVideoNumGOPsPerIDR) }.is_ok() {
                unsafe {
                    codec_api.SetValue(&CODECAPI_AVEncVideoNumGOPsPerIDR, &VARIANT::from(1u32))
                }
                .map_err(|err| format!("set one GOP per IDR failed: {err}"))?;
            }
            Ok::<(), String>(())
        })();
        EncoderKeyframeControl {
            config_method: method,
            config_applied: result.is_ok(),
            config_error: result.err(),
            force_supported,
        }
    }

    fn read_color_metadata(media_type: &IMFMediaType) -> MediaColorMetadata {
        MediaColorMetadata {
            primaries: unsafe { media_type.GetUINT32(&MF_MT_VIDEO_PRIMARIES) }.ok(),
            transfer: unsafe { media_type.GetUINT32(&MF_MT_TRANSFER_FUNCTION) }.ok(),
            matrix: unsafe { media_type.GetUINT32(&MF_MT_YUV_MATRIX) }.ok(),
            nominal_range: unsafe { media_type.GetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE) }.ok(),
            default_stride: unsafe { media_type.GetUINT32(&MF_MT_DEFAULT_STRIDE) }
                .ok()
                .map(|value| value as i32),
        }
    }

    fn create_input_sample(
        data: &[u8],
        sample_time: i64,
        sample_duration: i64,
    ) -> Result<IMFSample, String> {
        let buffer = unsafe { MFCreateMemoryBuffer(data.len() as u32) }
            .map_err(|err| format!("MFCreateMemoryBuffer input failed: {err}"))?;
        write_buffer(&buffer, data)?;
        let sample =
            unsafe { MFCreateSample() }.map_err(|err| format!("MFCreateSample input: {err}"))?;
        unsafe {
            sample
                .AddBuffer(&buffer)
                .map_err(|err| format!("input AddBuffer failed: {err}"))?;
            sample
                .SetSampleTime(sample_time)
                .map_err(|err| format!("SetSampleTime failed: {err}"))?;
            sample
                .SetSampleDuration(sample_duration)
                .map_err(|err| format!("SetSampleDuration failed: {err}"))?;
        }
        Ok(sample)
    }

    fn write_buffer(buffer: &IMFMediaBuffer, data: &[u8]) -> Result<(), String> {
        let mut pointer = ptr::null_mut();
        let mut max_length = 0u32;
        unsafe { buffer.Lock(&mut pointer, Some(&mut max_length), None) }
            .map_err(|err| format!("input buffer Lock failed: {err}"))?;
        let result = if pointer.is_null() || max_length < data.len() as u32 {
            Err("input buffer is smaller than NV12 frame".to_string())
        } else {
            unsafe { ptr::copy_nonoverlapping(data.as_ptr(), pointer, data.len()) };
            Ok(())
        };
        let unlock_result = unsafe { buffer.Unlock() };
        result?;
        unlock_result.map_err(|err| format!("input buffer Unlock failed: {err}"))?;
        unsafe { buffer.SetCurrentLength(data.len() as u32) }
            .map_err(|err| format!("input SetCurrentLength failed: {err}"))
    }

    fn submit_input(
        transform: &IMFTransform,
        sample: &IMFSample,
        output_info: &MFT_OUTPUT_STREAM_INFO,
        output_buffer_size: u32,
        output: &mut Option<File>,
        collect_samples: bool,
        pending_samples: &mut VecDeque<EncodedSample>,
        stats: &mut EncoderStats,
    ) -> Result<(), String> {
        loop {
            match unsafe { transform.ProcessInput(0, sample, 0) } {
                Ok(()) => return Ok(()),
                Err(err) if err.code() == MF_E_NOTACCEPTING => {
                    if !drain_available(
                        transform,
                        output_info,
                        output_buffer_size,
                        output,
                        collect_samples,
                        pending_samples,
                        stats,
                    )? {
                        return Err(
                            "encoder rejected input but produced no pending output".to_string()
                        );
                    }
                }
                Err(err) => return Err(format!("ProcessInput failed: {err}")),
            }
        }
    }

    fn drain_available(
        transform: &IMFTransform,
        output_info: &MFT_OUTPUT_STREAM_INFO,
        output_buffer_size: u32,
        output: &mut Option<File>,
        collect_samples: bool,
        pending_samples: &mut VecDeque<EncodedSample>,
        stats: &mut EncoderStats,
    ) -> Result<bool, String> {
        let mut produced_any = false;
        loop {
            match process_one_output(
                transform,
                output_info,
                output_buffer_size,
                output,
                collect_samples,
                pending_samples,
                stats,
            )? {
                OutputResult::Produced => produced_any = true,
                OutputResult::NeedMoreInput => return Ok(produced_any),
                OutputResult::StreamChange => continue,
            }
        }
    }

    fn process_one_output(
        transform: &IMFTransform,
        output_info: &MFT_OUTPUT_STREAM_INFO,
        output_buffer_size: u32,
        output: &mut Option<File>,
        collect_samples: bool,
        pending_samples: &mut VecDeque<EncodedSample>,
        stats: &mut EncoderStats,
    ) -> Result<OutputResult, String> {
        let transform_provides_sample = output_info.dwFlags
            & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32
                | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32)
            != 0;
        let supplied_sample = if transform_provides_sample {
            None
        } else {
            Some(create_output_sample(output_buffer_size)?)
        };
        let mut output_data = MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(supplied_sample),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        };
        let mut status = 0u32;
        let process_result =
            unsafe { transform.ProcessOutput(0, slice::from_mut(&mut output_data), &mut status) };
        let produced_sample = (*output_data.pSample).clone();
        unsafe {
            ManuallyDrop::drop(&mut output_data.pSample);
            ManuallyDrop::drop(&mut output_data.pEvents);
        }

        match process_result {
            Ok(()) => {
                let sample = produced_sample
                    .ok_or_else(|| "ProcessOutput returned no output sample".to_string())?;
                let bytes = read_sample_bytes(&sample)?;
                if bytes.is_empty() {
                    return Err("encoder produced an empty output sample".to_string());
                }
                if let Some(output) = output.as_mut() {
                    output
                        .write_all(&bytes)
                        .map_err(|err| format!("write H.264 output failed: {err}"))?;
                }
                stats.samples_out += 1;
                stats.bytes_out += bytes.len() as u64;
                let keyframe = detect_keyframe(&sample, &bytes);
                if let Some(keyframe) = keyframe {
                    stats.keyframe_detection_available = true;
                    if keyframe {
                        stats.keyframes += 1;
                    }
                }
                if collect_samples {
                    pending_samples.push_back(EncodedSample {
                        bytes,
                        keyframe,
                        sample_time_hns: unsafe { sample.GetSampleTime() }.ok(),
                    });
                }
                Ok(OutputResult::Produced)
            }
            Err(err) if err.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                Ok(OutputResult::NeedMoreInput)
            }
            Err(err) if err.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                let output_type =
                    unsafe { transform.GetOutputAvailableType(0, 0) }.map_err(|type_err| {
                        format!("output stream changed but no type is available: {type_err}")
                    })?;
                unsafe { transform.SetOutputType(0, &output_type, 0) }
                    .map_err(|type_err| format!("renegotiate output type failed: {type_err}"))?;
                Ok(OutputResult::StreamChange)
            }
            Err(err) => Err(format!("ProcessOutput failed: {err}")),
        }
    }

    fn create_output_sample(buffer_size: u32) -> Result<IMFSample, String> {
        let buffer = unsafe { MFCreateMemoryBuffer(buffer_size) }
            .map_err(|err| format!("MFCreateMemoryBuffer output failed: {err}"))?;
        let sample =
            unsafe { MFCreateSample() }.map_err(|err| format!("MFCreateSample output: {err}"))?;
        unsafe { sample.AddBuffer(&buffer) }
            .map_err(|err| format!("output AddBuffer failed: {err}"))?;
        Ok(sample)
    }

    fn read_sample_bytes(sample: &IMFSample) -> Result<Vec<u8>, String> {
        let buffer = unsafe { sample.ConvertToContiguousBuffer() }
            .map_err(|err| format!("ConvertToContiguousBuffer failed: {err}"))?;
        let length = unsafe { buffer.GetCurrentLength() }
            .map_err(|err| format!("output GetCurrentLength failed: {err}"))?;
        let mut pointer = ptr::null_mut();
        unsafe { buffer.Lock(&mut pointer, None, None) }
            .map_err(|err| format!("output buffer Lock failed: {err}"))?;
        let bytes = if pointer.is_null() {
            Vec::new()
        } else {
            unsafe { slice::from_raw_parts(pointer, length as usize) }.to_vec()
        };
        unsafe { buffer.Unlock() }.map_err(|err| format!("output buffer Unlock failed: {err}"))?;
        Ok(bytes)
    }

    fn detect_keyframe(sample: &IMFSample, bytes: &[u8]) -> Option<bool> {
        if let Ok(value) = unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) } {
            return Some(value != 0);
        }
        annex_b_contains_idr(bytes)
    }

    fn annex_b_contains_idr(bytes: &[u8]) -> Option<bool> {
        let mut found_start_code = false;
        let mut index = 0usize;
        while index + 3 < bytes.len() {
            let nal_start = if bytes[index..].starts_with(&[0, 0, 0, 1]) {
                Some(index + 4)
            } else if bytes[index..].starts_with(&[0, 0, 1]) {
                Some(index + 3)
            } else {
                None
            };
            if let Some(nal_start) = nal_start {
                found_start_code = true;
                if nal_start < bytes.len() && bytes[nal_start] & 0x1f == 5 {
                    return Some(true);
                }
                index = nal_start;
            } else {
                index += 1;
            }
        }
        found_start_code.then_some(false)
    }

    fn output_buffer_size(
        width: u32,
        height: u32,
        frame_size: usize,
        bitrate_bps: u32,
        info: &MFT_OUTPUT_STREAM_INFO,
    ) -> Result<u32, String> {
        let per_second = (bitrate_bps / 8).max(MIN_OUTPUT_BUFFER_SIZE);
        let raw_frame = u32::try_from(frame_size)
            .map_err(|_| "NV12 frame is too large for Media Foundation buffer".to_string())?;
        let dimension_hint = width
            .checked_mul(height)
            .ok_or_else(|| "output buffer dimension overflow".to_string())?;
        Ok(info
            .cbSize
            .max(MIN_OUTPUT_BUFFER_SIZE)
            .max(per_second)
            .max(raw_frame)
            .max(dimension_hint))
    }

    fn bitrate_to_bps(mbps: f64) -> Result<u32, String> {
        if !mbps.is_finite() || mbps <= 0.0 {
            return Err("bitrate-mbps must be greater than zero".to_string());
        }
        let bitrate = (mbps * 1_000_000.0).round();
        if bitrate > u32::MAX as f64 {
            Err("bitrate exceeds Media Foundation UINT32 range".to_string())
        } else {
            Ok(bitrate as u32)
        }
    }

    fn pack_ratio(numerator: u32, denominator: u32) -> u64 {
        (u64::from(numerator) << 32) | u64::from(denominator)
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

    fn guid_string(guid: &GUID) -> String {
        format!("{{{guid:?}}}")
    }

    fn optional_json_string(value: Option<&str>) -> String {
        value.map_or_else(
            || "null".to_string(),
            |value| format!(r#""{}""#, json_escape(value)),
        )
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
    EncodedSample, EncoderChoice, EncoderKeyframeControl, EncoderSelection, EncoderStats,
    WmfH264Encoder, ENCODER_NAME,
};
