#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncoderChoice {
    Auto,
    Software,
    Hardware,
}

#[derive(Debug)]
pub struct EncodeProbeConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub duration_sec: u64,
    pub bitrate_mbps: f64,
    pub output: String,
    pub encoder: EncoderChoice,
}

#[cfg(windows)]
mod platform {
    use std::fs::File;
    use std::io::{self, Write};
    use std::mem::ManuallyDrop;
    use std::path::Path;
    use std::ptr;
    use std::slice;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

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
    use windows::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
    };

    use super::{EncodeProbeConfig, EncoderChoice};
    use crate::nv12_synthetic;

    const SOFTWARE_H264_ENCODER_CLSID: GUID =
        GUID::from_u128(0x6ca50344_051a_4ded_9779_a43305165e35);
    const SOFTWARE_ENCODER_NAME: &str = "Microsoft H264 Encoder MFT";
    const HNS_PER_SECOND: i64 = 10_000_000;
    const MIN_OUTPUT_BUFFER_SIZE: u32 = 1_048_576;

    static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

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

    unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> windows::core::BOOL {
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

    #[derive(Debug, Default)]
    struct EncodeStats {
        frames_in: u64,
        samples_out: u64,
        bytes_out: u64,
        keyframes: u64,
        keyframe_detection_available: bool,
    }

    enum OutputResult {
        Produced,
        NeedMoreInput,
    }

    pub fn run(config: EncodeProbeConfig) -> Result<(), String> {
        validate_config(&config)?;
        if config.encoder == EncoderChoice::Hardware {
            return Err(
                "hardware encoder is not implemented in stage 3A; use --encoder software or auto"
                    .to_string(),
            );
        }

        let _com = ComGuard::initialize()?;
        let _mf = MediaFoundationGuard::startup()?;
        let _console_ctrl = ConsoleCtrlGuard::install()?;
        let transform = create_software_encoder()?;
        let frame_size = nv12_synthetic::buffer_size(config.width, config.height)?;
        let bitrate_bps = bitrate_to_bps(config.bitrate_mbps)?;

        let output_type = create_video_type(&config, MFVideoFormat_H264, bitrate_bps, None)?;
        let profile_attribute_set = unsafe {
            output_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)
        }
        .is_ok();
        let profile_set = match unsafe { transform.SetOutputType(0, &output_type, 0) } {
            Ok(()) => profile_attribute_set,
            Err(profile_error) if profile_attribute_set => {
                let fallback = create_video_type(&config, MFVideoFormat_H264, bitrate_bps, None)?;
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
        if !profile_set {
            eprintln!("H.264 Main profile was unavailable; using encoder default profile");
        }

        let input_type = create_video_type(
            &config,
            MFVideoFormat_NV12,
            bitrate_bps,
            Some(frame_size as u32),
        )?;
        unsafe { transform.SetInputType(0, &input_type, 0) }
            .map_err(|err| format!("SetInputType NV12 failed: {err}"))?;

        let output_info = unsafe { transform.GetOutputStreamInfo(0) }
            .map_err(|err| format!("GetOutputStreamInfo failed: {err}"))?;
        let output_buffer_size =
            output_buffer_size(&config, frame_size, bitrate_bps, &output_info)?;
        let output_path = Path::new(&config.output);
        let mut output =
            File::create(output_path).map_err(|err| format!("create output failed: {err}"))?;

        eprintln!(
            "encode-probe encoder=\"{}\" input=NV12 output=H264 size={}x{} fps={} bitrate_mbps={} output_buffer={} profile_main={}",
            SOFTWARE_ENCODER_NAME,
            config.width,
            config.height,
            config.fps,
            config.bitrate_mbps,
            output_buffer_size,
            profile_set
        );

        unsafe {
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|err| format!("begin streaming failed: {err}"))?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|err| format!("start of stream failed: {err}"))?;
        }

        let total_frames = u64::from(config.fps)
            .checked_mul(config.duration_sec)
            .ok_or_else(|| "frame count overflow".to_string())?;
        let sample_duration = HNS_PER_SECOND / i64::from(config.fps);
        let frame_interval = Duration::from_nanos(1_000_000_000 / u64::from(config.fps));
        let mut nv12 = vec![0u8; frame_size];
        let mut stats = EncodeStats::default();
        let started_at = Instant::now();
        let mut next_frame_at = started_at;
        let mut report_at = started_at;
        let mut previous_frames = 0u64;
        let mut previous_bytes = 0u64;

        for frame_index in 0..total_frames {
            if STOP_REQUESTED.load(Ordering::SeqCst) {
                break;
            }
            sleep_until(next_frame_at);
            nv12_synthetic::fill_frame(&mut nv12, config.width, config.height, frame_index)?;
            let sample_time = (frame_index as i64)
                .checked_mul(HNS_PER_SECOND)
                .ok_or_else(|| "sample time overflow".to_string())?
                / i64::from(config.fps);
            let sample = create_input_sample(&nv12, sample_time, sample_duration)?;

            submit_input(
                &transform,
                &sample,
                &output_info,
                output_buffer_size,
                &mut output,
                &mut stats,
            )?;
            stats.frames_in += 1;
            drain_available(
                &transform,
                &output_info,
                output_buffer_size,
                &mut output,
                &mut stats,
            )?;

            let now = Instant::now();
            if now.duration_since(report_at) >= Duration::from_secs(1) {
                print_stats(
                    &config,
                    &stats,
                    now.duration_since(report_at),
                    previous_frames,
                    previous_bytes,
                );
                previous_frames = stats.frames_in;
                previous_bytes = stats.bytes_out;
                report_at = now;
            }
            next_frame_at += frame_interval;
        }

        unsafe {
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
                .map_err(|err| format!("end of stream failed: {err}"))?;
            transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                .map_err(|err| format!("drain command failed: {err}"))?;
        }
        drain_available(
            &transform,
            &output_info,
            output_buffer_size,
            &mut output,
            &mut stats,
        )?;
        let _ = unsafe { transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0) };
        output
            .flush()
            .map_err(|err| format!("flush output failed: {err}"))?;

        let wall_time_sec = started_at.elapsed().as_secs_f64();
        let media_duration_sec = stats.frames_in as f64 / f64::from(config.fps);
        println!(
            r#"{{"type":"ENCODE_DONE","encoder":"{}","frames_in":{},"samples_out":{},"bytes_out":{},"duration_sec":{:.3},"wall_time_sec":{:.3},"fps":{:.2},"processing_fps":{:.2},"mbps":{:.3},"keyframes":{},"width":{},"height":{},"output":"{}"}}"#,
            SOFTWARE_ENCODER_NAME,
            stats.frames_in,
            stats.samples_out,
            stats.bytes_out,
            media_duration_sec,
            wall_time_sec,
            config.fps,
            stats.frames_in as f64 / wall_time_sec.max(0.001),
            stats.bytes_out as f64 * 8.0 / media_duration_sec.max(0.001) / 1_000_000.0,
            keyframes_json(&stats),
            config.width,
            config.height,
            json_escape(&config.output)
        );
        io::stdout().flush().ok();
        eprintln!(
            "encode-probe stopped reason={}",
            if STOP_REQUESTED.load(Ordering::SeqCst) {
                "console-control"
            } else {
                "duration-complete"
            }
        );
        Ok(())
    }

    fn validate_config(config: &EncodeProbeConfig) -> Result<(), String> {
        if config.width == 0
            || config.height == 0
            || config.width % 2 != 0
            || config.height % 2 != 0
        {
            return Err("width and height must be non-zero even values".to_string());
        }
        if config.fps == 0 || config.duration_sec == 0 {
            return Err("fps and duration-sec must be greater than zero".to_string());
        }
        if !config.bitrate_mbps.is_finite() || config.bitrate_mbps <= 0.0 {
            return Err("bitrate-mbps must be greater than zero".to_string());
        }
        if config.output.trim().is_empty() {
            return Err("output path must not be empty".to_string());
        }
        Ok(())
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
        config: &EncodeProbeConfig,
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
                media_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_ratio(config.width, config.height))?;
                media_type.SetUINT64(&MF_MT_FRAME_RATE, pack_ratio(config.fps, 1))?;
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
        stats: &mut EncodeStats,
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
        stats: &mut EncodeStats,
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
        stats: &mut EncodeStats,
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
                let sample = produced_sample.ok_or_else(|| {
                    "ProcessOutput succeeded without an output sample".to_string()
                })?;
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
        config: &EncodeProbeConfig,
        frame_size: usize,
        bitrate_bps: u32,
        info: &MFT_OUTPUT_STREAM_INFO,
    ) -> Result<u32, String> {
        let per_second = (bitrate_bps / 8).max(MIN_OUTPUT_BUFFER_SIZE);
        let raw_frame = u32::try_from(frame_size)
            .map_err(|_| "NV12 frame is too large for Media Foundation buffer".to_string())?;
        let dimension_hint = config
            .width
            .checked_mul(config.height)
            .ok_or_else(|| "output buffer dimension overflow".to_string())?;
        Ok(info
            .cbSize
            .max(MIN_OUTPUT_BUFFER_SIZE)
            .max(per_second)
            .max(raw_frame)
            .max(dimension_hint))
    }

    fn bitrate_to_bps(mbps: f64) -> Result<u32, String> {
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

    fn sleep_until(target: Instant) {
        let now = Instant::now();
        if target > now {
            thread::sleep(target.duration_since(now));
        }
    }

    fn print_stats(
        config: &EncodeProbeConfig,
        stats: &EncodeStats,
        elapsed: Duration,
        previous_frames: u64,
        previous_bytes: u64,
    ) {
        let elapsed_sec = elapsed.as_secs_f64().max(0.001);
        let frames_delta = stats.frames_in.saturating_sub(previous_frames);
        let processing_fps = frames_delta as f64 / elapsed_sec;
        let media_elapsed_sec = frames_delta as f64 / f64::from(config.fps);
        let mbps = stats.bytes_out.saturating_sub(previous_bytes) as f64 * 8.0
            / media_elapsed_sec.max(0.001)
            / 1_000_000.0;
        println!(
            r#"{{"type":"ENCODE_STATS","mode":"encode_probe","encoder":"{}","frames_in":{},"samples_out":{},"bytes_out":{},"mbps":{:.3},"fps":{:.2},"processing_fps":{:.2},"keyframes":{},"width":{},"height":{}}}"#,
            SOFTWARE_ENCODER_NAME,
            stats.frames_in,
            stats.samples_out,
            stats.bytes_out,
            mbps,
            config.fps,
            processing_fps,
            keyframes_json(stats),
            config.width,
            config.height
        );
        io::stdout().flush().ok();
    }

    fn keyframes_json(stats: &EncodeStats) -> String {
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
pub use platform::run;

#[cfg(not(windows))]
pub fn run(_config: EncodeProbeConfig) -> Result<(), String> {
    Err("encode-probe is only supported on Windows".to_string())
}
