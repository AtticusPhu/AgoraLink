#[cfg(windows)]
mod platform {
    use std::mem::ManuallyDrop;
    use std::ptr;
    use std::slice;

    use windows::core::{IUnknown, Result as WindowsResult, GUID};
    use windows::Win32::Media::MediaFoundation::{
        IMFMediaBuffer, IMFMediaType, IMFSample, IMFTransform, MFCreateMediaType,
        MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFShutdown, MFStartup,
        MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFSTARTUP_FULL,
        MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
        MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_END_STREAMING,
        MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
        MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_INFO,
        MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MF_E_NOTACCEPTING, MF_E_NO_MORE_TYPES,
        MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_FRAME_RATE,
        MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO,
        MF_MT_SUBTYPE, MF_VERSION,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED,
    };

    pub const DECODER_NAME: &str = "Microsoft H.264 Video Decoder MFT";
    const H264_DECODER_CLSID: GUID = GUID::from_u128(0x62ce7e72_4c71_4d20_b15d_452831a87d9d);
    const HNS_PER_SECOND: i64 = 10_000_000;

    pub struct DecodedFrame {
        pub nv12: Vec<u8>,
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
        Produced(DecodedFrame),
        NeedMoreInput,
        StreamChanged,
    }

    pub struct WmfH264Decoder {
        transform: IMFTransform,
        output_info: MFT_OUTPUT_STREAM_INFO,
        output_buffer_size: u32,
        width: u32,
        height: u32,
        fps: u32,
        finished: bool,
        _mf: MediaFoundationGuard,
        _com: ComGuard,
    }

    impl WmfH264Decoder {
        pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, String> {
            if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
                return Err("decoder width and height must be non-zero even values".to_string());
            }
            if fps == 0 {
                return Err("decoder fps must be greater than zero".to_string());
            }
            let com = ComGuard::initialize()?;
            let mf = MediaFoundationGuard::startup()?;
            let transform = create_decoder()?;
            let input_type = create_video_type(width, height, fps, MFVideoFormat_H264)?;
            unsafe { transform.SetInputType(0, &input_type, 0) }
                .map_err(|err| format!("SetInputType H.264 failed: {err}"))?;
            select_nv12_output_type(&transform)?;
            let output_info = unsafe { transform.GetOutputStreamInfo(0) }
                .map_err(|err| format!("GetOutputStreamInfo failed: {err}"))?;
            let frame_size = nv12_size(width, height)?;
            let output_buffer_size = output_info.cbSize.max(frame_size as u32);

            unsafe {
                transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                    .map_err(|err| format!("decoder begin streaming failed: {err}"))?;
                transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                    .map_err(|err| format!("decoder start of stream failed: {err}"))?;
            }

            Ok(Self {
                transform,
                output_info,
                output_buffer_size,
                width,
                height,
                fps,
                finished: false,
                _mf: mf,
                _com: com,
            })
        }

        pub fn decode_access_unit(
            &mut self,
            bytes: &[u8],
            frame_index: u64,
        ) -> Result<Vec<DecodedFrame>, String> {
            if self.finished {
                return Err("decoder has already been finished".to_string());
            }
            let sample_time = (frame_index as i64)
                .checked_mul(HNS_PER_SECOND)
                .ok_or_else(|| "decoder sample time overflow".to_string())?
                / i64::from(self.fps);
            let sample_duration = HNS_PER_SECOND / i64::from(self.fps);
            let sample = create_input_sample(bytes, sample_time, sample_duration)?;
            let mut frames = Vec::new();
            loop {
                match unsafe { self.transform.ProcessInput(0, &sample, 0) } {
                    Ok(()) => break,
                    Err(err) if err.code() == MF_E_NOTACCEPTING => {
                        if !self.drain_available(&mut frames)? {
                            return Err(
                                "decoder rejected input but produced no pending output".to_string()
                            );
                        }
                    }
                    Err(err) => return Err(format!("decoder ProcessInput failed: {err}")),
                }
            }
            self.drain_available(&mut frames)?;
            Ok(frames)
        }

        pub fn finish(&mut self) -> Result<Vec<DecodedFrame>, String> {
            if self.finished {
                return Ok(Vec::new());
            }
            unsafe {
                self.transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
                    .map_err(|err| format!("decoder end of stream failed: {err}"))?;
                self.transform
                    .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                    .map_err(|err| format!("decoder drain command failed: {err}"))?;
            }
            let mut frames = Vec::new();
            self.drain_available(&mut frames)?;
            let _ = unsafe {
                self.transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0)
            };
            self.finished = true;
            Ok(frames)
        }

        fn drain_available(&mut self, frames: &mut Vec<DecodedFrame>) -> Result<bool, String> {
            let mut produced_any = false;
            loop {
                match process_one_output(
                    &self.transform,
                    &self.output_info,
                    self.output_buffer_size,
                    self.width,
                    self.height,
                )? {
                    OutputResult::Produced(frame) => {
                        frames.push(frame);
                        produced_any = true;
                    }
                    OutputResult::NeedMoreInput => return Ok(produced_any),
                    OutputResult::StreamChanged => {
                        select_nv12_output_type(&self.transform)?;
                        self.output_info = unsafe { self.transform.GetOutputStreamInfo(0) }
                            .map_err(|err| {
                                format!("GetOutputStreamInfo after stream change failed: {err}")
                            })?;
                        self.output_buffer_size =
                            self.output_info
                                .cbSize
                                .max(nv12_size(self.width, self.height)? as u32);
                    }
                }
            }
        }
    }

    impl Drop for WmfH264Decoder {
        fn drop(&mut self) {
            if !self.finished {
                let _ = unsafe {
                    self.transform
                        .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0)
                };
            }
        }
    }

    fn create_decoder() -> Result<IMFTransform, String> {
        unsafe {
            CoCreateInstance::<_, IMFTransform>(
                &H264_DECODER_CLSID,
                None::<&IUnknown>,
                CLSCTX_INPROC_SERVER,
            )
        }
        .map_err(|err| format!("create Microsoft H.264 decoder MFT failed: {err}"))
    }

    fn create_video_type(
        width: u32,
        height: u32,
        fps: u32,
        subtype: GUID,
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
            }
            Ok(())
        })();
        configure_result.map_err(|err| format!("configure decoder media type failed: {err}"))?;
        Ok(media_type)
    }

    fn select_nv12_output_type(transform: &IMFTransform) -> Result<(), String> {
        let mut index = 0u32;
        let mut available = Vec::new();
        loop {
            let media_type = match unsafe { transform.GetOutputAvailableType(0, index) } {
                Ok(media_type) => media_type,
                Err(err) if err.code() == MF_E_NO_MORE_TYPES => break,
                Err(err) => return Err(format!("GetOutputAvailableType failed: {err}")),
            };
            let subtype = unsafe { media_type.GetGUID(&MF_MT_SUBTYPE) }
                .map_err(|err| format!("decoder output subtype query failed: {err}"))?;
            available.push(format!("{subtype:?}"));
            if subtype == MFVideoFormat_NV12 {
                unsafe { transform.SetOutputType(0, &media_type, 0) }
                    .map_err(|err| format!("SetOutputType advertised NV12 failed: {err}"))?;
                return Ok(());
            }
            index += 1;
        }
        Err(format!(
            "decoder does not advertise NV12 output; available={}",
            available.join(",")
        ))
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
                .map_err(|err| format!("decoder input AddBuffer failed: {err}"))?;
            sample
                .SetSampleTime(sample_time)
                .map_err(|err| format!("decoder SetSampleTime failed: {err}"))?;
            sample
                .SetSampleDuration(sample_duration)
                .map_err(|err| format!("decoder SetSampleDuration failed: {err}"))?;
        }
        Ok(sample)
    }

    fn write_buffer(buffer: &IMFMediaBuffer, data: &[u8]) -> Result<(), String> {
        let mut pointer = ptr::null_mut();
        let mut max_length = 0u32;
        unsafe { buffer.Lock(&mut pointer, Some(&mut max_length), None) }
            .map_err(|err| format!("decoder input buffer Lock failed: {err}"))?;
        let result = if pointer.is_null() || max_length < data.len() as u32 {
            Err("decoder input buffer is smaller than access unit".to_string())
        } else {
            unsafe { ptr::copy_nonoverlapping(data.as_ptr(), pointer, data.len()) };
            Ok(())
        };
        let unlock_result = unsafe { buffer.Unlock() };
        result?;
        unlock_result.map_err(|err| format!("decoder input buffer Unlock failed: {err}"))?;
        unsafe { buffer.SetCurrentLength(data.len() as u32) }
            .map_err(|err| format!("decoder input SetCurrentLength failed: {err}"))
    }

    fn process_one_output(
        transform: &IMFTransform,
        output_info: &MFT_OUTPUT_STREAM_INFO,
        output_buffer_size: u32,
        width: u32,
        height: u32,
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
                    .ok_or_else(|| "decoder ProcessOutput returned no sample".to_string())?;
                let bytes = read_sample_bytes(&sample)?;
                let expected = nv12_size(width, height)?;
                if bytes.len() < expected {
                    return Err(format!(
                        "decoder NV12 sample too small: expected {expected}, got {}",
                        bytes.len()
                    ));
                }
                Ok(OutputResult::Produced(DecodedFrame {
                    nv12: bytes[..expected].to_vec(),
                }))
            }
            Err(err) if err.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                Ok(OutputResult::NeedMoreInput)
            }
            Err(err) if err.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                Ok(OutputResult::StreamChanged)
            }
            Err(err) => Err(format!("decoder ProcessOutput failed: {err}")),
        }
    }

    fn create_output_sample(buffer_size: u32) -> Result<IMFSample, String> {
        let buffer = unsafe { MFCreateMemoryBuffer(buffer_size) }
            .map_err(|err| format!("MFCreateMemoryBuffer output failed: {err}"))?;
        let sample =
            unsafe { MFCreateSample() }.map_err(|err| format!("MFCreateSample output: {err}"))?;
        unsafe { sample.AddBuffer(&buffer) }
            .map_err(|err| format!("decoder output AddBuffer failed: {err}"))?;
        Ok(sample)
    }

    fn read_sample_bytes(sample: &IMFSample) -> Result<Vec<u8>, String> {
        let buffer = unsafe { sample.ConvertToContiguousBuffer() }
            .map_err(|err| format!("ConvertToContiguousBuffer failed: {err}"))?;
        let length = unsafe { buffer.GetCurrentLength() }
            .map_err(|err| format!("decoder output GetCurrentLength failed: {err}"))?;
        let mut pointer = ptr::null_mut();
        unsafe { buffer.Lock(&mut pointer, None, None) }
            .map_err(|err| format!("decoder output buffer Lock failed: {err}"))?;
        let bytes = if pointer.is_null() {
            Vec::new()
        } else {
            unsafe { slice::from_raw_parts(pointer, length as usize) }.to_vec()
        };
        unsafe { buffer.Unlock() }
            .map_err(|err| format!("decoder output buffer Unlock failed: {err}"))?;
        Ok(bytes)
    }

    fn nv12_size(width: u32, height: u32) -> Result<usize, String> {
        (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_add(pixels / 2))
            .ok_or_else(|| "NV12 decoder frame size overflow".to_string())
    }

    fn pack_ratio(numerator: u32, denominator: u32) -> u64 {
        (u64::from(numerator) << 32) | u64::from(denominator)
    }
}

#[cfg(windows)]
pub use platform::{DecodedFrame, WmfH264Decoder, DECODER_NAME};
