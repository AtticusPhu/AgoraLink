use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AudioCaptureProbeConfig {
    pub duration_sec: u64,
    pub output: PathBuf,
}

#[cfg(windows)]
mod imp {
    use super::AudioCaptureProbeConfig;
    use crate::json_escape;
    use std::convert::TryFrom;
    use std::ffi::c_void;
    use std::fs::File;
    use std::io::Write;
    use std::path::Path;
    use std::ptr;
    use std::slice;
    use std::thread;
    use std::time::{Duration, Instant};
    use windows::core::{BSTR, GUID};
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator,
        MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
        AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_LOOPBACK,
        AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
        WAVE_FORMAT_PCM,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED, STGM_READ,
    };

    const REFTIMES_PER_SEC: i64 = 10_000_000;
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
    const PCM_SUBFORMAT: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
    const FLOAT_SUBFORMAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SampleKind {
        Pcm,
        Float,
        Unsupported,
    }

    #[derive(Debug, Clone)]
    struct AudioFormat {
        sample_rate: u32,
        channels: u16,
        bits_per_sample: u16,
        block_align: u16,
        sample_kind: SampleKind,
    }

    #[derive(Debug, Default)]
    struct CaptureStats {
        frames_captured: u64,
        capture_callbacks: u64,
        silence_packets: u64,
        discontinuity_count: u64,
        glitch_count: u64,
        qpc_timestamp_available: bool,
    }

    struct ComGuard;

    impl ComGuard {
        fn init() -> Result<Self, String> {
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }
                .map_err(|err| format!("CoInitializeEx failed: {err}"))?;
            Ok(Self)
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    pub fn run(config: AudioCaptureProbeConfig) -> Result<(), String> {
        if config.duration_sec == 0 {
            return Err("--duration-sec must be greater than zero".to_string());
        }
        let _com = ComGuard::init()?;
        let endpoint = default_render_endpoint()?;
        let device_name = endpoint_friendly_name(&endpoint)
            .unwrap_or_else(|| "default render endpoint".to_string());
        let client: IAudioClient = unsafe {
            endpoint
                .Activate(CLSCTX_ALL, None)
                .map_err(|err| format!("IAudioClient activation failed: {err}"))?
        };

        let mut desired = desired_pcm_format();
        let desired_ptr = &mut desired as *mut WAVEFORMATEX;
        let init_flags = AUDCLNT_STREAMFLAGS_LOOPBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        let (format, format_ptr, using_mix_format) = match unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                init_flags,
                REFTIMES_PER_SEC,
                0,
                desired_ptr,
                None,
            )
        } {
            Ok(()) => (audio_format_from_ptr(desired_ptr)?, desired_ptr, false),
            Err(err) => {
                eprintln!(
                        "audio-capture-probe 48k stereo PCM init failed, falling back to mix format: {err}"
                    );
                let mix_ptr = unsafe {
                    client
                        .GetMixFormat()
                        .map_err(|err| format!("GetMixFormat failed: {err}"))?
                };
                unsafe {
                    client
                        .Initialize(
                            AUDCLNT_SHAREMODE_SHARED,
                            AUDCLNT_STREAMFLAGS_LOOPBACK,
                            REFTIMES_PER_SEC,
                            0,
                            mix_ptr,
                            None,
                        )
                        .map_err(|err| format!("IAudioClient Initialize failed: {err}"))?;
                }
                let parsed = audio_format_from_ptr(mix_ptr)?;
                (parsed, mix_ptr, true)
            }
        };

        if format.channels == 0 || format.block_align == 0 || format.sample_rate == 0 {
            unsafe { maybe_free_wave_format(format_ptr, using_mix_format) };
            return Err("WASAPI returned an invalid audio format".to_string());
        }
        if format.sample_kind == SampleKind::Unsupported {
            unsafe { maybe_free_wave_format(format_ptr, using_mix_format) };
            return Err(format!(
                "unsupported WASAPI mix format: {} Hz, {} channels, {} bits",
                format.sample_rate, format.channels, format.bits_per_sample
            ));
        }

        let capture: IAudioCaptureClient = unsafe {
            client
                .GetService()
                .map_err(|err| format!("IAudioCaptureClient GetService failed: {err}"))?
        };

        let mut output =
            File::create(&config.output).map_err(|err| format!("create output failed: {err}"))?;
        eprintln!(
            "audio-capture-probe device=\"{}\" output={} format={}Hz/{}ch/{:?}/{}bits",
            device_name,
            config.output.display(),
            format.sample_rate,
            format.channels,
            format.sample_kind,
            format.bits_per_sample
        );

        unsafe {
            client
                .Start()
                .map_err(|err| format!("IAudioClient Start failed: {err}"))?
        };
        let started = Instant::now();
        let deadline = started + Duration::from_secs(config.duration_sec);
        let mut stats = CaptureStats::default();
        let capture_result: Result<(), String> = loop {
            if Instant::now() >= deadline {
                break Ok(());
            }
            let mut packet_size = unsafe {
                capture
                    .GetNextPacketSize()
                    .map_err(|err| format!("GetNextPacketSize failed: {err}"))?
            };
            if packet_size == 0 {
                thread::sleep(Duration::from_millis(5));
                continue;
            }
            while packet_size > 0 {
                read_one_packet(&capture, &format, &mut output, &mut stats)?;
                packet_size = unsafe {
                    capture
                        .GetNextPacketSize()
                        .map_err(|err| format!("GetNextPacketSize failed: {err}"))?
                };
            }
        };
        let stop_result =
            unsafe { client.Stop() }.map_err(|err| format!("IAudioClient Stop failed: {err}"));
        unsafe { maybe_free_wave_format(format_ptr, using_mix_format) };
        capture_result?;
        stop_result?;
        output
            .flush()
            .map_err(|err| format!("flush output failed: {err}"))?;

        let audio_duration = if format.sample_rate > 0 {
            stats.frames_captured as f64 / format.sample_rate as f64
        } else {
            0.0
        };
        println!(
            r#"{{"type":"AUDIO_CAPTURE_STATS","mode":"audio_capture_probe","sample_rate":{},"channels":{},"bits_per_sample":16,"frames_captured":{},"audio_duration_sec":{:.3},"capture_callbacks":{},"silence_packets":{},"discontinuity_count":{},"glitch_count":{},"qpc_timestamp_available":{},"device_name":"{}","output":"{}","source_bits_per_sample":{},"source_sample_kind":"{}"}}"#,
            format.sample_rate,
            format.channels,
            stats.frames_captured,
            audio_duration,
            stats.capture_callbacks,
            stats.silence_packets,
            stats.discontinuity_count,
            stats.glitch_count,
            stats.qpc_timestamp_available,
            json_escape(&device_name),
            json_escape(&path_to_string(&config.output)),
            format.bits_per_sample,
            sample_kind_name(format.sample_kind),
        );
        Ok(())
    }

    fn read_one_packet(
        capture: &IAudioCaptureClient,
        format: &AudioFormat,
        output: &mut File,
        stats: &mut CaptureStats,
    ) -> Result<(), String> {
        let mut data = ptr::null_mut();
        let mut frames = 0u32;
        let mut flags = 0u32;
        let mut device_position = 0u64;
        let mut qpc_position = 0u64;
        unsafe {
            capture
                .GetBuffer(
                    &mut data,
                    &mut frames,
                    &mut flags,
                    Some(&mut device_position),
                    Some(&mut qpc_position),
                )
                .map_err(|err| format!("IAudioCaptureClient GetBuffer failed: {err}"))?;
        }
        let result = (|| {
            if frames == 0 {
                return Ok(());
            }
            stats.capture_callbacks += 1;
            stats.frames_captured += u64::from(frames);
            stats.qpc_timestamp_available |= qpc_position != 0;
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                stats.silence_packets += 1;
                write_silence(output, frames, format.channels)?;
            } else {
                let byte_len = frames as usize * format.block_align as usize;
                let bytes = unsafe { slice::from_raw_parts(data.cast::<u8>(), byte_len) };
                write_converted_pcm16(output, bytes, frames, format)?;
            }
            if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 {
                stats.discontinuity_count += 1;
                stats.glitch_count += 1;
            }
            if flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0 {
                stats.glitch_count += 1;
            }
            Ok(())
        })();
        unsafe {
            capture
                .ReleaseBuffer(frames)
                .map_err(|err| format!("IAudioCaptureClient ReleaseBuffer failed: {err}"))?;
        }
        result
    }

    fn default_render_endpoint() -> Result<IMMDevice, String> {
        let enumerator: IMMDeviceEnumerator = unsafe {
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|err| format!("MMDeviceEnumerator creation failed: {err}"))?
        };
        unsafe {
            enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|err| format!("GetDefaultAudioEndpoint failed: {err}"))
        }
    }

    fn endpoint_friendly_name(device: &IMMDevice) -> Option<String> {
        let store = unsafe { device.OpenPropertyStore(STGM_READ).ok()? };
        let prop = unsafe { store.GetValue(&PKEY_Device_FriendlyName).ok()? };
        let value = BSTR::try_from(&prop).ok()?;
        let text = value.to_string();
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    }

    fn desired_pcm_format() -> WAVEFORMATEX {
        WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 2,
            nSamplesPerSec: 48_000,
            nAvgBytesPerSec: 48_000 * 2 * 2,
            nBlockAlign: 2 * 2,
            wBitsPerSample: 16,
            cbSize: 0,
        }
    }

    fn audio_format_from_ptr(ptr: *const WAVEFORMATEX) -> Result<AudioFormat, String> {
        if ptr.is_null() {
            return Err("null WAVEFORMATEX".to_string());
        }
        let base = unsafe { *ptr };
        let tag = base.wFormatTag;
        let mut bits = base.wBitsPerSample;
        let kind = match u32::from(tag) {
            WAVE_FORMAT_PCM => SampleKind::Pcm,
            x if x == u32::from(WAVE_FORMAT_IEEE_FLOAT) => SampleKind::Float,
            x if x == u32::from(WAVE_FORMAT_EXTENSIBLE) => {
                let ext_ptr = ptr as *const WAVEFORMATEXTENSIBLE;
                let sub_format = unsafe { ptr::addr_of!((*ext_ptr).SubFormat).read_unaligned() };
                if sub_format == PCM_SUBFORMAT {
                    SampleKind::Pcm
                } else if sub_format == FLOAT_SUBFORMAT {
                    SampleKind::Float
                } else {
                    SampleKind::Unsupported
                }
            }
            _ => SampleKind::Unsupported,
        };
        if kind == SampleKind::Float && bits == 0 {
            bits = 32;
        }
        Ok(AudioFormat {
            sample_rate: base.nSamplesPerSec,
            channels: base.nChannels,
            bits_per_sample: bits,
            block_align: base.nBlockAlign,
            sample_kind: kind,
        })
    }

    fn write_silence(output: &mut File, frames: u32, channels: u16) -> Result<(), String> {
        let len = frames as usize * channels as usize * 2;
        let zeros = vec![0u8; len];
        output
            .write_all(&zeros)
            .map_err(|err| format!("write silence failed: {err}"))
    }

    fn write_converted_pcm16(
        output: &mut File,
        bytes: &[u8],
        frames: u32,
        format: &AudioFormat,
    ) -> Result<(), String> {
        let channels = format.channels as usize;
        let block_align = format.block_align as usize;
        let mut out = Vec::with_capacity(frames as usize * channels * 2);
        for frame in 0..frames as usize {
            let frame_offset = frame * block_align;
            for channel in 0..channels {
                let sample_offset = frame_offset + sample_offset_for_channel(format, channel);
                let sample = sample_to_i16(bytes, sample_offset, format)?;
                out.extend_from_slice(&sample.to_le_bytes());
            }
        }
        output
            .write_all(&out)
            .map_err(|err| format!("write PCM failed: {err}"))
    }

    fn sample_offset_for_channel(format: &AudioFormat, channel: usize) -> usize {
        let bytes_per_sample = (format.bits_per_sample as usize).saturating_div(8).max(1);
        channel * bytes_per_sample
    }

    fn sample_to_i16(bytes: &[u8], offset: usize, format: &AudioFormat) -> Result<i16, String> {
        match format.sample_kind {
            SampleKind::Pcm => match format.bits_per_sample {
                8 => {
                    let value = *bytes.get(offset).ok_or("PCM8 sample out of range")?;
                    Ok(((i16::from(value) - 128) << 8) as i16)
                }
                16 => {
                    let raw = bytes
                        .get(offset..offset + 2)
                        .ok_or("PCM16 sample out of range")?;
                    Ok(i16::from_le_bytes([raw[0], raw[1]]))
                }
                24 => {
                    let raw = bytes
                        .get(offset..offset + 3)
                        .ok_or("PCM24 sample out of range")?;
                    let value =
                        ((raw[0] as i32) << 8) | ((raw[1] as i32) << 16) | ((raw[2] as i32) << 24);
                    Ok((value >> 16) as i16)
                }
                32 => {
                    let raw = bytes
                        .get(offset..offset + 4)
                        .ok_or("PCM32 sample out of range")?;
                    let value = i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
                    Ok((value >> 16) as i16)
                }
                other => Err(format!("unsupported PCM bits_per_sample={other}")),
            },
            SampleKind::Float => {
                if format.bits_per_sample != 32 {
                    return Err(format!(
                        "unsupported float bits_per_sample={}",
                        format.bits_per_sample
                    ));
                }
                let raw = bytes
                    .get(offset..offset + 4)
                    .ok_or("float32 sample out of range")?;
                let value = f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
                Ok((value.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
            }
            SampleKind::Unsupported => Err("unsupported sample format".to_string()),
        }
    }

    unsafe fn maybe_free_wave_format(ptr: *mut WAVEFORMATEX, allocated: bool) {
        if allocated && !ptr.is_null() {
            CoTaskMemFree(Some(ptr.cast::<c_void>()));
        }
    }

    fn sample_kind_name(kind: SampleKind) -> &'static str {
        match kind {
            SampleKind::Pcm => "pcm",
            SampleKind::Float => "float",
            SampleKind::Unsupported => "unsupported",
        }
    }

    fn path_to_string(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }
}

#[cfg(windows)]
pub use imp::run;

#[cfg(not(windows))]
pub fn run(_config: AudioCaptureProbeConfig) -> Result<(), String> {
    Err("audio-capture-probe is only supported on Windows".to_string())
}
