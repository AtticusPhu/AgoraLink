#[cfg(windows)]
mod platform {
    use std::mem::ManuallyDrop;
    use std::ptr;
    use std::slice;

    use crate::color_spec::{ColorMatrix, ColorSpec, MediaColorMetadata};
    use windows::core::{IUnknown, Interface, Result as WindowsResult, GUID};
    use windows::Win32::Media::MediaFoundation::{
        IMF2DBuffer2, IMFMediaBuffer, IMFMediaType, IMFSample, IMFTransform,
        MF2DBuffer_LockFlags_Read, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
        MFMediaType_Video, MFNominalRange_16_235, MFShutdown, MFStartup, MFVideoFormat_H264,
        MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFVideoPrimaries_BT709,
        MFVideoPrimaries_SMPTE170M, MFVideoTransFunc_709, MFVideoTransferMatrix_BT601,
        MFVideoTransferMatrix_BT709, MFSTARTUP_FULL, MFT_MESSAGE_COMMAND_DRAIN,
        MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
        MFT_MESSAGE_NOTIFY_END_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
        MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_INFO,
        MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MF_E_NOTACCEPTING, MF_E_NO_MORE_TYPES,
        MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_DEFAULT_STRIDE,
        MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
        MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_MT_TRANSFER_FUNCTION,
        MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_VIDEO_PRIMARIES, MF_MT_YUV_MATRIX, MF_VERSION,
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
        pub y_stride: usize,
        pub uv_stride: usize,
        pub uv_offset: usize,
        pub allocated_height: usize,
        pub source_buffer_len: usize,
        pub expected_tight_len: usize,
        pub used_2d_buffer: bool,
        pub color_spec: ColorSpec,
        pub color_metadata: MediaColorMetadata,
    }

    struct OutputTypeInfo {
        stride: usize,
        metadata: MediaColorMetadata,
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
        output_stride: usize,
        output_color_metadata: MediaColorMetadata,
        fallback_color: ColorSpec,
        width: u32,
        height: u32,
        fps: u32,
        finished: bool,
        _mf: MediaFoundationGuard,
        _com: ComGuard,
    }

    impl WmfH264Decoder {
        pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, String> {
            Self::new_with_color(width, height, fps, ColorSpec::default())
        }

        pub fn new_with_color(
            width: u32,
            height: u32,
            fps: u32,
            fallback_color: ColorSpec,
        ) -> Result<Self, String> {
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
            apply_color_metadata(&input_type, fallback_color);
            unsafe { transform.SetInputType(0, &input_type, 0) }
                .map_err(|err| format!("SetInputType H.264 failed: {err}"))?;
            let output_type = select_nv12_output_type(&transform, width, fallback_color)?;
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
                output_stride: output_type.stride,
                output_color_metadata: output_type.metadata,
                fallback_color,
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
                    self.output_stride,
                    self.output_color_metadata,
                    self.fallback_color,
                )? {
                    OutputResult::Produced(frame) => {
                        frames.push(frame);
                        produced_any = true;
                    }
                    OutputResult::NeedMoreInput => return Ok(produced_any),
                    OutputResult::StreamChanged => {
                        let output_type = select_nv12_output_type(
                            &self.transform,
                            self.width,
                            self.fallback_color,
                        )?;
                        self.output_stride = output_type.stride;
                        self.output_color_metadata = output_type.metadata;
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
                eprintln!("decoder media type could not set {name}: {err}");
            }
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

    fn select_nv12_output_type(
        transform: &IMFTransform,
        width: u32,
        fallback_color: ColorSpec,
    ) -> Result<OutputTypeInfo, String> {
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
                apply_color_metadata(&media_type, fallback_color);
                unsafe { transform.SetOutputType(0, &media_type, 0) }
                    .map_err(|err| format!("SetOutputType advertised NV12 failed: {err}"))?;
                let current_type = unsafe { transform.GetOutputCurrentType(0) }
                    .unwrap_or_else(|_| media_type.clone());
                let metadata = read_color_metadata(&current_type);
                let stride = metadata
                    .default_stride
                    .map(|value| value.unsigned_abs() as usize)
                    .or_else(|| {
                        unsafe { current_type.GetUINT32(&MF_MT_DEFAULT_STRIDE) }
                            .ok()
                            .map(|value| (value as i32).unsigned_abs() as usize)
                    })
                    .filter(|stride| *stride >= width as usize)
                    .unwrap_or(width as usize);
                return Ok(OutputTypeInfo { stride, metadata });
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
        output_stride: usize,
        output_color_metadata: MediaColorMetadata,
        fallback_color: ColorSpec,
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
                Ok(OutputResult::Produced(read_nv12_sample(
                    &sample,
                    width,
                    height,
                    output_stride,
                    output_color_metadata,
                    fallback_color,
                )?))
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

    fn read_nv12_sample(
        sample: &IMFSample,
        width: u32,
        height: u32,
        configured_stride: usize,
        color_metadata: MediaColorMetadata,
        fallback_color: ColorSpec,
    ) -> Result<DecodedFrame, String> {
        let expected_tight_len = nv12_size(width, height)?;
        if let Ok(buffer) = unsafe { sample.GetBufferByIndex(0) } {
            if let Ok(buffer_2d) = buffer.cast::<IMF2DBuffer2>() {
                if let Some(frame) = try_read_2d_nv12(
                    &buffer_2d,
                    width,
                    height,
                    expected_tight_len,
                    color_metadata,
                    fallback_color,
                )? {
                    return Ok(frame);
                }
            }
        }

        let bytes = read_sample_bytes(sample)?;
        let source_buffer_len = bytes.len();
        let stride = resolve_contiguous_stride(
            source_buffer_len,
            width as usize,
            height as usize,
            configured_stride,
        )?;
        let allocated_height = resolve_allocated_height(source_buffer_len, stride, height as usize);
        let uv_offset = stride
            .checked_mul(allocated_height)
            .ok_or_else(|| "NV12 UV offset overflow".to_string())?;
        let layout_len = nv12_layout_len(uv_offset, stride, height as usize)?;
        Ok(DecodedFrame {
            nv12: bytes[..layout_len].to_vec(),
            y_stride: stride,
            uv_stride: stride,
            uv_offset,
            allocated_height,
            source_buffer_len,
            expected_tight_len,
            used_2d_buffer: false,
            color_spec: color_metadata.resolved_spec(fallback_color),
            color_metadata,
        })
    }

    fn try_read_2d_nv12(
        buffer: &IMF2DBuffer2,
        width: u32,
        height: u32,
        expected_tight_len: usize,
        color_metadata: MediaColorMetadata,
        fallback_color: ColorSpec,
    ) -> Result<Option<DecodedFrame>, String> {
        let mut scanline = ptr::null_mut();
        let mut pitch = 0i32;
        let mut buffer_start = ptr::null_mut();
        let mut buffer_len = 0u32;
        if unsafe {
            buffer.Lock2DSize(
                MF2DBuffer_LockFlags_Read,
                &mut scanline,
                &mut pitch,
                &mut buffer_start,
                &mut buffer_len,
            )
        }
        .is_err()
        {
            return Ok(None);
        }

        let result = (|| {
            if scanline.is_null()
                || buffer_start.is_null()
                || pitch <= 0
                || (pitch as usize) < width as usize
            {
                return Ok(None);
            }
            let stride = pitch as usize;
            let scanline_offset = unsafe { scanline.offset_from(buffer_start) };
            if scanline_offset < 0 {
                return Ok(None);
            }
            let available_len = (buffer_len as usize).saturating_sub(scanline_offset as usize);
            let allocated_height = resolve_allocated_height(available_len, stride, height as usize);
            let uv_offset = stride
                .checked_mul(allocated_height)
                .ok_or_else(|| "NV12 UV offset overflow".to_string())?;
            let layout_len = nv12_layout_len(uv_offset, stride, height as usize)?;
            if scanline_offset as usize + layout_len > buffer_len as usize {
                return Ok(None);
            }
            let mut nv12 = vec![0u8; layout_len];
            unsafe {
                ptr::copy_nonoverlapping(scanline, nv12.as_mut_ptr(), layout_len);
            }
            Ok(Some(DecodedFrame {
                nv12,
                y_stride: stride,
                uv_stride: stride,
                uv_offset,
                allocated_height,
                source_buffer_len: buffer_len as usize,
                expected_tight_len,
                used_2d_buffer: true,
                color_spec: color_metadata.resolved_spec(fallback_color),
                color_metadata,
            }))
        })();
        let unlock_result = unsafe { buffer.Unlock2D() };
        result.and_then(|frame| {
            unlock_result.map_err(|err| format!("decoder output Unlock2D failed: {err}"))?;
            Ok(frame)
        })
    }

    fn resolve_contiguous_stride(
        buffer_len: usize,
        width: usize,
        height: usize,
        configured_stride: usize,
    ) -> Result<usize, String> {
        let tight_uv_offset = width
            .checked_mul(height)
            .ok_or_else(|| "NV12 tight UV offset overflow".to_string())?;
        let tight_len = nv12_layout_len(tight_uv_offset, width, height)?;
        if buffer_len == tight_len {
            return Ok(width);
        }
        if configured_stride >= width
            && buffer_len
                >= nv12_layout_len(
                    configured_stride
                        .checked_mul(height)
                        .ok_or_else(|| "NV12 configured UV offset overflow".to_string())?,
                    configured_stride,
                    height,
                )?
        {
            return Ok(configured_stride);
        }
        let denominator = height
            .checked_mul(3)
            .ok_or_else(|| "NV12 stride inference overflow".to_string())?;
        let doubled = buffer_len
            .checked_mul(2)
            .ok_or_else(|| "NV12 buffer length overflow".to_string())?;
        if denominator != 0 && doubled % denominator == 0 {
            let inferred = doubled / denominator;
            if inferred >= width {
                return Ok(inferred);
            }
        }
        Err(format!(
            "cannot determine NV12 stride: width={width}, height={height}, buffer_len={buffer_len}, configured_stride={configured_stride}"
        ))
    }

    fn resolve_allocated_height(buffer_len: usize, stride: usize, visible_height: usize) -> usize {
        let denominator = stride.saturating_mul(3);
        let doubled = buffer_len.saturating_mul(2);
        if denominator != 0 && doubled % denominator == 0 {
            let allocated = doubled / denominator;
            if allocated >= visible_height && allocated % 2 == 0 {
                return allocated;
            }
        }
        visible_height
    }

    fn nv12_layout_len(uv_offset: usize, uv_stride: usize, height: usize) -> Result<usize, String> {
        uv_stride
            .checked_mul(height / 2)
            .and_then(|uv| uv_offset.checked_add(uv))
            .ok_or_else(|| "NV12 layout size overflow".to_string())
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
