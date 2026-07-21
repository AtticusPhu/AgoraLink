use std::collections::HashMap;
use std::env;
use std::io::{self, Write};
use std::net::UdpSocket;
use std::path::PathBuf;
use std::process;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod adaptive_quality;
mod async_mft_wait;
mod audio_capture_probe;
mod audio_timeline;
mod audio_udp;
mod av_sync;
mod bench_reassembly;
mod bgra_to_nv12;
mod bitrate;
mod callback_lifecycle;
mod capture_encode_probe;
mod capture_probe;
mod color_spec;
mod color_test_pattern;
mod d3d11_nv12_renderer;
mod decoded_frame_renderer;
mod display_capability;
mod encode_probe;
mod fec;
mod frame_rate_policy;
mod gpu_convert_probe;
mod gpu_nv12_capture;
mod h264_annex_b;
mod h264_file_viewer;
mod h264_reassembly;
mod h264_recv_dump;
mod h264_recv_view;
mod h264_send_probe;
mod local_control;
mod media_clock;
mod media_control;
mod nv12_synthetic;
mod nv12_to_bgra;
mod playout_buffer;
mod profile_transition;
mod repair;
mod sender_scheduling;
mod shutdown;
mod udp_socket;
mod video_renderer;
mod wgc_latest_capture;
mod win32_gdi_viewer;
mod wmf_h264_decoder;
mod wmf_h264_encoder;
mod wmf_probe;

const MAGIC: &[u8; 4] = b"AGM1";
const VERSION: u8 = 1;
pub(crate) const STREAM_VIDEO: u8 = 1;
pub(crate) const FLAG_KEYFRAME: u16 = 1 << 0;
pub(crate) const FLAG_END_OF_FRAME: u16 = 1 << 1;
pub(crate) const FLAG_CONFIG: u16 = 1 << 2;
pub(crate) const FLAG_H264_ANNEX_B: u16 = 1 << 3;
pub(crate) const FLAG_FEC: u16 = 1 << 4;
pub(crate) const FLAG_FEC_PROTECTED: u16 = 1 << 5;
pub(crate) const HEADER_LEN: usize = 38;
pub(crate) const MIN_UDP_PAYLOAD_SIZE: usize = 576;
pub(crate) const LEGACY_UDP_PAYLOAD_SIZE: usize = 1200;
pub(crate) const DEFAULT_REALTIME_UDP_PAYLOAD_SIZE: usize = 1452;
pub(crate) const MAX_UDP_PAYLOAD_SIZE: usize = 1472;
pub(crate) const MAX_MEDIA_PAYLOAD: usize = MAX_UDP_PAYLOAD_SIZE - HEADER_LEN;
pub(crate) const MIN_MEDIA_PAYLOAD: usize = MIN_UDP_PAYLOAD_SIZE - HEADER_LEN;
pub(crate) const LEGACY_MEDIA_PAYLOAD: usize = LEGACY_UDP_PAYLOAD_SIZE - HEADER_LEN;
pub(crate) const MAX_VIDEO_FRAME_BYTES: usize = 16 * 1024 * 1024;
const FEC_PARITY_PACKET_ALLOWANCE: usize = 1;
const VIDEO_PACKET_COUNT_SAFETY_MARGIN: usize = 4;
pub(crate) const MAX_VIDEO_PACKET_COUNT: usize = MAX_VIDEO_FRAME_BYTES.div_ceil(MIN_MEDIA_PAYLOAD)
    + FEC_PARITY_PACKET_ALLOWANCE
    + VIDEO_PACKET_COUNT_SAFETY_MARGIN;
pub(crate) const MAX_INFLIGHT_PACKET_SLOTS: usize = 64 * 1024;
pub(crate) const MAX_INFLIGHT_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_INFLIGHT_FRAMES: usize = 120;
const FRAME_TTL: Duration = Duration::from_millis(1000);

#[derive(Debug, Clone)]
pub(crate) struct MediaPacket {
    pub(crate) stream_id: u8,
    pub(crate) flags: u16,
    pub(crate) session_id: u64,
    pub(crate) frame_id: u64,
    pub(crate) packet_index: u16,
    pub(crate) packet_count: u16,
    pub(crate) timestamp_ms: u64,
    pub(crate) payload: Vec<u8>,
}

impl MediaPacket {
    pub(crate) fn encode(&self) -> Result<Vec<u8>, String> {
        self.encode_with_udp_payload_size(LEGACY_UDP_PAYLOAD_SIZE)
    }

    pub(crate) fn encode_with_udp_payload_size(
        &self,
        udp_payload_size: usize,
    ) -> Result<Vec<u8>, String> {
        validate_udp_payload_size(udp_payload_size)?;
        let media_payload_size = udp_payload_size - HEADER_LEN;
        if self.payload.len() > media_payload_size {
            return Err(format!("payload too large: {}", self.payload.len()));
        }
        if self.payload.len() > u16::MAX as usize {
            return Err("payload length exceeds u16".to_string());
        }
        if self.packet_count == 0 {
            return Err("packet_count must be greater than zero".to_string());
        }
        if usize::from(self.packet_count) > MAX_VIDEO_PACKET_COUNT {
            return Err(format!(
                "packet_count exceeds video frame limit: {} > {}",
                self.packet_count, MAX_VIDEO_PACKET_COUNT
            ));
        }
        if self.packet_index >= self.packet_count {
            return Err("packet_index must be less than packet_count".to_string());
        }

        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(self.stream_id);
        out.extend_from_slice(&self.flags.to_be_bytes());
        out.extend_from_slice(&self.session_id.to_be_bytes());
        out.extend_from_slice(&self.frame_id.to_be_bytes());
        out.extend_from_slice(&self.packet_index.to_be_bytes());
        out.extend_from_slice(&self.packet_count.to_be_bytes());
        out.extend_from_slice(&self.timestamp_ms.to_be_bytes());
        out.extend_from_slice(&(self.payload.len() as u16).to_be_bytes());
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    pub(crate) fn decode(buf: &[u8]) -> Result<Self, String> {
        if buf.len() > MAX_UDP_PAYLOAD_SIZE {
            return Err(format!("datagram exceeds UDP payload limit: {}", buf.len()));
        }
        if buf.len() < HEADER_LEN {
            return Err("packet too short".to_string());
        }
        if &buf[0..4] != MAGIC {
            return Err("bad magic".to_string());
        }
        if buf[4] != VERSION {
            return Err(format!("unsupported version: {}", buf[4]));
        }

        let stream_id = buf[5];
        let flags = u16::from_be_bytes([buf[6], buf[7]]);
        let session_id = u64::from_be_bytes(buf[8..16].try_into().unwrap());
        let frame_id = u64::from_be_bytes(buf[16..24].try_into().unwrap());
        let packet_index = u16::from_be_bytes([buf[24], buf[25]]);
        let packet_count = u16::from_be_bytes([buf[26], buf[27]]);
        let timestamp_ms = u64::from_be_bytes(buf[28..36].try_into().unwrap());
        let payload_len = u16::from_be_bytes([buf[36], buf[37]]) as usize;

        if packet_count == 0 {
            return Err("packet_count is zero".to_string());
        }
        if usize::from(packet_count) > MAX_VIDEO_PACKET_COUNT {
            return Err(format!(
                "packet_count exceeds video frame limit: {packet_count} > {MAX_VIDEO_PACKET_COUNT}"
            ));
        }
        if packet_index >= packet_count {
            return Err("packet_index out of range".to_string());
        }
        if payload_len > MAX_MEDIA_PAYLOAD {
            return Err(format!("payload_len exceeds limit: {}", payload_len));
        }
        if HEADER_LEN + payload_len > buf.len() {
            return Err("payload_len exceeds datagram length".to_string());
        }

        Ok(Self {
            stream_id,
            flags,
            session_id,
            frame_id,
            packet_index,
            packet_count,
            timestamp_ms,
            payload: buf[HEADER_LEN..HEADER_LEN + payload_len].to_vec(),
        })
    }
}

#[derive(Debug)]
enum Command {
    SelfTest,
    Sender {
        host: String,
        port: u16,
        fps: u32,
        bitrate_mbps: f64,
    },
    Receiver {
        bind: String,
        port: u16,
    },
    CaptureProbe {
        duration_sec: u64,
        target_fps: u32,
    },
    WmfProbe,
    GpuConvertProbe(gpu_convert_probe::GpuConvertProbeConfig),
    EncodeProbe(encode_probe::EncodeProbeConfig),
    CaptureEncodeProbe(capture_encode_probe::CaptureEncodeConfig),
    H264SendProbe(h264_send_probe::H264SendConfig),
    H264RecvDump(h264_recv_dump::H264RecvConfig),
    H264RecvView(h264_recv_view::H264RecvViewConfig),
    ScreenSend(h264_send_probe::H264SendConfig),
    ScreenRecv(h264_recv_view::H264RecvViewConfig),
    H264FileViewer(h264_file_viewer::H264FileViewerConfig),
    ColorTestPattern(color_test_pattern::ColorTestPatternConfig),
    BenchReassembly(bench_reassembly::BenchReassemblyConfig),
    AudioCaptureProbe(audio_capture_probe::AudioCaptureProbeConfig),
    AudioSend(audio_udp::AudioSendConfig),
    AudioRecvPlay(audio_udp::AudioRecvPlayConfig),
    Help,
}

#[derive(Debug, Default)]
struct ReceiverStats {
    packets_received: u64,
    packets_dropped_or_lost_estimate: u64,
    frames_complete: u64,
    frames_incomplete_expired: u64,
    last_frame_id: u64,
}

#[derive(Debug)]
struct FrameAssembly {
    packet_count: u16,
    received: Vec<bool>,
    received_count: u16,
    first_seen: Instant,
    bytes: usize,
}

#[derive(Debug)]
struct Reassembler {
    frames: HashMap<u64, FrameAssembly>,
    ttl: Duration,
    stats: ReceiverStats,
}

impl Reassembler {
    fn new(ttl: Duration) -> Self {
        Self {
            frames: HashMap::new(),
            ttl,
            stats: ReceiverStats::default(),
        }
    }

    fn accept(&mut self, packet: MediaPacket, now: Instant) -> bool {
        self.stats.packets_received += 1;
        if packet.stream_id != STREAM_VIDEO || packet.packet_count == 0 {
            self.stats.packets_dropped_or_lost_estimate += 1;
            return false;
        }

        let entry = self
            .frames
            .entry(packet.frame_id)
            .or_insert_with(|| FrameAssembly {
                packet_count: packet.packet_count,
                received: vec![false; packet.packet_count as usize],
                received_count: 0,
                first_seen: now,
                bytes: 0,
            });

        if entry.packet_count != packet.packet_count {
            self.stats.packets_dropped_or_lost_estimate += 1;
            return false;
        }

        let index = packet.packet_index as usize;
        if index >= entry.received.len() {
            self.stats.packets_dropped_or_lost_estimate += 1;
            return false;
        }

        if !entry.received[index] {
            entry.received[index] = true;
            entry.received_count += 1;
            entry.bytes += packet.payload.len();
        }

        if entry.received_count == entry.packet_count {
            self.frames.remove(&packet.frame_id);
            self.stats.frames_complete += 1;
            self.stats.last_frame_id = packet.frame_id;
            return true;
        }
        false
    }

    fn expire(&mut self, now: Instant) {
        let ttl = self.ttl;
        let expired: Vec<u64> = self
            .frames
            .iter()
            .filter_map(|(frame_id, frame)| {
                if now.duration_since(frame.first_seen) > ttl {
                    Some(*frame_id)
                } else {
                    None
                }
            })
            .collect();

        for frame_id in expired {
            if let Some(frame) = self.frames.remove(&frame_id) {
                self.stats.frames_incomplete_expired += 1;
                self.stats.packets_dropped_or_lost_estimate +=
                    u64::from(frame.packet_count.saturating_sub(frame.received_count));
                self.stats.last_frame_id = frame_id;
            }
        }
    }
}

fn main() {
    match parse_args(env::args().skip(1).collect()) {
        Ok(Command::SelfTest) => {
            if let Err(err) = run_self_test() {
                eprintln!("self-test failed: {err}");
                println!(
                    r#"{{"type":"SELF_TEST","ok":false,"error":"{}"}}"#,
                    json_escape(&err)
                );
                process::exit(1);
            }
            println!("{}", self_test_success_json());
        }
        Ok(Command::Sender {
            host,
            port,
            fps,
            bitrate_mbps,
        }) => {
            if let Err(err) = run_sender(&host, port, fps, bitrate_mbps) {
                eprintln!("sender error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::Receiver { bind, port }) => {
            if let Err(err) = run_receiver(&bind, port) {
                eprintln!("receiver error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::CaptureProbe {
            duration_sec,
            target_fps,
        }) => {
            if let Err(err) = capture_probe::run(duration_sec, target_fps) {
                eprintln!("capture-probe error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::WmfProbe) => {
            if let Err(err) = wmf_probe::run() {
                eprintln!("wmf-probe error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::GpuConvertProbe(config)) => {
            if let Err(err) = gpu_convert_probe::run(config) {
                eprintln!("gpu-convert-probe error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::EncodeProbe(config)) => {
            if let Err(err) = encode_probe::run(config) {
                eprintln!("encode-probe error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::CaptureEncodeProbe(config)) => {
            if let Err(err) = capture_encode_probe::run(config) {
                eprintln!("capture-encode-probe error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::H264SendProbe(config)) => {
            if let Err(err) = h264_send_probe::run(config) {
                eprintln!("h264-send-probe error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::H264RecvDump(config)) => {
            if let Err(err) = h264_recv_dump::run(config) {
                eprintln!("h264-recv-dump error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::H264RecvView(config)) => {
            if let Err(err) = h264_recv_view::run(config) {
                eprintln!("h264-recv-view error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::ScreenSend(config)) => {
            if let Err(err) = h264_send_probe::run(config) {
                eprintln!("screen-send error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::ScreenRecv(config)) => {
            if let Err(err) = h264_recv_view::run(config) {
                eprintln!("screen-recv error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::H264FileViewer(config)) => {
            if let Err(err) = h264_file_viewer::run(config) {
                eprintln!("h264-file-viewer error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::ColorTestPattern(config)) => {
            if let Err(err) = color_test_pattern::run(config) {
                eprintln!("color-test-pattern error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::BenchReassembly(config)) => {
            if let Err(err) = bench_reassembly::run(config) {
                eprintln!("bench-reassembly error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::AudioCaptureProbe(config)) => {
            if let Err(err) = audio_capture_probe::run(config) {
                eprintln!("audio-capture-probe error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::AudioSend(config)) => {
            if let Err(err) = audio_udp::run_audio_send(config) {
                eprintln!("audio-send error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::AudioRecvPlay(config)) => {
            if let Err(err) = audio_udp::run_audio_recv_play(config) {
                eprintln!("audio-recv-play error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::Help) => {
            print_help();
        }
        Err(err) => {
            eprintln!("{err}");
            eprintln!();
            print_help();
            process::exit(2);
        }
    }
}

fn self_test_success_json() -> String {
    format!(
        concat!(
            r#"{{"type":"SELF_TEST","ok":true,"version":"{}","#,
            r#""packet_format":"AGM1","capabilities":{{"#,
            r#""screen_send":true,"screen_recv":true,"audio_send":true,"#,
            r#""audio_recv_play":true,"hardware_runtime_probe_required":true}}}}"#
        ),
        env!("CARGO_PKG_VERSION")
    )
}

fn parse_args(args: Vec<String>) -> Result<Command, String> {
    if args.is_empty() {
        return Ok(Command::Help);
    }

    match args[0].as_str() {
        "-h" | "--help" | "help" => Ok(Command::Help),
        "self-test" => {
            if args.len() == 1 {
                Ok(Command::SelfTest)
            } else {
                Err("self-test does not accept extra arguments".to_string())
            }
        }
        "sender" => parse_sender_args(&args[1..]),
        "receiver" => parse_receiver_args(&args[1..]),
        "capture-probe" => parse_capture_probe_args(&args[1..]),
        "wmf-probe" => {
            if args.len() == 1 {
                Ok(Command::WmfProbe)
            } else {
                Err("wmf-probe does not accept extra arguments".to_string())
            }
        }
        "gpu-convert-probe" => parse_gpu_convert_probe_args(&args[1..]),
        "encode-probe" => parse_encode_probe_args(&args[1..]),
        "capture-encode-probe" => parse_capture_encode_probe_args(&args[1..]),
        "h264-send-probe" => parse_h264_send_probe_args(&args[1..]),
        "h264-recv-dump" => parse_h264_recv_dump_args(&args[1..]),
        "h264-recv-view" => parse_h264_recv_view_args(&args[1..]),
        "screen-send" => parse_screen_send_args(&args[1..]),
        "screen-recv" => parse_screen_recv_args(&args[1..]),
        "h264-file-viewer" => parse_h264_file_viewer_args(&args[1..]),
        "color-test-pattern" => parse_color_test_pattern_args(&args[1..]),
        "bench-reassembly" => parse_bench_reassembly_args(&args[1..]),
        "audio-capture-probe" => parse_audio_capture_probe_args(&args[1..]),
        "audio-send" => parse_audio_send_args(&args[1..]),
        "audio-recv-play" => parse_audio_recv_play_args(&args[1..]),
        other => Err(format!("unknown command: {other}")),
    }
}

fn parse_sender_args(args: &[String]) -> Result<Command, String> {
    let mut host = "127.0.0.1".to_string();
    let mut port: u16 = 50120;
    let mut fps: u32 = 30;
    let mut bitrate_mbps: f64 = 4.0;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--host" => {
                i += 1;
                host = required_value(args, i, "--host")?.to_string();
            }
            "--port" => {
                i += 1;
                port = parse_port(required_value(args, i, "--port")?)?;
            }
            "--fps" => {
                i += 1;
                fps = parse_fps(required_value(args, i, "--fps")?)?;
            }
            "--bitrate-mbps" => {
                i += 1;
                bitrate_mbps = parse_bitrate(required_value(args, i, "--bitrate-mbps")?)?;
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown sender argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::Sender {
        host,
        port,
        fps,
        bitrate_mbps,
    })
}

fn parse_receiver_args(args: &[String]) -> Result<Command, String> {
    let mut bind = "0.0.0.0".to_string();
    let mut port: u16 = 50120;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--bind" => {
                i += 1;
                bind = required_value(args, i, "--bind")?.to_string();
            }
            "--port" => {
                i += 1;
                port = parse_port(required_value(args, i, "--port")?)?;
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown receiver argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::Receiver { bind, port })
}

fn parse_capture_probe_args(args: &[String]) -> Result<Command, String> {
    let mut duration_sec = 10u64;
    let mut target_fps = 30u32;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--duration-sec" => {
                i += 1;
                duration_sec = parse_duration_sec(required_value(args, i, "--duration-sec")?)?;
            }
            "--target-fps" => {
                i += 1;
                target_fps = parse_fps(required_value(args, i, "--target-fps")?)?;
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown capture-probe argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::CaptureProbe {
        duration_sec,
        target_fps,
    })
}

fn parse_gpu_convert_probe_args(args: &[String]) -> Result<Command, String> {
    let mut duration_sec = 5u64;
    let mut target_fps = 30u32;
    let mut out_width = 1280u32;
    let mut out_height = 720u32;
    let mut color_spec = color_spec::ColorSpec::default();
    let mut debug_dump_nv12 = None;
    let mut debug_dump_bgra = None;
    let mut debug_dump_limit = 3usize;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--duration-sec" => {
                i += 1;
                duration_sec = parse_duration_sec(required_value(args, i, "--duration-sec")?)?;
            }
            "--target-fps" => {
                i += 1;
                target_fps = parse_fps(required_value(args, i, "--target-fps")?)?;
            }
            "--out-width" => {
                i += 1;
                out_width = parse_dimension(required_value(args, i, "--out-width")?, "out-width")?;
            }
            "--out-height" => {
                i += 1;
                out_height =
                    parse_dimension(required_value(args, i, "--out-height")?, "out-height")?;
            }
            "--color-matrix" => {
                i += 1;
                color_spec = color_spec::ColorSpec::with_matrix(color_spec::ColorMatrix::parse(
                    required_value(args, i, "--color-matrix")?,
                )?);
            }
            "--debug-dump-nv12" => {
                i += 1;
                debug_dump_nv12 = Some(required_value(args, i, "--debug-dump-nv12")?.to_string());
            }
            "--debug-dump-bgra" => {
                i += 1;
                debug_dump_bgra = Some(required_value(args, i, "--debug-dump-bgra")?.to_string());
            }
            "--debug-dump-limit" => {
                i += 1;
                debug_dump_limit = parse_count(
                    required_value(args, i, "--debug-dump-limit")?,
                    "debug-dump-limit",
                    1,
                    100,
                )?;
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown gpu-convert-probe argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::GpuConvertProbe(
        gpu_convert_probe::GpuConvertProbeConfig {
            duration_sec,
            target_fps,
            out_width,
            out_height,
            color_spec,
            debug_dump_nv12,
            debug_dump_bgra,
            debug_dump_limit,
        },
    ))
}

fn parse_encode_probe_args(args: &[String]) -> Result<Command, String> {
    let mut width = 1280u32;
    let mut height = 720u32;
    let mut fps = 30u32;
    let mut duration_sec = 5u64;
    let mut bitrate_mbps = 4.0f64;
    let mut output = "synthetic_720p30.h264".to_string();
    let mut encoder = wmf_h264_encoder::EncoderChoice::Auto;
    let mut color_spec = color_spec::ColorSpec::default();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--width" => {
                i += 1;
                width = parse_dimension(required_value(args, i, "--width")?, "width")?;
            }
            "--height" => {
                i += 1;
                height = parse_dimension(required_value(args, i, "--height")?, "height")?;
            }
            "--fps" => {
                i += 1;
                fps = parse_fps(required_value(args, i, "--fps")?)?;
            }
            "--duration-sec" => {
                i += 1;
                duration_sec = parse_duration_sec(required_value(args, i, "--duration-sec")?)?;
            }
            "--bitrate-mbps" => {
                i += 1;
                bitrate_mbps = parse_bitrate(required_value(args, i, "--bitrate-mbps")?)?;
            }
            "--output" => {
                i += 1;
                output = required_value(args, i, "--output")?.to_string();
                if output.trim().is_empty() {
                    return Err("output path must not be empty".to_string());
                }
            }
            "--encoder" => {
                i += 1;
                encoder = parse_encoder_choice(required_value(args, i, "--encoder")?)?;
            }
            "--color-matrix" => {
                i += 1;
                color_spec = color_spec::ColorSpec::with_matrix(color_spec::ColorMatrix::parse(
                    required_value(args, i, "--color-matrix")?,
                )?);
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown encode-probe argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::EncodeProbe(encode_probe::EncodeProbeConfig {
        width,
        height,
        fps,
        duration_sec,
        bitrate_mbps,
        output,
        encoder,
        color_spec,
    }))
}

fn parse_capture_encode_probe_args(args: &[String]) -> Result<Command, String> {
    let mut duration_sec = 5u64;
    let mut target_fps = 30u32;
    let mut explicit_bitrate_mbps = None;
    let mut quality_bpf = None;
    let mut out_width = 1280u32;
    let mut out_height = 720u32;
    let mut output = "capture_720p30.h264".to_string();
    let mut color_spec = color_spec::ColorSpec::default();
    let mut encoder = wmf_h264_encoder::EncoderChoice::Auto;
    let mut convert_backend = capture_encode_probe::ConvertBackend::Auto;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--duration-sec" => {
                i += 1;
                duration_sec = parse_duration_sec(required_value(args, i, "--duration-sec")?)?;
            }
            "--target-fps" => {
                i += 1;
                target_fps = parse_fps(required_value(args, i, "--target-fps")?)?;
            }
            "--bitrate-mbps" => {
                i += 1;
                explicit_bitrate_mbps =
                    Some(parse_bitrate(required_value(args, i, "--bitrate-mbps")?)?);
            }
            "--quality-bpf" => {
                i += 1;
                quality_bpf = Some(parse_quality_bpf(required_value(
                    args,
                    i,
                    "--quality-bpf",
                )?)?);
            }
            "--out-width" => {
                i += 1;
                out_width = parse_dimension(required_value(args, i, "--out-width")?, "out-width")?;
            }
            "--out-height" => {
                i += 1;
                out_height =
                    parse_dimension(required_value(args, i, "--out-height")?, "out-height")?;
            }
            "--output" => {
                i += 1;
                output = required_value(args, i, "--output")?.to_string();
                if output.trim().is_empty() {
                    return Err("output path must not be empty".to_string());
                }
            }
            "--encoder" => {
                i += 1;
                encoder = parse_encoder_choice(required_value(args, i, "--encoder")?)?;
            }
            "--convert-backend" => {
                i += 1;
                convert_backend =
                    parse_convert_backend(required_value(args, i, "--convert-backend")?)?;
            }
            "--color-matrix" => {
                i += 1;
                color_spec = color_spec::ColorSpec::with_matrix(color_spec::ColorMatrix::parse(
                    required_value(args, i, "--color-matrix")?,
                )?);
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown capture-encode-probe argument: {other}")),
        }
        i += 1;
    }

    let bitrate_selection = bitrate::BitrateSelection::resolve(
        out_width,
        out_height,
        target_fps,
        4.0,
        explicit_bitrate_mbps,
        quality_bpf,
    )?;
    let bitrate_mbps = bitrate_selection.target_mbps;

    Ok(Command::CaptureEncodeProbe(
        capture_encode_probe::CaptureEncodeConfig {
            duration_sec: Some(duration_sec),
            target_fps,
            bitrate_mbps,
            bitrate_selection,
            out_width,
            out_height,
            output,
            color_spec,
            encoder,
            convert_backend,
            keyframe_interval_sec: None,
            verbose: true,
        },
    ))
}

fn parse_h264_send_probe_args(args: &[String]) -> Result<Command, String> {
    let mut host = "127.0.0.1".to_string();
    let mut port = 50130u16;
    let mut duration_sec = 10u64;
    let mut target_fps = 30u32;
    let mut explicit_bitrate_mbps = None;
    let mut quality_bpf = None;
    let mut out_width = 1280u32;
    let mut out_height = 720u32;
    let mut color_spec = color_spec::ColorSpec::default();
    let mut encoder = wmf_h264_encoder::EncoderChoice::Auto;
    let mut convert_backend = capture_encode_probe::ConvertBackend::Auto;
    let mut packet_pacing = h264_send_probe::PacketPacing::Auto;
    let mut fec_mode = fec::FecMode::Off;
    let mut udp_payload_size = DEFAULT_REALTIME_UDP_PAYLOAD_SIZE;
    let mut keyframe_interval_sec = 1.0f64;
    let mut repair_mode = repair::RepairMode::Off;
    let mut repair_cache_ms = 3000u64;
    let mut adaptive = h264_send_probe::AdaptiveRuntimeConfig::default();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--host" => {
                i += 1;
                host = required_value(args, i, "--host")?.to_string();
            }
            "--port" => {
                i += 1;
                port = parse_port(required_value(args, i, "--port")?)?;
            }
            "--duration-sec" => {
                i += 1;
                duration_sec = parse_duration_sec(required_value(args, i, "--duration-sec")?)?;
            }
            "--target-fps" => {
                i += 1;
                target_fps = parse_fps(required_value(args, i, "--target-fps")?)?;
            }
            "--bitrate-mbps" => {
                i += 1;
                explicit_bitrate_mbps =
                    Some(parse_bitrate(required_value(args, i, "--bitrate-mbps")?)?);
            }
            "--quality-bpf" => {
                i += 1;
                quality_bpf = Some(parse_quality_bpf(required_value(
                    args,
                    i,
                    "--quality-bpf",
                )?)?);
            }
            "--out-width" => {
                i += 1;
                out_width = parse_dimension(required_value(args, i, "--out-width")?, "out-width")?;
            }
            "--out-height" => {
                i += 1;
                out_height =
                    parse_dimension(required_value(args, i, "--out-height")?, "out-height")?;
            }
            "--color-matrix" => {
                i += 1;
                color_spec = color_spec::ColorSpec::with_matrix(color_spec::ColorMatrix::parse(
                    required_value(args, i, "--color-matrix")?,
                )?);
            }
            "--encoder" => {
                i += 1;
                encoder = parse_encoder_choice(required_value(args, i, "--encoder")?)?;
            }
            "--convert-backend" => {
                i += 1;
                convert_backend =
                    parse_convert_backend(required_value(args, i, "--convert-backend")?)?;
            }
            "--packet-pacing" => {
                i += 1;
                packet_pacing = parse_packet_pacing(required_value(args, i, "--packet-pacing")?)?;
            }
            "--fec" => {
                i += 1;
                fec_mode = parse_fec_mode(required_value(args, i, "--fec")?)?;
            }
            "--udp-payload-size" => {
                i += 1;
                udp_payload_size =
                    parse_udp_payload_size(required_value(args, i, "--udp-payload-size")?)?;
            }
            "--keyframe-interval-sec" => {
                i += 1;
                keyframe_interval_sec = parse_keyframe_interval_sec(required_value(
                    args,
                    i,
                    "--keyframe-interval-sec",
                )?)?;
            }
            "--repair" => {
                i += 1;
                repair_mode = repair::RepairMode::parse(required_value(args, i, "--repair")?)?;
            }
            "--repair-cache-ms" => {
                i += 1;
                repair_cache_ms = parse_milliseconds(
                    required_value(args, i, "--repair-cache-ms")?,
                    "repair-cache-ms",
                    500,
                    10_000,
                )?;
            }
            "-h" | "--help" => return Ok(Command::Help),
            other if parse_adaptive_sender_option(other, args, &mut i, &mut adaptive)? => {}
            other => return Err(format!("unknown h264-send-probe argument: {other}")),
        }
        i += 1;
    }

    let bitrate_selection = bitrate::BitrateSelection::resolve(
        out_width,
        out_height,
        target_fps,
        4.0,
        explicit_bitrate_mbps,
        quality_bpf,
    )?;
    let bitrate_mbps = bitrate_selection.target_mbps;

    Ok(Command::H264SendProbe(h264_send_probe::H264SendConfig {
        host,
        port,
        duration_sec: Some(duration_sec),
        target_fps,
        bitrate_mbps,
        bitrate_selection,
        out_width,
        out_height,
        color_spec,
        encoder,
        convert_backend,
        packet_pacing,
        fec_mode,
        udp_payload_size,
        keyframe_interval_sec,
        repair_mode,
        repair_cache_ms,
        audio_mode: h264_send_probe::AudioSendMode::Off,
        adaptive,
        mode: h264_send_probe::H264SendMode::Probe,
        verbose: true,
    }))
}

fn parse_bench_reassembly_args(args: &[String]) -> Result<Command, String> {
    let mut frames = 1800u64;
    let mut packets_per_frame = 120u16;
    let mut payload_size = 1414usize;
    let mut loss_rate = 0.0f64;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--frames" => {
                i += 1;
                frames = required_value(args, i, "--frames")?
                    .parse()
                    .map_err(|_| "frames must be a positive integer".to_string())?;
                if frames == 0 {
                    return Err("frames must be greater than zero".to_string());
                }
            }
            "--packets-per-frame" => {
                i += 1;
                packets_per_frame = required_value(args, i, "--packets-per-frame")?
                    .parse()
                    .map_err(|_| "packets-per-frame must be between 1 and 65535".to_string())?;
                if packets_per_frame == 0 {
                    return Err("packets-per-frame must be greater than zero".to_string());
                }
            }
            "--payload-size" => {
                i += 1;
                payload_size = required_value(args, i, "--payload-size")?
                    .parse()
                    .map_err(|_| "payload-size must be a positive integer".to_string())?;
                if payload_size == 0 || payload_size > MAX_MEDIA_PAYLOAD {
                    return Err(format!(
                        "payload-size must be between 1 and {MAX_MEDIA_PAYLOAD}"
                    ));
                }
            }
            "--loss-rate" => {
                i += 1;
                loss_rate = required_value(args, i, "--loss-rate")?
                    .parse()
                    .map_err(|_| "loss-rate must be between 0 and 1".to_string())?;
                if !loss_rate.is_finite() || !(0.0..=1.0).contains(&loss_rate) {
                    return Err("loss-rate must be between 0 and 1".to_string());
                }
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown bench-reassembly argument: {other}")),
        }
        i += 1;
    }
    Ok(Command::BenchReassembly(
        bench_reassembly::BenchReassemblyConfig {
            frames,
            packets_per_frame,
            payload_size,
            loss_rate,
        },
    ))
}

fn parse_audio_capture_probe_args(args: &[String]) -> Result<Command, String> {
    let mut duration_sec = 10u64;
    let mut output = PathBuf::from("audio_probe.pcm");
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--duration-sec" => {
                i += 1;
                duration_sec = parse_duration_sec(required_value(args, i, "--duration-sec")?)?;
            }
            "--output" => {
                i += 1;
                output = PathBuf::from(required_value(args, i, "--output")?);
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown audio-capture-probe argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::AudioCaptureProbe(
        audio_capture_probe::AudioCaptureProbeConfig {
            duration_sec,
            output,
        },
    ))
}

fn parse_audio_send_args(args: &[String]) -> Result<Command, String> {
    let mut host: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut duration_sec = None;
    let mut frame_ms = 10u32;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--host" => {
                i += 1;
                host = Some(required_value(args, i, "--host")?.to_string());
            }
            "--port" => {
                i += 1;
                port = Some(parse_port(required_value(args, i, "--port")?)?);
            }
            "--duration-sec" => {
                i += 1;
                duration_sec = Some(parse_duration_sec(required_value(
                    args,
                    i,
                    "--duration-sec",
                )?)?);
            }
            "--frame-ms" => {
                i += 1;
                frame_ms = parse_audio_frame_ms(required_value(args, i, "--frame-ms")?)?;
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown audio-send argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::AudioSend(audio_udp::AudioSendConfig {
        host: host.ok_or("audio-send requires --host")?,
        port: port.ok_or("audio-send requires --port")?,
        duration_sec,
        frame_ms,
    }))
}

fn parse_audio_recv_play_args(args: &[String]) -> Result<Command, String> {
    let mut bind = "0.0.0.0".to_string();
    let mut port: Option<u16> = None;
    let mut duration_sec = None;
    let mut jitter_buffer_ms = 120u32;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--bind" => {
                i += 1;
                bind = required_value(args, i, "--bind")?.to_string();
            }
            "--port" => {
                i += 1;
                port = Some(parse_port(required_value(args, i, "--port")?)?);
            }
            "--duration-sec" => {
                i += 1;
                duration_sec = Some(parse_duration_sec(required_value(
                    args,
                    i,
                    "--duration-sec",
                )?)?);
            }
            "--jitter-buffer-ms" => {
                i += 1;
                jitter_buffer_ms =
                    parse_audio_jitter_buffer_ms(required_value(args, i, "--jitter-buffer-ms")?)?;
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown audio-recv-play argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::AudioRecvPlay(audio_udp::AudioRecvPlayConfig {
        bind,
        port: port.ok_or("audio-recv-play requires --port")?,
        duration_sec,
        jitter_buffer_ms,
    }))
}

fn parse_color_test_pattern_args(args: &[String]) -> Result<Command, String> {
    let mut output = "color_test_1080p.h264".to_string();
    let mut width = 1920u32;
    let mut height = 1080u32;
    let mut fps = 30u32;
    let mut duration_sec = 3u64;
    let mut bitrate_mbps = 8.0f64;
    let mut color_spec = color_spec::ColorSpec::default();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--output" => {
                i += 1;
                output = required_value(args, i, "--output")?.to_string();
            }
            "--width" => {
                i += 1;
                width = parse_dimension(required_value(args, i, "--width")?, "width")?;
            }
            "--height" => {
                i += 1;
                height = parse_dimension(required_value(args, i, "--height")?, "height")?;
            }
            "--fps" => {
                i += 1;
                fps = parse_fps(required_value(args, i, "--fps")?)?;
            }
            "--duration-sec" => {
                i += 1;
                duration_sec = parse_duration_sec(required_value(args, i, "--duration-sec")?)?;
            }
            "--bitrate-mbps" => {
                i += 1;
                bitrate_mbps = parse_bitrate(required_value(args, i, "--bitrate-mbps")?)?;
            }
            "--color-matrix" => {
                i += 1;
                color_spec = color_spec::ColorSpec::with_matrix(color_spec::ColorMatrix::parse(
                    required_value(args, i, "--color-matrix")?,
                )?);
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown color-test-pattern argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::ColorTestPattern(
        color_test_pattern::ColorTestPatternConfig {
            output,
            width,
            height,
            fps,
            duration_sec,
            bitrate_mbps,
            color_spec,
        },
    ))
}

fn parse_h264_recv_dump_args(args: &[String]) -> Result<Command, String> {
    let mut bind = "0.0.0.0".to_string();
    let mut port = 50130u16;
    let mut output = "received_capture.h264".to_string();
    let mut idle_timeout_sec = 3u64;
    let mut drop_damaged_gop = true;
    let mut reorder_wait_ms = None;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--bind" => {
                i += 1;
                bind = required_value(args, i, "--bind")?.to_string();
            }
            "--port" => {
                i += 1;
                port = parse_port(required_value(args, i, "--port")?)?;
            }
            "--output" => {
                i += 1;
                output = required_value(args, i, "--output")?.to_string();
                if output.trim().is_empty() {
                    return Err("output path must not be empty".to_string());
                }
            }
            "--idle-timeout-sec" => {
                i += 1;
                idle_timeout_sec =
                    parse_idle_timeout(required_value(args, i, "--idle-timeout-sec")?)?;
            }
            "--drop-damaged-gop" => {
                i += 1;
                drop_damaged_gop = parse_bool(required_value(args, i, "--drop-damaged-gop")?)?;
            }
            "--reorder-wait-ms" => {
                i += 1;
                reorder_wait_ms =
                    parse_reorder_wait_ms(required_value(args, i, "--reorder-wait-ms")?)?;
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown h264-recv-dump argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::H264RecvDump(h264_recv_dump::H264RecvConfig {
        bind,
        port,
        output,
        idle_timeout_sec,
        drop_damaged_gop,
        reorder_wait_ms,
    }))
}

fn parse_h264_file_viewer_args(args: &[String]) -> Result<Command, String> {
    let mut input = String::new();
    let mut render_scale = win32_gdi_viewer::RenderScaleMode::Exact;
    let mut window_mode = win32_gdi_viewer::WindowMode::Windowed;
    let mut render_backend = video_renderer::RenderBackend::Gdi;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--input" => {
                i += 1;
                input = required_value(args, i, "--input")?.to_string();
            }
            "--render-scale" => {
                i += 1;
                render_scale = win32_gdi_viewer::RenderScaleMode::parse(required_value(
                    args,
                    i,
                    "--render-scale",
                )?)?;
            }
            "--window-mode" => {
                i += 1;
                window_mode =
                    win32_gdi_viewer::WindowMode::parse(required_value(args, i, "--window-mode")?)?;
            }
            "--render-backend" => {
                i += 1;
                render_backend = video_renderer::RenderBackend::parse(required_value(
                    args,
                    i,
                    "--render-backend",
                )?)?;
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown h264-file-viewer argument: {other}")),
        }
        i += 1;
    }
    if input.trim().is_empty() {
        return Err("h264-file-viewer requires --input <path>".to_string());
    }
    Ok(Command::H264FileViewer(
        h264_file_viewer::H264FileViewerConfig {
            input,
            render_scale,
            window_mode,
            render_backend,
        },
    ))
}

fn parse_h264_recv_view_args(args: &[String]) -> Result<Command, String> {
    let mut bind = "0.0.0.0".to_string();
    let mut port = None;
    let mut decoder_fps = 30u32;
    let mut frame_timeout_ms = 300u64;
    let mut max_inflight_frames = 120usize;
    let mut max_decode_queue = 30usize;
    let mut strict_decode_order = true;
    let mut drop_damaged_gop = true;
    let mut reorder_wait_ms = None;
    let mut playout_delay_ms = 120u64;
    let mut debug_dump_frames = None;
    let mut debug_dump_limit = 10usize;
    let mut json_interval_ms = 1000u64;
    let mut title = "AgoraLink Native Viewer".to_string();
    let mut render_scale = win32_gdi_viewer::RenderScaleMode::Exact;
    let mut window_mode = win32_gdi_viewer::WindowMode::Windowed;
    let mut render_backend = video_renderer::RenderBackend::D3d11;
    let mut repair_mode = repair::RepairMode::Off;
    let mut nack_delay_ms = 20u64;
    let mut nack_repeat_ms = 20u64;
    let mut nack_max_rounds = 3u8;
    let mut display_refresh_detect = display_capability::DisplayRefreshDetect::Auto;
    let mut capability_feedback_ms = 1000u64;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--bind" => {
                i += 1;
                bind = required_value(args, i, "--bind")?.to_string();
            }
            "--port" => {
                i += 1;
                port = Some(parse_port(required_value(args, i, "--port")?)?);
            }
            "--fps" => {
                i += 1;
                decoder_fps = parse_fps(required_value(args, i, "--fps")?)?;
            }
            "--frame-timeout-ms" => {
                i += 1;
                frame_timeout_ms = parse_milliseconds(
                    required_value(args, i, "--frame-timeout-ms")?,
                    "frame-timeout-ms",
                    1,
                    60_000,
                )?;
            }
            "--max-inflight-frames" => {
                i += 1;
                max_inflight_frames = parse_count(
                    required_value(args, i, "--max-inflight-frames")?,
                    "max-inflight-frames",
                    1,
                    10_000,
                )?;
            }
            "--max-decode-queue" => {
                i += 1;
                max_decode_queue = parse_count(
                    required_value(args, i, "--max-decode-queue")?,
                    "max-decode-queue",
                    1,
                    1000,
                )?;
            }
            "--strict-decode-order" => {
                i += 1;
                strict_decode_order =
                    parse_bool(required_value(args, i, "--strict-decode-order")?)?;
            }
            "--drop-damaged-gop" => {
                i += 1;
                drop_damaged_gop = parse_bool(required_value(args, i, "--drop-damaged-gop")?)?;
            }
            "--reorder-wait-ms" => {
                i += 1;
                reorder_wait_ms =
                    parse_reorder_wait_ms(required_value(args, i, "--reorder-wait-ms")?)?;
            }
            "--playout-delay-ms" => {
                i += 1;
                playout_delay_ms = parse_milliseconds(
                    required_value(args, i, "--playout-delay-ms")?,
                    "playout-delay-ms",
                    0,
                    500,
                )?;
            }
            "--debug-dump-frames" => {
                i += 1;
                debug_dump_frames =
                    Some(required_value(args, i, "--debug-dump-frames")?.to_string());
            }
            "--debug-dump-limit" => {
                i += 1;
                debug_dump_limit = parse_count(
                    required_value(args, i, "--debug-dump-limit")?,
                    "debug-dump-limit",
                    1,
                    10_000,
                )?;
            }
            "--json-interval-ms" => {
                i += 1;
                json_interval_ms = parse_milliseconds(
                    required_value(args, i, "--json-interval-ms")?,
                    "json-interval-ms",
                    100,
                    60_000,
                )?;
            }
            "--title" => {
                i += 1;
                title = required_value(args, i, "--title")?.to_string();
                if title.trim().is_empty() {
                    return Err("title must not be empty".to_string());
                }
            }
            "--render-scale" => {
                i += 1;
                render_scale = win32_gdi_viewer::RenderScaleMode::parse(required_value(
                    args,
                    i,
                    "--render-scale",
                )?)?;
            }
            "--window-mode" => {
                i += 1;
                window_mode =
                    win32_gdi_viewer::WindowMode::parse(required_value(args, i, "--window-mode")?)?;
            }
            "--render-backend" => {
                i += 1;
                render_backend = video_renderer::RenderBackend::parse(required_value(
                    args,
                    i,
                    "--render-backend",
                )?)?;
            }
            "--repair" => {
                i += 1;
                repair_mode = repair::RepairMode::parse(required_value(args, i, "--repair")?)?;
            }
            "--nack-delay-ms" => {
                i += 1;
                nack_delay_ms = parse_milliseconds(
                    required_value(args, i, "--nack-delay-ms")?,
                    "nack-delay-ms",
                    1,
                    50,
                )?;
            }
            "--nack-repeat-ms" => {
                i += 1;
                nack_repeat_ms = parse_milliseconds(
                    required_value(args, i, "--nack-repeat-ms")?,
                    "nack-repeat-ms",
                    1,
                    50,
                )?;
            }
            "--nack-max-rounds" => {
                i += 1;
                nack_max_rounds = parse_count(
                    required_value(args, i, "--nack-max-rounds")?,
                    "nack-max-rounds",
                    1,
                    10,
                )? as u8;
            }
            "-h" | "--help" => return Ok(Command::Help),
            "--display-refresh-detect" => {
                i += 1;
                display_refresh_detect = display_capability::DisplayRefreshDetect::parse(
                    required_value(args, i, "--display-refresh-detect")?,
                )?;
            }
            "--adaptive-feedback-ms" => {
                i += 1;
                capability_feedback_ms = parse_milliseconds(
                    required_value(args, i, "--adaptive-feedback-ms")?,
                    "adaptive-feedback-ms",
                    500,
                    2000,
                )?;
            }
            other => return Err(format!("unknown h264-recv-view argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::H264RecvView(h264_recv_view::H264RecvViewConfig {
        bind,
        port: port.ok_or_else(|| "h264-recv-view requires --port <port>".to_string())?,
        decoder_fps,
        duration_sec: None,
        frame_timeout_ms,
        reorder_wait_ms,
        playout_delay_ms,
        max_inflight_frames,
        max_decode_queue,
        strict_decode_order,
        drop_damaged_gop,
        debug_dump_frames,
        debug_dump_limit,
        json_interval_ms,
        title,
        render_scale,
        window_mode,
        render_backend,
        repair_mode,
        nack_delay_ms,
        nack_repeat_ms,
        nack_max_rounds,
        audio_mode: h264_recv_view::AudioRecvMode::Off,
        audio_jitter_buffer_ms: 120,
        av_sync_mode: av_sync::AvSyncMode::Off,
        display_refresh_detect,
        capability_feedback_ms,
        mode: h264_recv_view::H264RecvViewMode::Probe,
        verbose: true,
    }))
}

fn parse_screen_send_args(args: &[String]) -> Result<Command, String> {
    let mut host = None;
    let mut port = None;
    let mut duration_sec = None;
    let mut target_fps = 60u32;
    let mut explicit_bitrate_mbps = None;
    let mut quality_bpf = None;
    let mut out_width = 1920u32;
    let mut out_height = 1080u32;
    let mut color_spec = color_spec::ColorSpec::default();
    let mut encoder = wmf_h264_encoder::EncoderChoice::Auto;
    let mut convert_backend = capture_encode_probe::ConvertBackend::Auto;
    let mut packet_pacing = h264_send_probe::PacketPacing::Auto;
    let mut fec_mode = fec::FecMode::Off;
    let mut udp_payload_size = DEFAULT_REALTIME_UDP_PAYLOAD_SIZE;
    let mut keyframe_interval_sec = 1.0f64;
    let mut repair_mode = repair::RepairMode::Nack;
    let mut repair_cache_ms = 3000u64;
    let mut audio_mode = h264_send_probe::AudioSendMode::Off;
    let mut adaptive = h264_send_probe::AdaptiveRuntimeConfig::default();
    let mut verbose = false;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--host" => {
                i += 1;
                host = Some(required_value(args, i, "--host")?.to_string());
            }
            "--port" => {
                i += 1;
                port = Some(parse_port(required_value(args, i, "--port")?)?);
            }
            "--duration-sec" => {
                i += 1;
                duration_sec = Some(parse_duration_sec(required_value(
                    args,
                    i,
                    "--duration-sec",
                )?)?);
            }
            "--fps" | "--target-fps" => {
                i += 1;
                target_fps = parse_fps(required_value(args, i, args[i - 1].as_str())?)?;
            }
            "--bitrate-mbps" => {
                i += 1;
                explicit_bitrate_mbps =
                    Some(parse_bitrate(required_value(args, i, "--bitrate-mbps")?)?);
            }
            "--quality-bpf" => {
                i += 1;
                quality_bpf = Some(parse_quality_bpf(required_value(
                    args,
                    i,
                    "--quality-bpf",
                )?)?);
            }
            "--width" | "--out-width" => {
                i += 1;
                out_width =
                    parse_dimension(required_value(args, i, args[i - 1].as_str())?, "width")?;
            }
            "--height" | "--out-height" => {
                i += 1;
                out_height =
                    parse_dimension(required_value(args, i, args[i - 1].as_str())?, "height")?;
            }
            "--color-matrix" => {
                i += 1;
                color_spec = color_spec::ColorSpec::with_matrix(color_spec::ColorMatrix::parse(
                    required_value(args, i, "--color-matrix")?,
                )?);
            }
            "--encoder" => {
                i += 1;
                encoder = parse_encoder_choice(required_value(args, i, "--encoder")?)?;
            }
            "--convert-backend" => {
                i += 1;
                convert_backend =
                    parse_convert_backend(required_value(args, i, "--convert-backend")?)?;
            }
            "--packet-pacing" => {
                i += 1;
                packet_pacing = parse_packet_pacing(required_value(args, i, "--packet-pacing")?)?;
            }
            "--fec" => {
                i += 1;
                fec_mode = parse_fec_mode(required_value(args, i, "--fec")?)?;
            }
            "--udp-payload-size" | "--payload-size" => {
                i += 1;
                udp_payload_size =
                    parse_udp_payload_size(required_value(args, i, args[i - 1].as_str())?)?;
            }
            "--keyframe-interval-sec" => {
                i += 1;
                keyframe_interval_sec = parse_keyframe_interval_sec(required_value(
                    args,
                    i,
                    "--keyframe-interval-sec",
                )?)?;
            }
            "--repair" => {
                i += 1;
                repair_mode = repair::RepairMode::parse(required_value(args, i, "--repair")?)?;
            }
            "--repair-cache-ms" => {
                i += 1;
                repair_cache_ms = parse_milliseconds(
                    required_value(args, i, "--repair-cache-ms")?,
                    "repair-cache-ms",
                    500,
                    10_000,
                )?;
            }
            "--audio" => {
                i += 1;
                audio_mode =
                    h264_send_probe::AudioSendMode::parse(required_value(args, i, "--audio")?)?;
            }
            "--verbose" => {
                verbose = true;
            }
            "-h" | "--help" => return Ok(Command::Help),
            other if parse_adaptive_sender_option(other, args, &mut i, &mut adaptive)? => {}
            other => return Err(format!("unknown screen-send argument: {other}")),
        }
        i += 1;
    }

    let bitrate_selection = bitrate::BitrateSelection::resolve(
        out_width,
        out_height,
        target_fps,
        22.0,
        explicit_bitrate_mbps,
        quality_bpf,
    )?;
    let bitrate_mbps = bitrate_selection.target_mbps;

    Ok(Command::ScreenSend(h264_send_probe::H264SendConfig {
        host: host.ok_or_else(|| "screen-send requires --host <ip>".to_string())?,
        port: port.ok_or_else(|| "screen-send requires --port <port>".to_string())?,
        duration_sec,
        target_fps,
        bitrate_mbps,
        bitrate_selection,
        out_width,
        out_height,
        color_spec,
        encoder,
        convert_backend,
        packet_pacing,
        fec_mode,
        udp_payload_size,
        keyframe_interval_sec,
        repair_mode,
        repair_cache_ms,
        audio_mode,
        adaptive,
        mode: h264_send_probe::H264SendMode::Screen,
        verbose,
    }))
}

fn parse_screen_recv_args(args: &[String]) -> Result<Command, String> {
    let mut bind = "0.0.0.0".to_string();
    let mut port = None;
    let mut decoder_fps = 30u32;
    let mut duration_sec = None;
    let mut frame_timeout_ms = 300u64;
    let mut max_inflight_frames = 120usize;
    let mut max_decode_queue = 30usize;
    let mut strict_decode_order = true;
    let mut drop_damaged_gop = true;
    let mut reorder_wait_ms = None;
    let mut playout_delay_ms = 120u64;
    let mut playout_delay_explicit = false;
    let mut debug_dump_frames = None;
    let mut debug_dump_limit = 10usize;
    let mut json_interval_ms = 1000u64;
    let mut title = "AgoraLink Native Viewer".to_string();
    let mut render_scale = win32_gdi_viewer::RenderScaleMode::Exact;
    let mut window_mode = win32_gdi_viewer::WindowMode::Windowed;
    let mut render_backend = video_renderer::RenderBackend::D3d11;
    let mut repair_mode = repair::RepairMode::Off;
    let mut nack_delay_ms = 20u64;
    let mut nack_repeat_ms = 20u64;
    let mut nack_max_rounds = 3u8;
    let mut audio_mode = h264_recv_view::AudioRecvMode::Off;
    let mut audio_jitter_buffer_ms = 120u32;
    let mut requested_av_sync_mode = None;
    let mut display_refresh_detect = display_capability::DisplayRefreshDetect::Auto;
    let mut capability_feedback_ms = 1000u64;
    let mut verbose = false;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--bind" => {
                i += 1;
                bind = required_value(args, i, "--bind")?.to_string();
            }
            "--port" => {
                i += 1;
                port = Some(parse_port(required_value(args, i, "--port")?)?);
            }
            "--fps" => {
                i += 1;
                decoder_fps = parse_fps(required_value(args, i, "--fps")?)?;
            }
            "--duration-sec" => {
                i += 1;
                duration_sec = Some(parse_duration_sec(required_value(
                    args,
                    i,
                    "--duration-sec",
                )?)?);
            }
            "--frame-timeout-ms" => {
                i += 1;
                frame_timeout_ms = parse_milliseconds(
                    required_value(args, i, "--frame-timeout-ms")?,
                    "frame-timeout-ms",
                    1,
                    60_000,
                )?;
            }
            "--max-inflight-frames" => {
                i += 1;
                max_inflight_frames = parse_count(
                    required_value(args, i, "--max-inflight-frames")?,
                    "max-inflight-frames",
                    1,
                    10_000,
                )?;
            }
            "--max-decode-queue" => {
                i += 1;
                max_decode_queue = parse_count(
                    required_value(args, i, "--max-decode-queue")?,
                    "max-decode-queue",
                    1,
                    1000,
                )?;
            }
            "--strict-decode-order" => {
                i += 1;
                strict_decode_order =
                    parse_bool(required_value(args, i, "--strict-decode-order")?)?;
            }
            "--drop-damaged-gop" => {
                i += 1;
                drop_damaged_gop = parse_bool(required_value(args, i, "--drop-damaged-gop")?)?;
            }
            "--reorder-wait-ms" => {
                i += 1;
                reorder_wait_ms =
                    parse_reorder_wait_ms(required_value(args, i, "--reorder-wait-ms")?)?;
            }
            "--playout-delay-ms" => {
                i += 1;
                playout_delay_ms = parse_milliseconds(
                    required_value(args, i, "--playout-delay-ms")?,
                    "playout-delay-ms",
                    0,
                    500,
                )?;
                playout_delay_explicit = true;
            }
            "--debug-dump-frames" => {
                i += 1;
                debug_dump_frames =
                    Some(required_value(args, i, "--debug-dump-frames")?.to_string());
            }
            "--debug-dump-limit" => {
                i += 1;
                debug_dump_limit = parse_count(
                    required_value(args, i, "--debug-dump-limit")?,
                    "debug-dump-limit",
                    1,
                    10_000,
                )?;
            }
            "--json-interval-ms" => {
                i += 1;
                json_interval_ms = parse_milliseconds(
                    required_value(args, i, "--json-interval-ms")?,
                    "json-interval-ms",
                    100,
                    60_000,
                )?;
            }
            "--title" => {
                i += 1;
                title = required_value(args, i, "--title")?.to_string();
                if title.trim().is_empty() {
                    return Err("title must not be empty".to_string());
                }
            }
            "--render-scale" => {
                i += 1;
                render_scale = win32_gdi_viewer::RenderScaleMode::parse(required_value(
                    args,
                    i,
                    "--render-scale",
                )?)?;
            }
            "--window-mode" => {
                i += 1;
                window_mode =
                    win32_gdi_viewer::WindowMode::parse(required_value(args, i, "--window-mode")?)?;
            }
            "--render-backend" => {
                i += 1;
                render_backend = video_renderer::RenderBackend::parse(required_value(
                    args,
                    i,
                    "--render-backend",
                )?)?;
            }
            "--repair" => {
                i += 1;
                repair_mode = repair::RepairMode::parse(required_value(args, i, "--repair")?)?;
            }
            "--nack-delay-ms" => {
                i += 1;
                nack_delay_ms = parse_milliseconds(
                    required_value(args, i, "--nack-delay-ms")?,
                    "nack-delay-ms",
                    1,
                    50,
                )?;
            }
            "--nack-repeat-ms" => {
                i += 1;
                nack_repeat_ms = parse_milliseconds(
                    required_value(args, i, "--nack-repeat-ms")?,
                    "nack-repeat-ms",
                    1,
                    50,
                )?;
            }
            "--nack-max-rounds" => {
                i += 1;
                nack_max_rounds = parse_count(
                    required_value(args, i, "--nack-max-rounds")?,
                    "nack-max-rounds",
                    1,
                    10,
                )? as u8;
            }
            "--audio" => {
                i += 1;
                audio_mode =
                    h264_recv_view::AudioRecvMode::parse(required_value(args, i, "--audio")?)?;
            }
            "--audio-jitter-buffer-ms" => {
                i += 1;
                audio_jitter_buffer_ms = parse_audio_jitter_buffer_ms(required_value(
                    args,
                    i,
                    "--audio-jitter-buffer-ms",
                )?)?;
            }
            "--av-sync" => {
                i += 1;
                requested_av_sync_mode = Some(av_sync::AvSyncMode::parse(required_value(
                    args,
                    i,
                    "--av-sync",
                )?)?);
            }
            "--display-refresh-detect" => {
                i += 1;
                display_refresh_detect = display_capability::DisplayRefreshDetect::parse(
                    required_value(args, i, "--display-refresh-detect")?,
                )?;
            }
            "--adaptive-feedback-ms" => {
                i += 1;
                capability_feedback_ms = parse_milliseconds(
                    required_value(args, i, "--adaptive-feedback-ms")?,
                    "adaptive-feedback-ms",
                    500,
                    2000,
                )?;
            }
            "--verbose" => {
                verbose = true;
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown screen-recv argument: {other}")),
        }
        i += 1;
    }
    if audio_mode == h264_recv_view::AudioRecvMode::On && !playout_delay_explicit {
        playout_delay_ms = 250;
    }
    let av_sync_mode = if audio_mode == h264_recv_view::AudioRecvMode::On {
        requested_av_sync_mode.unwrap_or(av_sync::AvSyncMode::Conservative)
    } else {
        av_sync::AvSyncMode::Off
    };

    Ok(Command::ScreenRecv(h264_recv_view::H264RecvViewConfig {
        bind,
        port: port.ok_or_else(|| "screen-recv requires --port <port>".to_string())?,
        decoder_fps,
        duration_sec,
        frame_timeout_ms,
        reorder_wait_ms,
        playout_delay_ms,
        max_inflight_frames,
        max_decode_queue,
        strict_decode_order,
        drop_damaged_gop,
        debug_dump_frames,
        debug_dump_limit,
        json_interval_ms,
        title,
        render_scale,
        window_mode,
        render_backend,
        repair_mode,
        nack_delay_ms,
        nack_repeat_ms,
        nack_max_rounds,
        audio_mode,
        audio_jitter_buffer_ms,
        av_sync_mode,
        display_refresh_detect,
        capability_feedback_ms,
        mode: h264_recv_view::H264RecvViewMode::Screen,
        verbose,
    }))
}

fn parse_adaptive_sender_option(
    option: &str,
    args: &[String],
    index: &mut usize,
    config: &mut h264_send_probe::AdaptiveRuntimeConfig,
) -> Result<bool, String> {
    let name = option;
    match option {
        "--adaptive-quality" => {
            *index += 1;
            config.quality.mode =
                adaptive_quality::AdaptiveMode::parse(required_value(args, *index, name)?)?;
        }
        "--display-refresh-detect" => {
            *index += 1;
            config.display_refresh_detect = display_capability::DisplayRefreshDetect::parse(
                required_value(args, *index, name)?,
            )?;
        }
        "--max-fps" => {
            *index += 1;
            config.max_fps = frame_rate_policy::MaxFps::parse(required_value(args, *index, name)?)?;
        }
        "--enable-high-refresh" => {
            *index += 1;
            config.enable_high_refresh = parse_bool(required_value(args, *index, name)?)?;
        }
        "--adaptive-min-width" => {
            *index += 1;
            config.quality.min_width =
                parse_dimension(required_value(args, *index, name)?, "adaptive-min-width")?;
        }
        "--adaptive-min-height" => {
            *index += 1;
            config.quality.min_height =
                parse_dimension(required_value(args, *index, name)?, "adaptive-min-height")?;
        }
        "--adaptive-min-fps" => {
            *index += 1;
            config.quality.min_fps = parse_fps(required_value(args, *index, name)?)?;
        }
        "--adaptive-max-bitrate-mbps" => {
            *index += 1;
            config.quality.max_bitrate_mbps =
                Some(parse_bitrate(required_value(args, *index, name)?)?);
        }
        "--adaptive-feedback-ms" => {
            *index += 1;
            config.feedback_ms = parse_milliseconds(
                required_value(args, *index, name)?,
                "adaptive-feedback-ms",
                500,
                2000,
            )?;
        }
        "--adaptive-upgrade-stable-sec" => {
            *index += 1;
            config.quality.upgrade_stable_sec = parse_seconds_range(
                required_value(args, *index, name)?,
                "adaptive-upgrade-stable-sec",
                1,
                3600,
            )?;
        }
        "--adaptive-resolution-cooldown-sec" => {
            *index += 1;
            config.quality.resolution_cooldown_sec = parse_seconds_range(
                required_value(args, *index, name)?,
                "adaptive-resolution-cooldown-sec",
                1,
                3600,
            )?;
        }
        "--adaptive-fps-cooldown-sec" => {
            *index += 1;
            config.quality.fps_cooldown_sec = parse_seconds_range(
                required_value(args, *index, name)?,
                "adaptive-fps-cooldown-sec",
                1,
                3600,
            )?;
        }
        "--interactive-lag-guard" => {
            *index += 1;
            config.quality.interactive_lag_guard = parse_bool(required_value(args, *index, name)?)?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn parse_seconds_range(text: &str, name: &str, minimum: u64, maximum: u64) -> Result<u64, String> {
    let value = text
        .parse::<u64>()
        .map_err(|_| format!("{name} must be an integer"))?;
    if !(minimum..=maximum).contains(&value) {
        return Err(format!("{name} must be between {minimum} and {maximum}"));
    }
    Ok(value)
}

fn required_value<'a>(args: &'a [String], index: usize, name: &str) -> Result<&'a str, String> {
    args.get(index)
        .map(String::as_str)
        .ok_or_else(|| format!("missing value for {name}"))
}

fn parse_port(text: &str) -> Result<u16, String> {
    let port: u16 = text.parse().map_err(|_| format!("invalid port: {text}"))?;
    if port == 0 {
        Err("port must be greater than zero".to_string())
    } else {
        Ok(port)
    }
}

fn parse_bool(text: &str) -> Result<bool, String> {
    match text.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(format!("invalid boolean value: {text}")),
    }
}

fn parse_fps(text: &str) -> Result<u32, String> {
    let fps: u32 = text.parse().map_err(|_| format!("invalid fps: {text}"))?;
    if (1..=240).contains(&fps) {
        Ok(fps)
    } else {
        Err("fps must be between 1 and 240".to_string())
    }
}

fn parse_bitrate(text: &str) -> Result<f64, String> {
    let bitrate: f64 = text
        .parse()
        .map_err(|_| format!("invalid bitrate-mbps: {text}"))?;
    if bitrate.is_finite() && bitrate > 0.0 && bitrate <= 1000.0 {
        Ok(bitrate)
    } else {
        Err("bitrate-mbps must be > 0 and <= 1000".to_string())
    }
}

fn parse_quality_bpf(text: &str) -> Result<f64, String> {
    let value: f64 = text
        .parse()
        .map_err(|_| format!("invalid quality-bpf: {text}"))?;
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err("quality-bpf must be a finite value greater than zero".to_string())
    }
}

fn parse_packet_pacing(text: &str) -> Result<h264_send_probe::PacketPacing, String> {
    match text.to_ascii_lowercase().as_str() {
        "auto" => Ok(h264_send_probe::PacketPacing::Auto),
        "batch" => Ok(h264_send_probe::PacketPacing::Batch),
        "off" => Ok(h264_send_probe::PacketPacing::Off),
        _ => Err(format!(
            "invalid packet-pacing: {text}; expected auto, batch, or off"
        )),
    }
}

fn parse_fec_mode(text: &str) -> Result<fec::FecMode, String> {
    match text.to_ascii_lowercase().as_str() {
        "off" => Ok(fec::FecMode::Off),
        "single-xor" => Ok(fec::FecMode::SingleXor),
        _ => Err(format!(
            "invalid fec mode: {text}; expected off or single-xor"
        )),
    }
}

fn parse_keyframe_interval_sec(text: &str) -> Result<f64, String> {
    let seconds: f64 = text
        .parse()
        .map_err(|_| format!("invalid keyframe-interval-sec: {text}"))?;
    if seconds.is_finite() && (0.5..=60.0).contains(&seconds) {
        Ok(seconds)
    } else {
        Err("keyframe-interval-sec must be between 0.5 and 60".to_string())
    }
}

fn parse_duration_sec(text: &str) -> Result<u64, String> {
    let duration: u64 = text
        .parse()
        .map_err(|_| format!("invalid duration-sec: {text}"))?;
    if (1..=3600).contains(&duration) {
        Ok(duration)
    } else {
        Err("duration-sec must be between 1 and 3600".to_string())
    }
}

fn parse_audio_frame_ms(text: &str) -> Result<u32, String> {
    let value: u32 = text
        .parse()
        .map_err(|_| format!("invalid frame-ms: {text}"))?;
    if value == 10 || value == 20 {
        Ok(value)
    } else {
        Err("frame-ms must be 10 or 20".to_string())
    }
}

fn parse_audio_jitter_buffer_ms(text: &str) -> Result<u32, String> {
    let value: u32 = text
        .parse()
        .map_err(|_| format!("invalid jitter-buffer-ms: {text}"))?;
    if value <= 500 {
        Ok(value)
    } else {
        Err("jitter-buffer-ms must be between 0 and 500".to_string())
    }
}

fn parse_idle_timeout(text: &str) -> Result<u64, String> {
    let duration: u64 = text
        .parse()
        .map_err(|_| format!("invalid idle-timeout-sec: {text}"))?;
    if duration <= 3600 {
        Ok(duration)
    } else {
        Err("idle-timeout-sec must be between 0 and 3600".to_string())
    }
}

fn parse_milliseconds(text: &str, name: &str, minimum: u64, maximum: u64) -> Result<u64, String> {
    let value: u64 = text
        .parse()
        .map_err(|_| format!("invalid {name}: {text}"))?;
    if (minimum..=maximum).contains(&value) {
        Ok(value)
    } else {
        Err(format!("{name} must be between {minimum} and {maximum}"))
    }
}

fn parse_reorder_wait_ms(text: &str) -> Result<Option<u64>, String> {
    if text.eq_ignore_ascii_case("auto") {
        return Ok(None);
    }
    parse_milliseconds(text, "reorder-wait-ms", 1, 1000).map(Some)
}

fn parse_count(text: &str, name: &str, minimum: usize, maximum: usize) -> Result<usize, String> {
    let value: usize = text
        .parse()
        .map_err(|_| format!("invalid {name}: {text}"))?;
    if (minimum..=maximum).contains(&value) {
        Ok(value)
    } else {
        Err(format!("{name} must be between {minimum} and {maximum}"))
    }
}

fn parse_dimension(text: &str, name: &str) -> Result<u32, String> {
    let value: u32 = text
        .parse()
        .map_err(|_| format!("invalid {name}: {text}"))?;
    if value < 16 || value > 8192 {
        return Err(format!("{name} must be between 16 and 8192"));
    }
    if value % 2 != 0 {
        return Err(format!("{name} must be even for NV12"));
    }
    Ok(value)
}

fn parse_udp_payload_size(text: &str) -> Result<usize, String> {
    let value: usize = text
        .parse()
        .map_err(|_| format!("invalid udp-payload-size: {text}"))?;
    validate_udp_payload_size(value)?;
    Ok(value)
}

pub(crate) fn validate_udp_payload_size(value: usize) -> Result<(), String> {
    if (MIN_UDP_PAYLOAD_SIZE..=MAX_UDP_PAYLOAD_SIZE).contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "udp-payload-size must be between {MIN_UDP_PAYLOAD_SIZE} and {MAX_UDP_PAYLOAD_SIZE}"
        ))
    }
}

fn parse_encoder_choice(text: &str) -> Result<wmf_h264_encoder::EncoderChoice, String> {
    match text {
        "auto" => Ok(wmf_h264_encoder::EncoderChoice::Auto),
        "software" => Ok(wmf_h264_encoder::EncoderChoice::Software),
        "microsoft" => Ok(wmf_h264_encoder::EncoderChoice::Microsoft),
        "hardware" => Ok(wmf_h264_encoder::EncoderChoice::Hardware),
        "intel-qsv" => Ok(wmf_h264_encoder::EncoderChoice::IntelQsv),
        _ => Err("encoder must be auto, hardware, software, microsoft, or intel-qsv".to_string()),
    }
}

fn parse_convert_backend(text: &str) -> Result<capture_encode_probe::ConvertBackend, String> {
    match text {
        "auto" => Ok(capture_encode_probe::ConvertBackend::Auto),
        "cpu" => Ok(capture_encode_probe::ConvertBackend::Cpu),
        "d3d11" => Ok(capture_encode_probe::ConvertBackend::D3d11),
        _ => Err("convert-backend must be auto, cpu, or d3d11".to_string()),
    }
}

fn print_help() {
    println!(
        "AgoraLink Native Media prototype\n\n\
Usage:\n\
  agoralink_media self-test\n\
  agoralink_media bench-reassembly [--frames <n>] [--packets-per-frame <n>] [--payload-size <bytes>] [--loss-rate <0-1>]\n\
  agoralink_media sender --host <ip> --port <port> --fps <fps> --bitrate-mbps <mbps>\n\
  agoralink_media receiver --bind <ip> --port <port>\n\
  agoralink_media capture-probe --duration-sec <seconds> --target-fps <fps>\n\
  agoralink_media wmf-probe\n\
  agoralink_media audio-capture-probe [--duration-sec <seconds>] [--output <path>]\n\
  agoralink_media audio-send --host <ip> --port <port> [--duration-sec <seconds>] [--frame-ms 10|20]\n\
  agoralink_media audio-recv-play --bind <ip> --port <port> [--duration-sec <seconds>] [--jitter-buffer-ms <0-500>]\n\
  agoralink_media gpu-convert-probe --duration-sec <seconds> --target-fps <fps> --out-width <pixels> --out-height <pixels> [--debug-dump-nv12 <dir>] [--debug-dump-bgra <dir>] [--debug-dump-limit <n>]\n\
  agoralink_media encode-probe --width <pixels> --height <pixels> --fps <fps> --duration-sec <seconds> --bitrate-mbps <mbps> --output <path> [--encoder auto|hardware|software|microsoft|intel-qsv] [--color-matrix bt601|bt709]\n\
  agoralink_media capture-encode-probe --duration-sec <seconds> --target-fps <fps> [--bitrate-mbps <mbps>] [--quality-bpf <float>] --out-width <pixels> --out-height <pixels> --output <path> [--encoder auto|hardware|software|microsoft|intel-qsv] [--convert-backend auto|cpu|d3d11] [--color-matrix bt601|bt709]\n\
  agoralink_media h264-send-probe --host <ip> --port <port> --duration-sec <seconds> --target-fps <fps> [--bitrate-mbps <mbps>] [--quality-bpf <float>] --out-width <pixels> --out-height <pixels> [--encoder auto|hardware|software|microsoft|intel-qsv] [--convert-backend auto|cpu|d3d11] [--packet-pacing auto|off|batch] [--fec off|single-xor] [--repair off|nack] [--repair-cache-ms <500-10000>] [--udp-payload-size <576-1472>] [--keyframe-interval-sec <seconds>] [--adaptive-quality off|smoothness] [--display-refresh-detect auto|off] [--max-fps auto|60|75|90|120] [--enable-high-refresh true|false] [--adaptive-min-width <pixels>] [--adaptive-min-height <pixels>] [--adaptive-min-fps <fps>] [--adaptive-max-bitrate-mbps <mbps>] [--adaptive-feedback-ms <500-2000>] [--adaptive-upgrade-stable-sec <seconds>] [--adaptive-resolution-cooldown-sec <seconds>] [--adaptive-fps-cooldown-sec <seconds>] [--interactive-lag-guard true|false] [--color-matrix bt601|bt709]\n\
  agoralink_media h264-recv-dump --bind <ip> --port <port> --output <path> [--idle-timeout-sec <seconds>] [--drop-damaged-gop <true|false>] [--reorder-wait-ms auto|<ms>]\n\
  agoralink_media h264-recv-view --bind <ip> --port <port> [--fps <fps>] [--frame-timeout-ms <ms>] [--reorder-wait-ms auto|<ms>] [--playout-delay-ms <0-500>] [--repair off|nack] [--nack-delay-ms <1-50>] [--nack-repeat-ms <1-50>] [--nack-max-rounds <1-10>] [--render-scale exact|fit|stretch] [--window-mode windowed|borderless-fullscreen] [--render-backend gdi|d3d11] [--display-refresh-detect auto|off] [--adaptive-feedback-ms <500-2000>] [--max-inflight-frames <n>] [--max-decode-queue <n>] [--strict-decode-order <true|false>] [--drop-damaged-gop <true|false>] [--debug-dump-frames <dir>] [--debug-dump-limit <n>] [--json-interval-ms <ms>] [--title <text>]\n\
  agoralink_media screen-send --host <ip> --port <port> [--width <pixels>] [--height <pixels>] [--fps <fps>] [--bitrate-mbps <mbps>] [--quality-bpf <float>] [--duration-sec <seconds>] [--encoder auto|hardware|software|microsoft|intel-qsv] [--convert-backend auto|cpu|d3d11] [--packet-pacing auto|off|batch] [--fec off|single-xor] [--repair off|nack] [--repair-cache-ms <500-10000>] [--udp-payload-size <576-1472>] [--keyframe-interval-sec <seconds>] [--adaptive-quality off|smoothness] [--display-refresh-detect auto|off] [--max-fps auto|60|75|90|120] [--enable-high-refresh true|false] [--adaptive-min-width <pixels>] [--adaptive-min-height <pixels>] [--adaptive-min-fps <fps>] [--adaptive-max-bitrate-mbps <mbps>] [--adaptive-feedback-ms <500-2000>] [--adaptive-upgrade-stable-sec <seconds>] [--adaptive-resolution-cooldown-sec <seconds>] [--adaptive-fps-cooldown-sec <seconds>] [--interactive-lag-guard true|false] [--audio off|system] [--color-matrix bt601|bt709] [--verbose]\n\
  agoralink_media screen-recv --bind <ip> --port <port> [--fps <fps>] [--duration-sec <seconds>] [--frame-timeout-ms <ms>] [--reorder-wait-ms auto|<ms>] [--playout-delay-ms <0-500>] [--repair off|nack] [--nack-delay-ms <1-50>] [--nack-repeat-ms <1-50>] [--nack-max-rounds <1-10>] [--render-scale exact|fit|stretch] [--window-mode windowed|borderless-fullscreen] [--render-backend gdi|d3d11] [--display-refresh-detect auto|off] [--adaptive-feedback-ms <500-2000>] [--max-decode-queue <n>] [--strict-decode-order <true|false>] [--drop-damaged-gop <true|false>] [--audio off|on] [--av-sync off|conservative] [--audio-jitter-buffer-ms <0-500>] [--json-interval-ms <ms>] [--title <text>] [--verbose]\n\
  agoralink_media h264-file-viewer --input <path> [--render-scale exact|fit|stretch] [--window-mode windowed|borderless-fullscreen] [--render-backend gdi|d3d11]\n\n\
  agoralink_media color-test-pattern --output <path> --width <pixels> --height <pixels> --duration-sec <seconds> [--fps <fps>] [--bitrate-mbps <mbps>] [--color-matrix bt601|bt709]\n\n\
Defaults:\n\
  bench-reassembly: --frames 1800 --packets-per-frame 120 --payload-size 1414 --loss-rate 0\n\
  sender: --host 127.0.0.1 --port 50120 --fps 30 --bitrate-mbps 4\n\
  receiver: --bind 0.0.0.0 --port 50120\n\
  capture-probe: --duration-sec 10 --target-fps 30\n\
  audio-capture-probe: --duration-sec 10 --output audio_probe.pcm\n\
  audio-send: --host required --port required --frame-ms 10 --duration-sec unlimited\n\
  audio-recv-play: --bind 0.0.0.0 --port required --jitter-buffer-ms 120 --duration-sec unlimited\n\
  gpu-convert-probe: --duration-sec 5 --target-fps 30 --out-width 1280 --out-height 720 --color-matrix bt709 --debug-dump-limit 3\n\
  encode-probe: --width 1280 --height 720 --fps 30 --duration-sec 5 --bitrate-mbps 4 --output synthetic_720p30.h264 --encoder auto --color-matrix bt709\n\
  capture-encode-probe: --duration-sec 5 --target-fps 30 --bitrate-mbps 4 --out-width 1280 --out-height 720 --output capture_720p30.h264 --encoder auto --convert-backend auto --color-matrix bt709\n\
  h264-send-probe: --host 127.0.0.1 --port 50130 --duration-sec 10 --target-fps 30 --bitrate-mbps 4 --out-width 1280 --out-height 720 --encoder auto --convert-backend auto --packet-pacing auto --fec off --udp-payload-size 1452 --keyframe-interval-sec 1 --color-matrix bt709\n\
  h264-recv-dump: --bind 0.0.0.0 --port 50130 --output received_capture.h264 --idle-timeout-sec 3 --drop-damaged-gop true --reorder-wait-ms auto\n\
  h264-recv-view: --bind 0.0.0.0 --port required --fps 30 --frame-timeout-ms 300 --reorder-wait-ms auto --playout-delay-ms 120 --render-scale exact --window-mode windowed --render-backend d3d11 --max-inflight-frames 120 --max-decode-queue 30 --strict-decode-order true --drop-damaged-gop true --debug-dump-limit 10 --json-interval-ms 1000 --title \"AgoraLink Native Viewer\"\n\
  screen-send: --width 1920 --height 1080 --fps 60 --bitrate-mbps 22 --repair nack --adaptive-quality off --encoder auto --convert-backend auto --display-refresh-detect auto --max-fps 60 --enable-high-refresh false --adaptive-min-width 1280 --adaptive-min-height 720 --adaptive-min-fps 30 --adaptive-feedback-ms 1000 --adaptive-upgrade-stable-sec 15 --adaptive-resolution-cooldown-sec 30 --adaptive-fps-cooldown-sec 20 --interactive-lag-guard true\n\
  screen-send bitrate precedence: explicit --bitrate-mbps, then explicit --quality-bpf, then 22 Mbps\n\
  adaptive smoothness ladder: Q0 1080p60 -> Q1 900p60 -> Q2 720p60 -> Q3 min(initial,18 Mbps) -> Q4 min(initial,15 Mbps); emergency FPS only after Q4: E1 45 FPS -> E2 30 FPS\n\
  encoder auto prefers Intel QSV and keeps the existing software fallback; convert-backend auto prefers D3D11 and keeps the existing CPU fallback\n\
  screen-recv: --bind 0.0.0.0 --port required --fps 30 --frame-timeout-ms 300 --reorder-wait-ms auto --playout-delay-ms 120, or 250 when --audio on unless explicitly set --audio off --av-sync off when audio is off, otherwise conservative --audio-jitter-buffer-ms 120 --render-scale exact --window-mode windowed --render-backend d3d11 --max-inflight-frames 120 --max-decode-queue 30 --strict-decode-order true --drop-damaged-gop true --json-interval-ms 1000 --title \"AgoraLink Native Viewer\" --duration-sec unlimited --verbose off\n\
  h264-file-viewer: --input received_capture_lan.h264 --render-scale exact --window-mode windowed --render-backend gdi\n\
  color-test-pattern: --output color_test_1080p.h264 --width 1920 --height 1080 --fps 30 --duration-sec 3 --bitrate-mbps 8 --color-matrix bt709"
    );
}

fn run_sender(host: &str, port: u16, fps: u32, bitrate_mbps: f64) -> io::Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let target = format!("{host}:{port}");
    socket.connect(&target)?;

    eprintln!(
        "agoralink_media sender target={} fps={} bitrate_mbps={}",
        target, fps, bitrate_mbps
    );

    let session_id = make_session_id();
    let frame_interval = Duration::from_nanos(1_000_000_000u64 / u64::from(fps));
    let bytes_per_frame = estimate_frame_payload_bytes(fps, bitrate_mbps);
    let mut frame_id = 0u64;
    let mut packets_sent = 0u64;
    let mut frames_sent = 0u64;
    let mut window_packets = 0u64;
    let mut window_frames = 0u64;
    let mut window_bytes = 0u64;
    let mut next_frame_at = Instant::now();
    let mut stats_at = Instant::now();

    loop {
        let now = Instant::now();
        if now < next_frame_at {
            thread::sleep(next_frame_at.duration_since(now));
        }

        let keyframe = frame_id % u64::from(fps) == 0;
        let timestamp_ms = now_millis();
        let packets = build_frame_packets(
            session_id,
            frame_id,
            timestamp_ms,
            bytes_per_frame,
            keyframe,
        )
        .map_err(io::Error::other)?;

        for packet in packets {
            let sent = socket.send(&packet)?;
            packets_sent += 1;
            window_packets += 1;
            window_bytes += sent as u64;
        }

        frames_sent += 1;
        window_frames += 1;
        frame_id += 1;

        let elapsed = stats_at.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let elapsed_sec = elapsed.as_secs_f64().max(0.001);
            let mbps = (window_bytes as f64 * 8.0) / elapsed_sec / 1_000_000.0;
            let measured_fps = window_frames as f64 / elapsed_sec;
            println!(
                r#"{{"type":"MEDIA_STATS","mode":"sender","packets_sent":{},"frames_sent":{},"mbps":{:.3},"fps":{:.2},"target_bitrate_mbps":{:.3}}}"#,
                packets_sent, frames_sent, mbps, measured_fps, bitrate_mbps
            );
            io::stdout().flush().ok();
            window_packets = 0;
            window_frames = 0;
            window_bytes = 0;
            stats_at = Instant::now();
        }

        let after_send = Instant::now();
        next_frame_at += frame_interval;
        if next_frame_at < after_send {
            next_frame_at = after_send + frame_interval;
        }

        let _ = window_packets;
    }
}

fn run_receiver(bind: &str, port: u16) -> io::Result<()> {
    let socket = UdpSocket::bind(format!("{bind}:{port}"))?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    eprintln!("agoralink_media receiver bind={bind}:{port}");

    let mut buf = [0u8; 2048];
    let mut reassembler = Reassembler::new(FRAME_TTL);
    let mut stats_at = Instant::now();
    let mut window_bytes = 0u64;
    let mut window_frames = 0u64;

    loop {
        match socket.recv_from(&mut buf) {
            Ok((len, _addr)) => match MediaPacket::decode(&buf[..len]) {
                Ok(packet) => {
                    let now = Instant::now();
                    window_bytes += len as u64;
                    if reassembler.accept(packet, now) {
                        window_frames += 1;
                    }
                }
                Err(_err) => {
                    reassembler.stats.packets_dropped_or_lost_estimate += 1;
                }
            },
            Err(err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut => {}
            Err(err) => return Err(err),
        }

        let now = Instant::now();
        reassembler.expire(now);

        let elapsed = stats_at.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let elapsed_sec = elapsed.as_secs_f64().max(0.001);
            let mbps = (window_bytes as f64 * 8.0) / elapsed_sec / 1_000_000.0;
            let fps = window_frames as f64 / elapsed_sec;
            let stats = &reassembler.stats;
            println!(
                r#"{{"type":"MEDIA_STATS","mode":"receiver","packets_received":{},"packets_dropped_or_lost_estimate":{},"frames_complete":{},"frames_incomplete_expired":{},"mbps":{:.3},"fps":{:.2},"last_frame_id":{},"inflight_frames":{}}}"#,
                stats.packets_received,
                stats.packets_dropped_or_lost_estimate,
                stats.frames_complete,
                stats.frames_incomplete_expired,
                mbps,
                fps,
                stats.last_frame_id,
                reassembler.frames.len()
            );
            io::stdout().flush().ok();
            stats_at = Instant::now();
            window_bytes = 0;
            window_frames = 0;
        }
    }
}

fn build_frame_packets(
    session_id: u64,
    frame_id: u64,
    timestamp_ms: u64,
    payload_size: usize,
    keyframe: bool,
) -> Result<Vec<Vec<u8>>, String> {
    if payload_size > MAX_VIDEO_FRAME_BYTES {
        return Err(format!(
            "frame exceeds encoded byte limit: {payload_size} > {MAX_VIDEO_FRAME_BYTES}"
        ));
    }
    let packet_count = payload_size.div_ceil(LEGACY_MEDIA_PAYLOAD).max(1);
    if packet_count > MAX_VIDEO_PACKET_COUNT {
        return Err(format!("frame too large: {} packets", packet_count));
    }

    let mut packets = Vec::with_capacity(packet_count);
    let mut remaining = payload_size;
    for packet_index in 0..packet_count {
        let chunk_len = remaining.min(LEGACY_MEDIA_PAYLOAD);
        remaining = remaining.saturating_sub(chunk_len);
        let mut flags = 0u16;
        if keyframe {
            flags |= FLAG_KEYFRAME;
        }
        if packet_index + 1 == packet_count {
            flags |= FLAG_END_OF_FRAME;
        }
        let fill = (frame_id as u8).wrapping_add(packet_index as u8);
        let packet = MediaPacket {
            stream_id: STREAM_VIDEO,
            flags,
            session_id,
            frame_id,
            packet_index: packet_index as u16,
            packet_count: packet_count as u16,
            timestamp_ms,
            payload: vec![fill; chunk_len],
        };
        packets.push(packet.encode()?);
    }

    Ok(packets)
}

pub(crate) fn packetize_media_payload(
    session_id: u64,
    frame_id: u64,
    timestamp_ms: u64,
    payload: &[u8],
    frame_flags: u16,
) -> Result<Vec<Vec<u8>>, String> {
    if payload.len() > MAX_VIDEO_FRAME_BYTES {
        return Err(format!(
            "encoded frame exceeds byte limit: {} > {MAX_VIDEO_FRAME_BYTES}",
            payload.len()
        ));
    }
    let packet_count = payload.len().div_ceil(LEGACY_MEDIA_PAYLOAD).max(1);
    if packet_count > MAX_VIDEO_PACKET_COUNT {
        return Err(format!("encoded frame too large: {packet_count} packets"));
    }

    let mut packets = Vec::with_capacity(packet_count);
    for packet_index in 0..packet_count {
        let start = packet_index * LEGACY_MEDIA_PAYLOAD;
        let end = (start + LEGACY_MEDIA_PAYLOAD).min(payload.len());
        let mut flags = if packet_index == 0 { frame_flags } else { 0 };
        if packet_index + 1 == packet_count {
            flags |= FLAG_END_OF_FRAME;
        }
        packets.push(
            MediaPacket {
                stream_id: STREAM_VIDEO,
                flags,
                session_id,
                frame_id,
                packet_index: packet_index as u16,
                packet_count: packet_count as u16,
                timestamp_ms,
                payload: payload[start..end].to_vec(),
            }
            .encode()?,
        );
    }
    Ok(packets)
}

fn estimate_frame_payload_bytes(fps: u32, bitrate_mbps: f64) -> usize {
    let bytes_per_second = bitrate_mbps * 1_000_000.0 / 8.0;
    (bytes_per_second / fps as f64).round().max(1.0) as usize
}

pub(crate) fn make_session_id() -> u64 {
    now_millis() ^ ((process::id() as u64) << 32)
}

pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn run_self_test() -> Result<(), String> {
    if parse_packet_pacing("batch")? != h264_send_probe::PacketPacing::Batch
        || parse_packet_pacing("auto")?.effective_name() != "batch"
    {
        return Err("batch packet pacing parsing failed".to_string());
    }
    for value in [1200, 1452, 1472] {
        if parse_udp_payload_size(&value.to_string())? != value {
            return Err(format!("UDP payload size parsing failed for {value}"));
        }
    }
    if parse_udp_payload_size("500").is_ok() || parse_udp_payload_size("1600").is_ok() {
        return Err("invalid UDP payload size was accepted".to_string());
    }
    match parse_args(vec![
        "audio-capture-probe".to_string(),
        "--duration-sec".to_string(),
        "2".to_string(),
        "--output".to_string(),
        "test_audio.pcm".to_string(),
    ])? {
        Command::AudioCaptureProbe(config)
            if config.duration_sec == 2 && config.output == PathBuf::from("test_audio.pcm") => {}
        _ => return Err("audio-capture-probe parsing failed".to_string()),
    }
    match parse_args(vec![
        "audio-send".to_string(),
        "--host".to_string(),
        "127.0.0.1".to_string(),
        "--port".to_string(),
        "55200".to_string(),
        "--frame-ms".to_string(),
        "20".to_string(),
    ])? {
        Command::AudioSend(config)
            if config.host == "127.0.0.1" && config.port == 55200 && config.frame_ms == 20 => {}
        _ => return Err("audio-send parsing failed".to_string()),
    }
    match parse_args(vec![
        "audio-recv-play".to_string(),
        "--port".to_string(),
        "55200".to_string(),
        "--jitter-buffer-ms".to_string(),
        "80".to_string(),
    ])? {
        Command::AudioRecvPlay(config)
            if config.bind == "0.0.0.0"
                && config.port == 55200
                && config.jitter_buffer_ms == 80 => {}
        _ => return Err("audio-recv-play parsing failed".to_string()),
    }
    if parse_audio_frame_ms("5").is_ok() || parse_audio_jitter_buffer_ms("501").is_ok() {
        return Err("invalid audio UDP argument was accepted".to_string());
    }
    for value in ["0", "120", "500"] {
        parse_milliseconds(value, "playout-delay-ms", 0, 500)?;
    }
    if parse_milliseconds("-1", "playout-delay-ms", 0, 500).is_ok()
        || parse_milliseconds("501", "playout-delay-ms", 0, 500).is_ok()
    {
        return Err("invalid playout delay was accepted".to_string());
    }
    if parse_fec_mode("off")? != fec::FecMode::Off
        || parse_fec_mode("single-xor")? != fec::FecMode::SingleXor
        || parse_fec_mode("invalid").is_ok()
    {
        return Err("FEC mode parsing failed".to_string());
    }
    if (parse_keyframe_interval_sec("0.5")? - 0.5).abs() > f64::EPSILON {
        return Err("fractional keyframe interval parsing failed".to_string());
    }
    if parse_reorder_wait_ms("auto")?.is_some() || parse_reorder_wait_ms("42")? != Some(42) {
        return Err("reorder-wait-ms parsing failed".to_string());
    }
    match parse_args(vec![
        "h264-recv-dump".to_string(),
        "--drop-damaged-gop".to_string(),
        "false".to_string(),
    ])? {
        Command::H264RecvDump(config) if !config.drop_damaged_gop => {}
        _ => return Err("drop-damaged-gop parsing failed".to_string()),
    }
    match parse_args(vec![
        "screen-send".to_string(),
        "--host".to_string(),
        "127.0.0.1".to_string(),
        "--port".to_string(),
        "55134".to_string(),
    ])? {
        Command::ScreenSend(config)
            if config.udp_payload_size == DEFAULT_REALTIME_UDP_PAYLOAD_SIZE
                && config.audio_mode == h264_send_probe::AudioSendMode::Off
                && config.adaptive.quality.mode == adaptive_quality::AdaptiveMode::Off
                && config.adaptive.max_fps == frame_rate_policy::MaxFps::Fixed(60)
                && !config.adaptive.enable_high_refresh
                && config.adaptive.feedback_ms == 1000 => {}
        _ => return Err("screen-send UDP payload default mismatch".to_string()),
    }
    match parse_args(vec![
        "screen-send".to_string(),
        "--host".to_string(),
        "127.0.0.1".to_string(),
        "--port".to_string(),
        "55134".to_string(),
        "--adaptive-quality".to_string(),
        "smoothness".to_string(),
        "--max-fps".to_string(),
        "120".to_string(),
        "--enable-high-refresh".to_string(),
        "true".to_string(),
        "--adaptive-feedback-ms".to_string(),
        "750".to_string(),
    ])? {
        Command::ScreenSend(config)
            if config.adaptive.quality.mode == adaptive_quality::AdaptiveMode::Smoothness
                && config.adaptive.max_fps == frame_rate_policy::MaxFps::Fixed(120)
                && config.adaptive.enable_high_refresh
                && config.adaptive.feedback_ms == 750 => {}
        _ => return Err("screen-send adaptive option parsing failed".to_string()),
    }
    match parse_args(vec![
        "screen-send".to_string(),
        "--host".to_string(),
        "127.0.0.1".to_string(),
        "--port".to_string(),
        "55134".to_string(),
        "--audio".to_string(),
        "system".to_string(),
    ])? {
        Command::ScreenSend(config)
            if config.audio_mode == h264_send_probe::AudioSendMode::System => {}
        _ => return Err("screen-send audio parsing failed".to_string()),
    }
    match parse_args(vec![
        "screen-recv".to_string(),
        "--port".to_string(),
        "55134".to_string(),
        "--fps".to_string(),
        "60".to_string(),
    ])? {
        Command::ScreenRecv(config)
            if config.playout_delay_ms == 120
                && config.decoder_fps == 60
                && config.render_scale == win32_gdi_viewer::RenderScaleMode::Exact
                && config.window_mode == win32_gdi_viewer::WindowMode::Windowed
                && config.render_backend == video_renderer::RenderBackend::D3d11
                && config.audio_mode == h264_recv_view::AudioRecvMode::Off
                && config.display_refresh_detect
                    == display_capability::DisplayRefreshDetect::Auto
                && config.capability_feedback_ms == 1000 => {}
        _ => return Err("screen-recv playout delay default mismatch".to_string()),
    }
    match parse_args(vec![
        "screen-recv".to_string(),
        "--port".to_string(),
        "55134".to_string(),
        "--audio".to_string(),
        "on".to_string(),
    ])? {
        Command::ScreenRecv(config)
            if config.audio_mode == h264_recv_view::AudioRecvMode::On
                && config.playout_delay_ms == 250
                && config.audio_jitter_buffer_ms == 120
                && config.av_sync_mode == av_sync::AvSyncMode::Conservative => {}
        _ => return Err("screen-recv audio parsing failed".to_string()),
    }
    match parse_args(vec![
        "screen-recv".to_string(),
        "--port".to_string(),
        "55134".to_string(),
        "--audio".to_string(),
        "on".to_string(),
        "--av-sync".to_string(),
        "off".to_string(),
    ])? {
        Command::ScreenRecv(config) if config.av_sync_mode == av_sync::AvSyncMode::Off => {}
        _ => return Err("screen-recv av-sync off parsing failed".to_string()),
    }
    match parse_args(vec![
        "screen-recv".to_string(),
        "--port".to_string(),
        "55134".to_string(),
        "--audio".to_string(),
        "off".to_string(),
        "--av-sync".to_string(),
        "conservative".to_string(),
    ])? {
        Command::ScreenRecv(config) if config.av_sync_mode == av_sync::AvSyncMode::Off => {}
        _ => return Err("audio-off did not force AV sync off".to_string()),
    }
    match parse_args(vec![
        "h264-recv-view".to_string(),
        "--port".to_string(),
        "55135".to_string(),
        "--fps".to_string(),
        "60".to_string(),
    ])? {
        Command::H264RecvView(config)
            if config.playout_delay_ms == 120
                && config.decoder_fps == 60
                && config.render_scale == win32_gdi_viewer::RenderScaleMode::Exact
                && config.window_mode == win32_gdi_viewer::WindowMode::Windowed
                && config.render_backend == video_renderer::RenderBackend::D3d11 => {}
        _ => return Err("h264-recv-view playout delay default mismatch".to_string()),
    }
    match parse_args(vec![
        "h264-file-viewer".to_string(),
        "--input".to_string(),
        "test.h264".to_string(),
    ])? {
        Command::H264FileViewer(config)
            if config.render_scale == win32_gdi_viewer::RenderScaleMode::Exact
                && config.window_mode == win32_gdi_viewer::WindowMode::Windowed
                && config.render_backend == video_renderer::RenderBackend::Gdi => {}
        _ => return Err("h264-file-viewer render defaults mismatch".to_string()),
    }
    match parse_args(vec![
        "h264-file-viewer".to_string(),
        "--input".to_string(),
        "test.h264".to_string(),
        "--window-mode".to_string(),
        "borderless-fullscreen".to_string(),
    ])? {
        Command::H264FileViewer(config)
            if config.render_scale == win32_gdi_viewer::RenderScaleMode::Exact
                && config.window_mode == win32_gdi_viewer::WindowMode::BorderlessFullscreen => {}
        _ => return Err("h264-file-viewer window mode parsing mismatch".to_string()),
    }
    bgra_to_nv12::run_self_test()?;
    audio_udp::run_self_test()?;
    adaptive_quality::run_self_test()?;
    bench_reassembly::run_self_test()?;
    bitrate::run_self_test()?;
    color_spec::run_self_test()?;
    d3d11_nv12_renderer::run_self_test()?;
    display_capability::run_self_test()?;
    frame_rate_policy::run_self_test()?;
    h264_annex_b::run_self_test()?;
    h264_recv_dump::run_self_test()?;
    h264_recv_view::run_self_test()?;
    h264_send_probe::run_self_test()?;
    media_clock::run_self_test()?;
    media_control::run_self_test()?;
    nv12_to_bgra::run_self_test()?;
    playout_buffer::run_self_test()?;
    repair::run_self_test()?;
    sender_scheduling::run_self_test()?;
    win32_gdi_viewer::run_self_test()?;
    let nv12_size = nv12_synthetic::buffer_size(16, 16)?;
    if nv12_size != 16 * 16 * 3 / 2 {
        return Err("NV12 buffer size mismatch".to_string());
    }
    let mut first_nv12 = vec![0u8; nv12_size];
    let mut second_nv12 = vec![0u8; nv12_size];
    nv12_synthetic::fill_frame(&mut first_nv12, 16, 16, 0)?;
    nv12_synthetic::fill_frame(&mut second_nv12, 16, 16, 1)?;
    if first_nv12 == second_nv12 {
        return Err("synthetic NV12 frames did not change over time".to_string());
    }

    let packet = MediaPacket {
        stream_id: STREAM_VIDEO,
        flags: FLAG_KEYFRAME | FLAG_END_OF_FRAME,
        session_id: 123,
        frame_id: 456,
        packet_index: 0,
        packet_count: 1,
        timestamp_ms: 789,
        payload: vec![7; 32],
    };
    let encoded = packet.encode()?;
    if encoded.len() != HEADER_LEN + 32 {
        return Err("encoded length mismatch".to_string());
    }
    let decoded = MediaPacket::decode(&encoded)?;
    if decoded.session_id != packet.session_id
        || decoded.frame_id != packet.frame_id
        || decoded.flags != packet.flags
        || decoded.payload != packet.payload
    {
        return Err("packet roundtrip mismatch".to_string());
    }

    let frame_packets = build_frame_packets(1, 9, 1000, 5000, true)?;
    if frame_packets.len() != 5 {
        return Err(format!("expected 5 packets, got {}", frame_packets.len()));
    }
    if frame_packets
        .iter()
        .any(|item| item.len() > LEGACY_UDP_PAYLOAD_SIZE)
    {
        return Err("packet exceeded UDP payload limit".to_string());
    }

    let encoded_payload = vec![0x55; 5000];
    let encoded_packets = packetize_media_payload(
        3,
        11,
        3000,
        &encoded_payload,
        FLAG_KEYFRAME | FLAG_CONFIG | FLAG_H264_ANNEX_B,
    )?;
    if encoded_packets.len() != 5 {
        return Err(format!(
            "expected 5 encoded media packets, got {}",
            encoded_packets.len()
        ));
    }
    let first_encoded = MediaPacket::decode(&encoded_packets[0])?;
    let last_encoded = MediaPacket::decode(encoded_packets.last().unwrap())?;
    if first_encoded.flags & (FLAG_KEYFRAME | FLAG_CONFIG | FLAG_H264_ANNEX_B)
        != FLAG_KEYFRAME | FLAG_CONFIG | FLAG_H264_ANNEX_B
        || last_encoded.flags & FLAG_END_OF_FRAME == 0
    {
        return Err("encoded media packet flags mismatch".to_string());
    }

    let mut reassembler = Reassembler::new(FRAME_TTL);
    let now = Instant::now();
    for raw in frame_packets.iter().rev() {
        let packet = MediaPacket::decode(raw)?;
        reassembler.accept(packet, now);
    }
    if reassembler.stats.frames_complete != 1 {
        return Err("reassembler did not complete frame".to_string());
    }

    let mut expiring = Reassembler::new(Duration::from_millis(1));
    let incomplete = build_frame_packets(2, 10, 2000, 3000, false)?;
    let first = MediaPacket::decode(&incomplete[0])?;
    expiring.accept(first, Instant::now() - Duration::from_millis(10));
    expiring.expire(Instant::now());
    if expiring.stats.frames_incomplete_expired != 1 {
        return Err("expired incomplete frame was not counted".to_string());
    }
    if expiring.stats.packets_dropped_or_lost_estimate == 0 {
        return Err("lost packet estimate was not updated".to_string());
    }

    Ok(())
}

fn json_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod r4_cli_policy_tests {
    use super::*;

    fn screen_send_config(extra: &[&str]) -> h264_send_probe::H264SendConfig {
        let mut args = vec![
            "screen-send".to_string(),
            "--host".to_string(),
            "127.0.0.1".to_string(),
            "--port".to_string(),
            "55134".to_string(),
        ];
        args.extend(extra.iter().map(|value| (*value).to_string()));
        match parse_args(args).expect("screen-send arguments should parse") {
            Command::ScreenSend(config) => config,
            other => panic!("expected screen-send command, got {other:?}"),
        }
    }

    #[test]
    fn r4_screen_send_default_is_complete_product_tuple() {
        let config = screen_send_config(&[]);

        assert_eq!((config.out_width, config.out_height), (1920, 1080));
        assert_eq!(config.target_fps, 60);
        assert!((config.bitrate_mbps - 22.0).abs() < f64::EPSILON);
        assert_eq!(config.bitrate_selection.source.name(), "default");
        assert_eq!(
            config.adaptive.quality.mode,
            adaptive_quality::AdaptiveMode::Off
        );
        assert_eq!(config.repair_mode.name(), "nack");
        assert_eq!(config.encoder.name(), "auto");
        assert_eq!(config.convert_backend.name(), "auto");
    }

    #[test]
    fn r4_explicit_bitrate_overrides_quality_bpf() {
        let config = screen_send_config(&["--bitrate-mbps", "30", "--quality-bpf", "1.0"]);

        assert!((config.bitrate_mbps - 30.0).abs() < f64::EPSILON);
        assert_eq!(config.bitrate_selection.source.name(), "explicit-bitrate");
        assert_eq!(config.bitrate_selection.quality_bpf_requested, Some(1.0));
    }

    #[test]
    fn r4_explicit_bitrate_is_preserved_with_adaptive_quality_off() {
        let config = screen_send_config(&["--bitrate-mbps", "30", "--adaptive-quality", "off"]);

        assert!((config.bitrate_mbps - 30.0).abs() < f64::EPSILON);
        assert_eq!(
            config.adaptive.quality.mode,
            adaptive_quality::AdaptiveMode::Off
        );
    }

    #[test]
    fn r4_quality_bpf_is_used_when_explicit_bitrate_is_absent() {
        let config = screen_send_config(&["--quality-bpf", "0.5"]);
        let expected = 1920.0 * 1080.0 * 60.0 * 0.5 / 1_000_000.0;

        assert!((config.bitrate_mbps - expected).abs() < 0.001);
        assert_eq!(config.bitrate_selection.source.name(), "quality-bpf");
    }
}

#[cfg(test)]
mod media_packet_boundary_tests {
    use super::*;

    fn encoded_packet_with_count(packet_count: u16) -> Vec<u8> {
        let mut bytes = MediaPacket {
            stream_id: STREAM_VIDEO,
            flags: FLAG_END_OF_FRAME,
            session_id: 1,
            frame_id: 2,
            packet_index: 0,
            packet_count: 1,
            timestamp_ms: 3,
            payload: vec![0x41],
        }
        .encode()
        .unwrap();
        bytes[26..28].copy_from_slice(&packet_count.to_be_bytes());
        bytes
    }

    #[test]
    fn packet_count_zero_rejected() {
        assert!(MediaPacket::decode(&encoded_packet_with_count(0)).is_err());
    }

    #[test]
    fn packet_count_over_limit_rejected_before_allocation() {
        let over_limit = u16::try_from(MAX_VIDEO_PACKET_COUNT + 1).unwrap();
        assert!(MediaPacket::decode(&encoded_packet_with_count(over_limit)).is_err());
    }

    #[test]
    fn max_legal_packet_count_accepted() {
        let max_legal = u16::try_from(MAX_VIDEO_PACKET_COUNT).unwrap();
        let packet = MediaPacket::decode(&encoded_packet_with_count(max_legal)).unwrap();
        assert_eq!(packet.packet_count, max_legal);
    }
}

#[cfg(test)]
mod self_test_output_tests {
    use super::self_test_success_json;

    #[test]
    fn self_test_success_includes_version_and_capabilities() {
        let output = self_test_success_json();

        assert!(output.contains(r#""type":"SELF_TEST""#));
        assert!(output.contains(r#""ok":true"#));
        assert!(output.contains(concat!(r#""version":""#, env!("CARGO_PKG_VERSION"), r#"""#)));
        assert!(output.contains(r#""capabilities":{"#));
        assert!(output.contains(r#""hardware_runtime_probe_required":true"#));
    }
}
