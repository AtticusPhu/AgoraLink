#[cfg(windows)]
mod platform {
    use std::fs::File;
    use std::io::Write;
    use std::mem::ManuallyDrop;
    use std::path::Path;
    use std::ptr;
    use std::slice;

    use windows::core::{IUnknown, Result as WindowsResult, GUID};
    use windows::Win32::Media::MediaFoundation::{
        eAVEncH264VProfile_Main, IMFMediaBuffer, IMFMediaType, IMFSample, IMFTransform,
        MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video,
        MFSampleExtension_CleanPoint, MFShutdown, MFStartup, MFVideoFormat_H264,
        MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFSTARTUP_FULL,
        MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
        MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_END_STREAMING,
        MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
        MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_INFO,
        MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT,
        MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AVG_BITRATE, MF_MT_FIXED_SIZE_SAMPLES,
        MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
        MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SAMPLE_SIZE, MF_MT_SUBTYPE,
        MF_VERSION,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED,
    };

    pub const ENCODER_NAME: &str = "Microsoft H264 Encoder MFT";
    const SOFTWARE_H264_ENCODER_CLSID: GUID =
        GUID::from_u128(0x6ca50344_051a_4ded_9779_a43305165e35);
    const HNS_PER_SECOND: i64 = 10_000_000;
    const MIN_OUTPUT_BUFFER_SIZE: u32 = 1_048_576;

    #[derive(Clone, Copy, Debug, Default)]
    pub struct EncoderStats {
        pub frames_in: u64,
        pub samples_out: u64,
        pub bytes_out: u64,
        pub keyframes: u64,
        pub keyframe_detection_available: bool,
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
    }

    pub struct WmfH264Encoder {
        transform: IMFTransform,
        output_info: MFT_OUTPUT_STREAM_INFO,
        output_buffer_size: u32,
        output: File,
        frame_size: usize,
        fps: u32,
        stats: EncoderStats,
        finished: bool,
        profile_main: bool,
        _mf: MediaFoundationGuard,
        _com: ComGuard,
    }

    impl WmfH264Encoder {
        pub fn new(
            width: u32,
            height: u32,
            fps: u32,
            bitrate_mbps: f64,
            output_path: &str,
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
            let transform = create_software_encoder()?;

            let output_type =
                create_video_type(width, height, fps, MFVideoFormat_H264, bitrate_bps, None)?;
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
            unsafe { transform.SetInputType(0, &input_type, 0) }
                .map_err(|err| format!("SetInputType NV12 failed: {err}"))?;
            let output_info = unsafe { transform.GetOutputStreamInfo(0) }
                .map_err(|err| format!("GetOutputStreamInfo failed: {err}"))?;
            let output_buffer_size =
                output_buffer_size(width, height, frame_size, bitrate_bps, &output_info)?;
            let output = File::create(Path::new(output_path))
                .map_err(|err| format!("create output failed: {err}"))?;

            unsafe {
                transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                    .map_err(|err| format!("begin streaming failed: {err}"))?;
                transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                    .map_err(|err| format!("start of stream failed: {err}"))?;
            }

            Ok(Self {
                transform,
                output_info,
                output_buffer_size,
                output,
                frame_size,
                fps,
                stats: EncoderStats::default(),
                finished: false,
                profile_main,
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
            submit_input(
                &self.transform,
                &sample,
                &self.output_info,
                self.output_buffer_size,
                &mut self.output,
                &mut self.stats,
            )?;
            self.stats.frames_in += 1;
            drain_available(
                &self.transform,
                &self.output_info,
                self.output_buffer_size,
                &mut self.output,
                &mut self.stats,
            )?;
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
            drain_available(
                &self.transform,
                &self.output_info,
                self.output_buffer_size,
                &mut self.output,
                &mut self.stats,
            )?;
            let _ = unsafe {
                self.transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0)
            };
            self.output
                .flush()
                .map_err(|err| format!("flush output failed: {err}"))?;
            self.finished = true;
            Ok(self.stats)
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
    }

    impl Drop for WmfH264Encoder {
        fn drop(&mut self) {
            if !self.finished {
                let _ = unsafe {
                    self.transform
                        .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0)
                };
                let _ = self.output.flush();
            }
        }
    }

    fn create_software_encoder() -> Result<IMFTransform, String> {
        unsafe {
            CoCreateInstance::<_, IMFTransform>(
                &SOFTWARE_H264_ENCODER_CLSID,
                None::<&IUnknown>,
                CLSCTX_INPROC_SERVER,
            )
        }
        .map_err(|err| format!("create Microsoft H264 Encoder MFT failed: {err}"))
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
        output: &mut File,
        stats: &mut EncoderStats,
    ) -> Result<(), String> {
        loop {
            match unsafe { transform.ProcessInput(0, sample, 0) } {
                Ok(()) => return Ok(()),
                Err(err) if err.code() == MF_E_NOTACCEPTING => {
                    if !drain_available(transform, output_info, output_buffer_size, output, stats)?
                    {
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
        output: &mut File,
        stats: &mut EncoderStats,
    ) -> Result<bool, String> {
        let mut produced_any = false;
        loop {
            match process_one_output(transform, output_info, output_buffer_size, output, stats)? {
                OutputResult::Produced => produced_any = true,
                OutputResult::NeedMoreInput => return Ok(produced_any),
            }
        }
    }

    fn process_one_output(
        transform: &IMFTransform,
        output_info: &MFT_OUTPUT_STREAM_INFO,
        output_buffer_size: u32,
        output: &mut File,
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
                output
                    .write_all(&bytes)
                    .map_err(|err| format!("write H.264 output failed: {err}"))?;
                stats.samples_out += 1;
                stats.bytes_out += bytes.len() as u64;
                if let Some(keyframe) = detect_keyframe(&sample, &bytes) {
                    stats.keyframe_detection_available = true;
                    if keyframe {
                        stats.keyframes += 1;
                    }
                }
                Ok(OutputResult::Produced)
            }
            Err(err) if err.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                Ok(OutputResult::NeedMoreInput)
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
}

#[cfg(windows)]
pub use platform::{EncoderStats, WmfH264Encoder, ENCODER_NAME};
